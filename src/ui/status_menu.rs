//! The status item in the menu bar. Its menu shows the engine status and
//! the containers. It starts, stops, and restarts a container or a Compose
//! project when the window is hidden or minimized.
//!
//! "Show in Menu Bar" in the View menu adds and removes the item.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use gpui::*;

use super::native::{self, Dot, ShowInMenuBar, StatusItem, StatusLine};
use crate::docker::containers::{Action, State};
use crate::docker::{ContainerRow, EngineStatus};
use crate::state::container_store::{ContainerStore, ListItem, build_list};

/// What a menu item does.
#[derive(Clone, Debug, PartialEq)]
enum Command {
    Open,
    /// Opens the window and selects the container.
    Reveal(String),
    Run(String, Action),
    RunGroup(String, Action),
    OpenUrl(String),
    StartColima,
    Quit,
    /// Stops Colima, then quits.
    QuitCompletely,
}

pub enum StatusMenuEvent {
    /// The user chose "Show in Colima GUI" for this container.
    Reveal(String),
}

pub struct StatusMenu {
    store: Entity<ContainerStore>,
    window: AnyWindowHandle,
    item: Option<StatusItem>,
    /// The commands of the current menu. The tag of an item is its index.
    commands: Rc<RefCell<Vec<Command>>>,
    tx: async_channel::Sender<Command>,
    _task: Task<()>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<StatusMenuEvent> for StatusMenu {}

impl StatusMenu {
    pub fn new(store: Entity<ContainerStore>, window: AnyWindowHandle, cx: &mut Context<Self>) -> Self {
        // AppKit calls the menu target in its own event loop. A command
        // goes through the channel, and runs later in a GPUI task.
        let (tx, rx) = async_channel::unbounded::<Command>();
        let task = cx.spawn(async move |this, cx| {
            while let Ok(command) = rx.recv().await {
                if this.update(cx, |this, cx| this.run(command, cx)).is_err() {
                    break;
                }
            }
        });
        let subs = vec![
            cx.observe(&store, |this, _, cx| this.rebuild(cx)),
            cx.observe_global::<ShowInMenuBar>(|this, cx| this.sync_item(cx)),
        ];
        let mut this = StatusMenu {
            store,
            window,
            item: None,
            commands: Rc::default(),
            tx,
            _task: task,
            _subs: subs,
        };
        this.sync_item(cx);
        this
    }

    /// Adds or removes the status item, as the user chose.
    fn sync_item(&mut self, cx: &mut Context<Self>) {
        let show = cx.try_global::<ShowInMenuBar>().is_some_and(|s| s.0);
        if show == self.item.is_some() {
            return;
        }
        self.item = if show {
            let commands = self.commands.clone();
            let tx = self.tx.clone();
            StatusItem::new(move |tag| {
                if let Some(command) = commands.borrow().get(tag) {
                    let _ = tx.try_send(command.clone());
                }
            })
        } else {
            None
        };
        self.rebuild(cx);
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let Some(item) = &self.item else { return };
        let store = self.store.read(cx);
        let (lines, commands) = menu_lines(store);
        item.set_lines(&lines);
        item.set_dimmed(store.status != EngineStatus::Connected);
        *self.commands.borrow_mut() = commands;
    }

    fn run(&mut self, command: Command, cx: &mut Context<Self>) {
        match command {
            Command::Open => self.open(cx),
            Command::Reveal(id) => {
                self.open(cx);
                cx.emit(StatusMenuEvent::Reveal(id));
            }
            Command::Run(id, action) => self.store.update(cx, |s, cx| s.run_action(&id, action, cx)),
            Command::RunGroup(project, action) => {
                self.store.update(cx, |s, cx| s.run_group_action(&project, action, cx))
            }
            Command::OpenUrl(url) => cx.open_url(&url),
            Command::StartColima => self.store.update(cx, |s, cx| s.start_colima(cx)),
            Command::Quit => cx.quit(),
            Command::QuitCompletely => cx.dispatch_action(&native::QuitCompletely),
        }
    }

    /// Brings the window to the front. The app can be hidden, or the window
    /// can be minimized.
    fn open(&self, cx: &mut Context<Self>) {
        cx.activate(true);
        let _ = self.window.update(cx, |_, window, _| window.activate_window());
    }
}

/// Collects the lines of a menu, and gives each command a tag.
#[derive(Default)]
struct Builder {
    lines: Vec<StatusLine>,
    commands: Vec<Command>,
}

impl Builder {
    fn item(&mut self, label: impl Into<SharedString>, command: Option<Command>) -> &mut Self {
        let line = self.line(label, command);
        self.lines.push(line);
        self
    }

    fn disabled(&mut self, label: impl Into<SharedString>) -> &mut Self {
        self.lines.push(StatusLine::Item {
            label: label.into(),
            enabled: false,
            indent: 0,
            dot: None,
            tag: None,
            submenu: Vec::new(),
        });
        self
    }

    fn separator(&mut self) -> &mut Self {
        self.lines.push(StatusLine::Separator);
        self
    }

    /// An item that is disabled when `command` is `None`.
    fn line(&mut self, label: impl Into<SharedString>, command: Option<Command>) -> StatusLine {
        StatusLine::Item {
            label: label.into(),
            enabled: command.is_some(),
            indent: 0,
            dot: None,
            tag: command.map(|c| {
                self.commands.push(c);
                self.commands.len() - 1
            }),
            submenu: Vec::new(),
        }
    }

    /// An item that opens a submenu. The submenu shares the tags.
    fn submenu(&mut self, label: impl Into<SharedString>, indent: isize, dot: Dot, f: impl FnOnce(&mut Self)) {
        let outer = std::mem::take(&mut self.lines);
        f(self);
        let submenu = std::mem::replace(&mut self.lines, outer);
        self.lines.push(StatusLine::Item {
            label: label.into(),
            enabled: true,
            indent,
            dot: Some(dot),
            tag: None,
            submenu,
        });
    }
}

fn menu_lines(store: &ContainerStore) -> (Vec<StatusLine>, Vec<Command>) {
    let mut b = Builder::default();
    let stopped = matches!(store.status, EngineStatus::NotRunning | EngineStatus::Error(_));
    b.disabled(engine_label(store));
    if stopped && !store.starting_colima {
        b.item("Start Colima", Some(Command::StartColima));
    }
    b.separator().item(format!("Open {}", native::APP_NAME), Some(Command::Open));

    if store.status == EngineStatus::Connected {
        b.separator();
        let rows = &store.set.rows;
        if store.is_loading() {
            b.disabled("Loading…");
        } else if rows.is_empty() {
            b.disabled("No containers");
        }
        for item in build_list(rows.values(), "", &HashSet::new()) {
            match item {
                ListItem::Group { project, total, running, failed, .. } => {
                    let dot = if failed > 0 {
                        Dot::Failed
                    } else if running > 0 {
                        Dot::Running
                    } else {
                        Dot::Stopped
                    };
                    b.submenu(project.clone(), 0, dot, |b| {
                        b.disabled(format!("{running} of {total} running")).separator();
                        let group = |a| Some(Command::RunGroup(project.clone(), a));
                        b.item("Start All", (running < total).then(|| group(Action::Start)).flatten());
                        b.item("Stop All", (running > 0).then(|| group(Action::Stop)).flatten());
                        b.item("Restart All", group(Action::Restart));
                    });
                }
                ListItem::Row { id, grouped } => {
                    let Some(row) = rows.get(&id) else { continue };
                    let pending = store.set.pending.get(&id).copied();
                    let dot = row_dot(row, pending, row.failed_in(rows.values()));
                    b.submenu(row.name.clone(), grouped as isize, dot, |b| row_menu(b, row, pending));
                }
            }
        }
    }

    b.separator().item(format!("Quit {}", native::APP_NAME), Some(Command::Quit));
    b.item("Quit Completely", Some(Command::QuitCompletely));
    (b.lines, b.commands)
}

fn engine_label(store: &ContainerStore) -> String {
    if store.starting_colima {
        return "Colima is starting…".into();
    }
    match &store.status {
        EngineStatus::Connecting => "Connecting…".into(),
        EngineStatus::Connected => match store.profile() {
            Some(p) if p != "default" => format!("Colima is running ({p})"),
            _ => "Colima is running".into(),
        },
        // Other machines can run, so a stopped machine always shows its name.
        EngineStatus::NotRunning => match store.profile() {
            Some(p) => format!("{p} is stopped"),
            None => "Colima is stopped".into(),
        },
        EngineStatus::Error(_) => "Cannot connect to Docker".into(),
    }
}

fn row_dot(row: &ContainerRow, pending: Option<Action>, failed: bool) -> Dot {
    if pending.is_some() || matches!(row.state, State::Paused | State::Restarting) {
        Dot::Busy
    } else if row.state == State::Running {
        Dot::Running
    } else if failed {
        Dot::Failed
    } else {
        Dot::Stopped
    }
}

/// The submenu of a container. A pending action disables the actions.
fn row_menu(b: &mut Builder, row: &ContainerRow, pending: Option<Action>) {
    let id = &row.id;
    let running = row.state.is_running();
    let action = |a| (pending.is_none()).then(|| Command::Run(id.clone(), a));
    b.disabled(pending.map_or(row.status.clone(), |a| a.pending_label().to_string()));
    b.separator();
    if running {
        b.item("Stop", action(Action::Stop));
        b.item("Restart", action(Action::Restart));
    } else {
        b.item("Start", action(Action::Start));
    }
    let urls: Vec<_> = row.ports.iter().filter_map(|p| Some((p.host(), p.url()?))).collect();
    if running && !urls.is_empty() {
        b.separator();
        for (host, url) in urls {
            b.item(format!("Open {host}"), Some(Command::OpenUrl(url)));
        }
    }
    b.separator().item(format!("Show in {}", native::APP_NAME), Some(Command::Reveal(id.clone())));
}

#[cfg(test)]
mod tests {
    use super::{Action, Builder, Command, Dot, StatusLine};

    fn labels(lines: &[StatusLine]) -> Vec<String> {
        lines
            .iter()
            .map(|l| match l {
                StatusLine::Item { label, enabled: true, .. } => label.to_string(),
                StatusLine::Item { label, enabled: false, .. } => format!("({label})"),
                StatusLine::Separator => "-".into(),
            })
            .collect()
    }

    #[test]
    fn submenu_tags_point_to_their_commands() {
        let mut b = Builder::default();
        b.item("Open", Some(Command::Open));
        b.submenu("web", 0, Dot::Running, |b| {
            b.disabled("Up 5 minutes").separator();
            b.item("Stop", Some(Command::Run("a".into(), Action::Stop)));
            b.item("Start", None);
        });
        b.item("Quit", Some(Command::Quit));

        assert_eq!(labels(&b.lines), ["Open", "web", "Quit"]);
        let StatusLine::Item { submenu, tag: None, .. } = &b.lines[1] else { panic!() };
        assert_eq!(labels(submenu), ["(Up 5 minutes)", "-", "Stop", "(Start)"]);
        let StatusLine::Item { tag: Some(stop), .. } = &submenu[2] else { panic!() };
        assert_eq!(b.commands[*stop], Command::Run("a".into(), Action::Stop));
        let StatusLine::Item { tag: Some(quit), .. } = &b.lines[2] else { panic!() };
        assert_eq!(b.commands[*quit], Command::Quit);
    }
}
