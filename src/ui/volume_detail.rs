//! The detail pane of a volume: a summary, the files, and the inspect data.

use std::ops::Range;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use crate::colima::{self, Entry, EntryKind, MAX_ENTRIES};
use crate::docker::stats::format_bytes;
use crate::docker::volumes::{self, VolumeInfo};
use crate::runtime::Backend;
use crate::state::container_store::ContainerStore;
use crate::state::volume_store::VolumeStore;
use crate::ui::json_view::{self, Json};
use crate::ui::container_detail::{card, field, list_row, loading, section, segmented_control};
use crate::ui::{ENTER_DURATION, NATIVE_BUTTON_HEIGHT, TOOLBAR_HEIGHT, drag_area, native, tabular_nums, text, toast};

const FILE_ROW_HEIGHT: f32 = 30.;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum VolumeTab {
    #[default]
    Info,
    Files,
    Inspect,
}

impl VolumeTab {
    const ALL: [VolumeTab; 3] = [VolumeTab::Info, VolumeTab::Files, VolumeTab::Inspect];

    fn label(self) -> &'static str {
        match self {
            VolumeTab::Info => "Info",
            VolumeTab::Files => "Files",
            VolumeTab::Inspect => "Inspect",
        }
    }
}

pub enum VolumeDetailEvent {
    Close,
    /// Show a container in the container list.
    RevealContainer(String),
    /// Ask the user, then remove the volume.
    Remove(String),
}

enum Load<T> {
    Loading,
    Loaded(T),
    Failed(String),
}

pub struct VolumeDetail {
    name: String,
    store: Entity<VolumeStore>,
    containers: Entity<ContainerStore>,
    tab: VolumeTab,
    /// The number of tab changes. Each change fades in the new tab body.
    tab_changes: usize,
    info: Load<Rc<VolumeInfo>>,
    inspect_json: Rc<Json>,
    inspect_list: ListState,
    /// The folder that the Files tab shows, as names below the mount point.
    folder: Vec<String>,
    /// `None` until the Files tab shows one time.
    files: Option<Load<Rc<Vec<Entry>>>>,
    /// Increments on each folder load. A result with an old value is for a
    /// different folder, so the pane ignores it.
    files_gen: u64,
    files_scroll: UniformListScrollHandle,
}

impl EventEmitter<VolumeDetailEvent> for VolumeDetail {}

impl VolumeDetail {
    pub fn new(
        name: String,
        store: Entity<VolumeStore>,
        containers: Entity<ContainerStore>,
        tab: VolumeTab,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = VolumeDetail {
            name,
            store,
            containers,
            tab: VolumeTab::Info,
            tab_changes: 0,
            info: Load::Loading,
            inspect_json: Rc::new(Json::default()),
            inspect_list: ListState::new(0, ListAlignment::Top, px(200.)),
            folder: Vec::new(),
            files: None,
            files_gen: 0,
            files_scroll: UniformListScrollHandle::new(),
        };
        this.load_info(cx);
        this.set_tab(tab, cx);
        // The pane has its own entrance, so the first tab does not fade.
        this.tab_changes = 0;
        this
    }

    pub fn tab(&self) -> VolumeTab {
        self.tab
    }

    fn set_tab(&mut self, tab: VolumeTab, cx: &mut Context<Self>) {
        if tab != self.tab {
            self.tab_changes += 1;
        }
        self.tab = tab;
        if tab == VolumeTab::Files && self.files.is_none() {
            self.load_files(cx);
        }
        cx.notify();
    }

    fn load_info(&mut self, cx: &mut Context<Self>) {
        let Some(docker) = self.containers.read(cx).docker().cloned() else { return };
        let name = self.name.clone();
        let rx = Backend::global(cx).spawn(async move { volumes::inspect(&docker, &name).await });
        cx.spawn(async move |this, cx| {
            let result = rx.await;
            let _ = this.update(cx, |this, cx| {
                this.info = match result {
                    Ok(Ok(info)) => {
                        let json = Json::new(info.json.clone());
                        this.inspect_list.reset_with_uniform_height(json.lines.len(), px(json_view::LINE_HEIGHT));
                        this.inspect_json = Rc::new(json);
                        Load::Loaded(Rc::new(info))
                    }
                    Ok(Err(e)) => Load::Failed(e.to_string()),
                    Err(_) => Load::Failed("cancelled".into()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// The folder of the Files tab, as a path in the VM.
    fn folder_path(&self, cx: &App) -> Option<String> {
        let row = self.store.read(cx).rows.get(&self.name)?;
        let mut path = row.mountpoint.trim_end_matches('/').to_string();
        for part in &self.folder {
            path.push('/');
            path.push_str(part);
        }
        Some(path)
    }

    fn load_files(&mut self, cx: &mut Context<Self>) {
        self.files_gen += 1;
        let generation = self.files_gen;
        let profile = self.containers.read(cx).profile().map(str::to_string);
        let (Some(profile), Some(path)) = (profile, self.folder_path(cx)) else {
            self.files = Some(Load::Failed(
                "The files show only for a volume in a Colima machine.".into(),
            ));
            return;
        };
        // Keep the old list on screen for a reload of the same folder.
        if !matches!(self.files, Some(Load::Loaded(_))) {
            self.files = Some(Load::Loading);
        }
        let rx = Backend::global(cx).spawn(async move {
            tokio::task::spawn_blocking(move || colima::list_folder(&profile, &path))
                .await
                .map_err(anyhow::Error::from)
                .and_then(|r| r)
        });
        cx.spawn(async move |this, cx| {
            let result = rx.await;
            let _ = this.update(cx, |this, cx| {
                if this.files_gen != generation {
                    return;
                }
                this.files = Some(match result {
                    Ok(Ok(entries)) => Load::Loaded(Rc::new(entries)),
                    Ok(Err(e)) => Load::Failed(e.to_string()),
                    Err(_) => Load::Failed("cancelled".into()),
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens a folder of the current folder. `None` opens the enclosing
    /// folder.
    fn open_folder(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        match name {
            // A name from `find` has no slash. Refuse the names that go up.
            Some(name) if name != "." && name != ".." && !name.contains('/') => self.folder.push(name),
            Some(_) => return,
            None => {
                if self.folder.pop().is_none() {
                    return;
                }
            }
        }
        self.files = Some(Load::Loading);
        self.files_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.load_files(cx);
        cx.notify();
    }

    /// The containers that use the volume: the ID, the name, and the state.
    fn users(&self, cx: &App) -> Vec<(String, String, crate::docker::containers::State, String)> {
        let mut users: Vec<_> = self
            .containers
            .read(cx)
            .set
            .rows
            .values()
            .filter(|c| c.volumes.contains(&self.name))
            .map(|c| (c.id.clone(), c.name.clone(), c.state, c.status.clone()))
            .collect();
        users.sort_by(|a, b| a.1.cmp(&b.1));
        users
    }

    fn render_info(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let mono = t.mono_font_family.clone();
        let muted = t.muted_foreground;
        let store = self.store.read(cx);
        let Some(row) = store.rows.get(&self.name).cloned() else {
            return div().into_any_element();
        };
        let size = store.sizes.get(&self.name).copied();
        let info = match &self.info {
            Load::Loaded(info) => Some(info.clone()),
            _ => None,
        };
        let created = info
            .as_ref()
            .and_then(|i| i.created.as_deref())
            .and_then(|c| chrono::DateTime::parse_from_rfc3339(c).ok())
            .map(|c| ago(c.timestamp()));
        let mountpoint = row.mountpoint.clone();

        let summary = card(
            [
                field("Name", div().font_family(mono.clone()).truncate().child(row.name.clone()), cx),
                field("Driver", div().child(row.driver.clone()), cx),
                field(
                    "Size",
                    div()
                        .font_features(tabular_nums())
                        .child(size.map_or("–".into(), format_bytes)),
                    cx,
                ),
                field("Created", div().child(created.unwrap_or_else(|| "–".into())), cx),
                field(
                    "Mount point",
                    h_flex()
                        .gap_1()
                        .child(div().flex_1().min_w_0().truncate().font_family(mono.clone()).child(mountpoint.clone()))
                        .child(
                            Button::new("copy-mountpoint")
                                .icon(Icon::new(IconName::Copy))
                                .ghost()
                                .xsmall()
                                .tooltip("Copy the path in the Colima machine")
                                .on_click(move |_, _, cx| {
                                    toast::copy(mountpoint.clone(), mountpoint.clone(), cx)
                                }),
                        ),
                    cx,
                ),
            ],
            cx,
        );

        let compose = row.compose_project.clone().map(|project| {
            section("Compose", cx).child(card(
                [
                    Some(field("Project", div().child(project), cx)),
                    row.compose_volume.clone().map(|v| field("Volume", div().font_family(mono.clone()).child(v), cx)),
                ]
                .into_iter()
                .flatten(),
                cx,
            ))
        });

        let users = self.users(cx);
        let in_use = !users.is_empty();
        let used_by = section("Used by", cx).child(if users.is_empty() {
            card([list_row().text_color(muted).child("No container uses this volume.")], cx)
        } else {
            card(
                users.into_iter().enumerate().map(|(ix, (id, name, state, status))| {
                    let dot = super::state_color(state, cx);
                    list_row()
                        .id(("user", ix))
                        .gap_2p5()
                        .cursor_pointer()
                        .hover(|d| d.bg(native::hover(cx)))
                        .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(dot))
                        .child(div().flex_1().min_w_0().truncate().child(name))
                        .child(div().flex_shrink_0().max_w(relative(0.5)).truncate().text_color(muted).child(status))
                        .child(Icon::new(IconName::ChevronRight).xsmall().text_color(muted))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(VolumeDetailEvent::RevealContainer(id.clone()))
                        }))
                        // `card` takes plain rows.
                        .into_any_element()
                })
                .map(|el| div().child(el)),
                cx,
            )
        });

        let pairs = |title: &'static str, list: &[(String, String)], cx: &App| {
            (!list.is_empty()).then(|| {
                section(title, cx).child(card(
                    list.iter().map(|(k, v)| {
                        list_row()
                            .gap_3()
                            .items_start()
                            .font_family(mono.clone())
                            .text_size(px(text::CALLOUT))
                            .child(div().w(relative(0.45)).flex_shrink_0().text_color(muted).child(k.clone()))
                            .child(div().flex_1().min_w_0().child(if v.is_empty() { "–".to_string() } else { v.clone() }))
                    }),
                    cx,
                ))
            })
        };
        let labels = info.as_ref().and_then(|i| pairs("Labels", &i.labels, cx));
        let options = info.as_ref().and_then(|i| pairs("Options", &i.options, cx));

        let remove = Button::new("remove-volume")
            .outline()
            .small()
            .h(px(NATIVE_BUTTON_HEIGHT))
            .w_full()
            .icon(Icon::new(IconName::Trash))
            .label("Remove…")
            .disabled(in_use || store.removing.contains(&self.name))
            .tooltip(if in_use { "Remove the containers that use the volume first" } else { "Delete the volume and its data" })
            .on_click(cx.listener(|this, _, _, cx| cx.emit(VolumeDetailEvent::Remove(this.name.clone()))));

        // The content is in an inner column, so a long list scrolls and
        // the cards do not shrink to the height of the pane.
        div()
            .id("volume-info")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .px_5()
                    .pb_5()
                    .gap_5()
                    .text_size(px(text::BODY))
                    .child(summary)
                    .children(compose)
                    .child(used_by)
                    .children(labels)
                    .children(options)
                    .child(div().pt_1().child(remove)),
            )
            .into_any_element()
    }

    fn render_files(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let muted = t.muted_foreground;
        let at_root = self.folder.is_empty();
        let shown = self.store.read(cx).rows.get(&self.name).map(|r| r.display_name(true).to_string());
        let crumbs = std::iter::once(shown.unwrap_or_default())
            .chain(self.folder.iter().cloned())
            .collect::<Vec<_>>()
            .join(" / ");
        let bar = h_flex()
            .px_5()
            .pb_2()
            .gap_1()
            .child(
                Button::new("folder-up")
                    .icon(Icon::new(IconName::ChevronLeft))
                    .ghost()
                    .xsmall()
                    .disabled(at_root)
                    .tooltip("Enclosing folder")
                    .on_click(cx.listener(|this, _, _, cx| this.open_folder(None, cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(t.mono_font_family.clone())
                    .text_size(px(text::CALLOUT))
                    .text_color(muted)
                    .child(crumbs),
            )
            .child(
                Button::new("reload-files")
                    .icon(Icon::new(IconName::RefreshCw))
                    .ghost()
                    .xsmall()
                    .tooltip("Read the folder again")
                    .on_click(cx.listener(|this, _, _, cx| this.load_files(cx))),
            );

        let body = match &self.files {
            None | Some(Load::Loading) => loading("files-loading"),
            Some(Load::Failed(e)) => div().px_5().text_color(t.red).child(e.clone()).into_any_element(),
            Some(Load::Loaded(entries)) if entries.is_empty() => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .pb(px(TOOLBAR_HEIGHT))
                .text_color(muted)
                .child("The folder is empty")
                .into_any_element(),
            Some(Load::Loaded(entries)) => {
                let entries = entries.clone();
                let full = entries.len() >= MAX_ENTRIES;
                v_flex()
                    .size_full()
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .child(
                                uniform_list(
                                    "volume-files",
                                    entries.len(),
                                    cx.processor(move |this, range: Range<usize>, _, cx| {
                                        range.map(|ix| this.render_entry(ix, &entries[ix], cx)).collect::<Vec<_>>()
                                    }),
                                )
                                .size_full()
                                .track_scroll(&self.files_scroll),
                            )
                            .child(Scrollbar::vertical(&self.files_scroll)),
                    )
                    .when(full, |d| {
                        d.child(
                            div()
                                .px_5()
                                .py_2()
                                .text_size(px(text::CALLOUT))
                                .text_color(muted)
                                .child(format!("The list shows the first {MAX_ENTRIES} entries.")),
                        )
                    })
                    .into_any_element()
            }
        };
        v_flex()
            .size_full()
            .child(bar)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    fn render_entry(&self, ix: usize, entry: &Entry, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let folder = entry.kind == EntryKind::Folder;
        let icon = match entry.kind {
            EntryKind::Folder => IconName::Folder,
            EntryKind::Link => IconName::FileSymlink,
            EntryKind::File | EntryKind::Other => IconName::File,
        };
        let name = entry.name.clone();
        let row = h_flex()
            .id(("entry", ix))
            .w_full()
            .h(px(FILE_ROW_HEIGHT))
            .px_2()
            .gap_2()
            .rounded(px(6.))
            .text_size(px(text::BODY))
            .child(Icon::new(icon).small().text_color(if folder { native::accent(cx) } else { t.muted_foreground }))
            .child(div().flex_1().min_w_0().truncate().child(entry.name.clone()))
            // A long name truncates, so the tooltip shows all of it.
            .tooltip({
                let name = entry.name.clone();
                move |window, cx| gpui_component::tooltip::Tooltip::new(name.clone()).build(window, cx)
            })
            .child(
                div()
                    .w(px(72.))
                    .flex_shrink_0()
                    .text_right()
                    .text_size(px(text::CALLOUT))
                    .font_features(tabular_nums())
                    .text_color(t.muted_foreground)
                    .when(!folder, |d| d.child(format_bytes(entry.size))),
            )
            .child(
                div()
                    .w(px(96.))
                    .flex_shrink_0()
                    .text_right()
                    .truncate()
                    .text_size(px(text::CALLOUT))
                    .text_color(t.muted_foreground)
                    .child(ago(entry.modified)),
            )
            .when(folder, |d| {
                d.cursor_pointer()
                    .hover(|s| s.bg(native::hover(cx)))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_folder(Some(name.clone()), cx)))
            });
        // A row of a uniform list gets the width of its content, so the
        // wrapper sets the width of the pane.
        div().w_full().px_3().child(row).into_any_element()
    }

    fn render_inspect(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        match &self.info {
            Load::Loading => loading("volume-inspect-loading"),
            Load::Failed(e) => div().p_3().text_color(t.red).child(e.clone()).into_any_element(),
            Load::Loaded(_) => json_view::render(&self.inspect_json, &self.inspect_list, cx),
        }
    }
}

/// A time as words, for example "3 days ago". `secs` is the time in seconds
/// since the Unix epoch.
fn ago(secs: i64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    ago_from(now - secs)
}

fn ago_from(delta: i64) -> String {
    let unit = |n: i64, word: &str| format!("{n} {word}{} ago", if n == 1 { "" } else { "s" });
    match delta.max(0) {
        d if d < 60 => "Just now".into(),
        d if d < 3_600 => unit(d / 60, "minute"),
        d if d < 86_400 => unit(d / 3_600, "hour"),
        d if d < 2_592_000 => unit(d / 86_400, "day"),
        d if d < 31_536_000 => unit(d / 2_592_000, "month"),
        d => unit(d / 31_536_000, "year"),
    }
}

impl Render for VolumeDetail {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let store = self.store.read(cx);
        let row = store.rows.get(&self.name).cloned();
        let size = store.sizes.get(&self.name).copied();

        let body = match self.tab {
            VolumeTab::Info => self.render_info(cx),
            VolumeTab::Files => self.render_files(cx),
            VolumeTab::Inspect => self.render_inspect(cx),
        };
        let body = if self.tab_changes > 0 {
            div()
                .size_full()
                .child(body)
                .with_animation(
                    ("volume-tab-fade", self.tab_changes),
                    Animation::new(ENTER_DURATION).with_easing(ease_out_quint()),
                    |d, t| d.opacity(t),
                )
                .into_any_element()
        } else {
            body
        };

        let header = row.as_ref().map(|r| {
            let kind = if r.anonymous { "Anonymous volume".to_string() } else { format!("{} volume", r.driver) };
            let subtitle = [r.compose_project.clone(), Some(kind), size.map(format_bytes)]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            h_flex()
                .px_5()
                .pt_3()
                .pb_5()
                .gap_3()
                .child(
                    div()
                        .size(px(48.))
                        .flex_shrink_0()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(t.border)
                        .bg(t.background)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(IconName::HardDrive).size(px(24.)).text_color(native::accent(cx))),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_size(px(text::TITLE3))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(r.display_name(true).to_string()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(text::CALLOUT))
                                .text_color(t.muted_foreground)
                                .child(subtitle),
                        ),
                )
        });

        let labels: Vec<&'static str> = VolumeTab::ALL.iter().map(|t| t.label()).collect();
        let selected = VolumeTab::ALL.iter().position(|&tab| tab == self.tab).unwrap_or(0);
        let view = cx.weak_entity();
        let tabs = segmented_control(
            "volume-tabs",
            labels,
            selected,
            Rc::new(move |ix, _, cx| {
                let _ = view.update(cx, |this, cx| this.set_tab(VolumeTab::ALL[ix], cx));
            }),
            cx,
        );

        v_flex()
            .size_full()
            .border_l_1()
            .border_color(t.border)
            .bg(t.sidebar.opacity(0.5))
            .child(
                h_flex()
                    .h(px(TOOLBAR_HEIGHT + 8.))
                    .flex_shrink_0()
                    .px_3()
                    .child(drag_area("volume-detail-drag-left").flex_1().h_full())
                    .child(tabs)
                    .child(drag_area("volume-detail-drag-right").flex_1().h_full())
                    .child(
                        Button::new("close-volume-detail")
                            .icon(Icon::new(IconName::X))
                            .custom(native::toolbar_button(cx))
                            .xsmall()
                            .tooltip("Close (Esc)")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(VolumeDetailEvent::Close))),
                    ),
            )
            .children(header)
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::ago_from;

    #[test]
    fn times_read_as_words() {
        assert_eq!(ago_from(5), "Just now");
        assert_eq!(ago_from(-5), "Just now");
        assert_eq!(ago_from(60), "1 minute ago");
        assert_eq!(ago_from(7_200), "2 hours ago");
        assert_eq!(ago_from(86_400 * 3), "3 days ago");
        assert_eq!(ago_from(31_536_000 * 2), "2 years ago");
    }
}
