//! The form that makes a new Colima machine or changes the settings of a
//! machine. It shows in the modal panel of the main window.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState, NumberInput};
use gpui_component::radio::RadioGroup;
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::{NATIVE_BUTTON_HEIGHT, modal, text};
use crate::colima::{self, Mount, Runtime, Settings, VmType};
use crate::state::machine_store::MachineStore;

/// The width of the panel.
const DIALOG_WIDTH: f32 = 500.;
/// The width of the labels at the left of the controls, as in a macOS form.
const LABEL_WIDTH: f32 = 120.;
/// The height of a form row with a short control. The label box and the
/// control box both have this height.
const ROW_HEIGHT: f32 = 26.;
/// The width of the number fields.
const NUMBER_WIDTH: f32 = 130.;
/// The smallest disk of a new machine, in GiB.
const MIN_DISK: u32 = 10;
/// The largest disk, in GiB.
const MAX_DISK: u32 = 2000;

/// What the form does when the user confirms it.
#[derive(Clone)]
enum Mode {
    New,
    Edit { name: String, old: Settings, running: bool },
}

pub struct MachineForm {
    mode: Mode,
    machines: Entity<MachineStore>,
    name: Entity<InputState>,
    cpus: Entity<InputState>,
    memory: Entity<InputState>,
    disk: Entity<InputState>,
    runtime: Runtime,
    vm_type: VmType,
    rosetta: bool,
    mounts: Vec<Mount>,
    /// The problem of the last confirm, if the values are not correct.
    error: Option<SharedString>,
    host_cpus: u32,
    host_memory: f32,
    /// True after a confirm that started the command. A second Return then
    /// has no effect.
    done: bool,
    _subs: Vec<Subscription>,
}

/// Opens the form for a new machine. `animate` is false for an open from
/// the keyboard.
pub fn open_new(machines: Entity<MachineStore>, animate: bool, window: &mut Window, cx: &mut App) {
    open(Mode::New, machines, animate, window, cx);
}

/// Opens the form that changes the settings of the machine `name`.
pub fn open_edit(
    name: String,
    settings: Settings,
    running: bool,
    machines: Entity<MachineStore>,
    window: &mut Window,
    cx: &mut App,
) {
    open(Mode::Edit { name, old: settings, running }, machines, true, window, cx);
}

fn open(mode: Mode, machines: Entity<MachineStore>, animate: bool, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| MachineForm::new(mode, machines, window, cx));
    let focus = match form.read(cx).mode {
        Mode::New => form.read(cx).name.read(cx).focus_handle(cx),
        Mode::Edit { .. } => form.read(cx).cpus.read(cx).focus_handle(cx),
    };
    let confirm = form.clone();
    modal::open(
        modal::Request {
            view: form.into(),
            width: DIALOG_WIDTH,
            focus: Some(focus),
            // Return comes from the keyboard, so the panel closes at once.
            on_confirm: Some(Rc::new(move |window, cx| {
                if confirm.update(cx, |form, cx| form.confirm(cx)) {
                    modal::close(false, window, cx);
                }
            })),
            animate,
        },
        window,
        cx,
    );
}

impl MachineForm {
    fn new(mode: Mode, machines: Entity<MachineStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (host_cpus, host_memory) = colima::host_resources();
        let settings = match &mode {
            Mode::New => Settings::default(),
            Mode::Edit { old, .. } => old.clone(),
        };
        let number = |value: String, step: f64, min: f64, max: f64, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).default_value(value).step(step).min(min).max(max))
        };
        let min_disk = match &mode {
            // A disk can get larger, but not smaller.
            Mode::Edit { old, .. } => old.disk,
            Mode::New => MIN_DISK,
        };
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("dev"));
        let cpus = number(settings.cpus.to_string(), 1., 1., host_cpus as f64, window, cx);
        let memory = number(colima::format_gib(settings.memory), 1., 1., host_memory.floor() as f64, window, cx);
        let disk = number(settings.disk.to_string(), 10., min_disk as f64, MAX_DISK as f64, window, cx);
        // A field can take Return before the panel gets it.
        let subs = [&name, &cpus, &memory, &disk]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, ev: &InputEvent, window, cx| {
                    if matches!(ev, InputEvent::PressEnter { .. }) && this.confirm(cx) {
                        modal::close(false, window, cx);
                    }
                })
            })
            .collect();
        MachineForm {
            mode,
            machines,
            name,
            cpus,
            memory,
            disk,
            runtime: settings.runtime,
            vm_type: settings.vm_type,
            rosetta: settings.rosetta,
            mounts: settings.mounts,
            error: None,
            host_cpus,
            host_memory,
            done: false,
            _subs: subs,
        }
    }

    /// Reads the values of the form. The error tells the first value that
    /// is not correct.
    fn values(&self, cx: &App) -> Result<(String, Settings), String> {
        let name = match &self.mode {
            Mode::Edit { name, .. } => name.clone(),
            Mode::New => {
                let name = self.name.read(cx).value().trim().to_string();
                if name.is_empty() {
                    return Err("Type a name for the machine.".into());
                }
                if !colima::valid_profile_name(&name) {
                    return Err("Use only letters, digits, “-”, “_”, and “.” in the name.".into());
                }
                if self.machines.read(cx).machines.iter().any(|m| m.name == name) {
                    return Err(format!("A machine with the name “{name}” exists already."));
                }
                name
            }
        };
        let cpus = self.cpus.read(cx).value().trim().parse::<u32>().ok().filter(|n| (1..=self.host_cpus).contains(n));
        let Some(cpus) = cpus else {
            return Err(format!("Set the CPUs to a whole number from 1 to {}.", self.host_cpus));
        };
        let max_memory = self.host_memory.floor();
        let memory = self.memory.read(cx).value().trim().parse::<f32>().ok().filter(|m| (0.5..=max_memory).contains(m));
        let Some(memory) = memory else {
            return Err(format!("Set the memory to a number from 0.5 to {max_memory:.0} GiB."));
        };
        let min_disk = match &self.mode {
            Mode::Edit { old, .. } => old.disk,
            Mode::New => MIN_DISK,
        };
        let disk = self.disk.read(cx).value().trim().parse::<u32>().ok().filter(|d| (min_disk..=MAX_DISK).contains(d));
        let Some(disk) = disk else {
            return Err(match self.mode {
                Mode::Edit { .. } => format!("Set the disk to {min_disk} GiB or more. A disk cannot get smaller."),
                Mode::New => format!("Set the disk to a whole number from {MIN_DISK} to {MAX_DISK} GiB."),
            });
        };
        let settings = Settings {
            cpus,
            memory,
            disk,
            runtime: self.runtime,
            vm_type: self.vm_type,
            rosetta: self.rosetta && self.vm_type == VmType::Vz,
            mounts: self.mounts.clone(),
        };
        Ok((name, settings))
    }

    /// Starts the command of the form. Returns false, and shows the
    /// problem, if a value is not correct.
    fn confirm(&mut self, cx: &mut Context<Self>) -> bool {
        if self.done {
            return false;
        }
        let (name, settings) = match self.values(cx) {
            Ok(values) => values,
            Err(e) => {
                self.error = Some(e.into());
                cx.notify();
                return false;
            }
        };
        match self.mode.clone() {
            Mode::New => self.machines.update(cx, |m, cx| m.create(&name, settings, cx)),
            // No change needs no restart.
            Mode::Edit { old, .. } if old == settings => {}
            Mode::Edit { old, .. } => self.machines.update(cx, |m, cx| m.configure(&name, old, settings, cx)),
        }
        self.done = true;
        true
    }

    fn add_folders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Share".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let _ = this.update(cx, |this, cx| {
                for path in paths {
                    let location = path.display().to_string();
                    if !this.mounts.iter().any(|m| m.location == location) {
                        this.mounts.push(Mount { location, writable: false });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn editing(&self) -> bool {
        matches!(self.mode, Mode::Edit { .. })
    }

    fn render_mounts(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let rows = self.mounts.iter().enumerate().map(|(ix, m)| {
            h_flex()
                .id(("mount", ix))
                .gap_2()
                .h(px(26.))
                .child(Icon::new(IconName::Folder).small().text_color(muted))
                .child(div().flex_1().min_w_0().truncate().child(m.location.clone()))
                .child(
                    Checkbox::new(("mount-writable", ix))
                        .label("Writable")
                        .small()
                        .checked(m.writable)
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            if let Some(m) = this.mounts.get_mut(ix) {
                                m.writable = *checked;
                            }
                            cx.notify();
                        })),
                )
                .child(
                    Button::new(("mount-remove", ix))
                        .icon(Icon::new(IconName::Close))
                        .ghost()
                        .xsmall()
                        .tooltip("Remove")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if ix < this.mounts.len() {
                                this.mounts.remove(ix);
                            }
                            cx.notify();
                        })),
                )
        });
        let rows: Vec<_> = rows.collect();
        v_flex()
            .w_full()
            .gap_1()
            .children(rows)
            .child(
                h_flex().child(
                    Button::new("mount-add")
                        .label("Add Folder…")
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| this.add_folders(window, cx))),
                ),
            )
            .into_any_element()
    }
}

/// A row of the form: a label at the left and the control at the right.
/// The optional note shows when the pointer is on the info icon after the
/// label.
fn row(label: &'static str, control: impl IntoElement, note: Option<&'static str>, cx: &App) -> Div {
    let t = cx.theme();
    h_flex()
        .w_full()
        .items_start()
        .gap_3()
        .child(
            h_flex()
                .w(px(LABEL_WIDTH))
                .flex_shrink_0()
                .h(px(ROW_HEIGHT))
                .gap_1()
                .text_size(px(text::BODY))
                .child(label)
                .when_some(note, |d, note| {
                    d.child(
                        div()
                            .id(label)
                            .text_color(t.muted_foreground)
                            .hover(|d| d.text_color(t.foreground))
                            .child(Icon::new(IconName::Info).small())
                            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(note).build(window, cx)),
                    )
                }),
        )
        // The control box has the height of the label box, so a short
        // control has its center on the label. A taller control, such as
        // the folder list, starts at the top.
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .min_h(px(ROW_HEIGHT))
                .child(div().w_full().child(control)),
        )
}

fn number(state: &Entity<InputState>, suffix: Option<&'static str>, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    h_flex()
        .gap_2()
        .child(div().w(px(NUMBER_WIDTH)).child(NumberInput::new(state).small()))
        .when_some(suffix, |d, suffix| d.child(div().text_color(t.muted_foreground).child(suffix)))
}

impl Render for MachineForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editing = self.editing();
        let fixed = editing.then_some("Make a new machine to change this.");
        let runtime_ix = Runtime::ALL.iter().position(|&r| r == self.runtime);
        let vm_ix = VmType::ALL.iter().position(|&v| v == self.vm_type);
        let runtime = RadioGroup::horizontal("machine-runtime")
            .children(Runtime::ALL.map(Runtime::label))
            .selected_index(runtime_ix)
            .disabled(editing)
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.runtime = Runtime::ALL[*ix];
                cx.notify();
            }));
        let vm_type = RadioGroup::horizontal("machine-vm-type")
            .children(VmType::ALL.map(VmType::label))
            .selected_index(vm_ix)
            .disabled(editing)
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.vm_type = VmType::ALL[*ix];
                cx.notify();
            }));
        // A block switch fills the row, and a click anywhere in the row
        // changes it. A flex item gets only the width of the track.
        let rosetta = h_flex().child(
            Switch::new("machine-rosetta")
                .checked(self.rosetta && self.vm_type == VmType::Vz)
                .disabled(self.vm_type != VmType::Vz)
                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                    this.rosetta = *checked;
                    cx.notify();
                })),
        );
        let error = self.error.clone();
        let restart_note = matches!(self.mode, Mode::Edit { running: true, .. })
            .then_some("Colima stops the machine, and starts it again with the new settings.");
        let mounts = self.render_mounts(cx);
        let (muted, red) = (cx.theme().muted_foreground, cx.theme().red);
        let fields = v_flex()
            .w_full()
            .gap_3()
            .py_2()
            .text_size(px(text::BODY))
            .when(!editing, |d| {
                d.child(row("Name:", Input::new(&self.name).small(), None, cx))
            })
            .child(row("CPUs:", number(&self.cpus, None, cx), None, cx))
            .child(row("Memory:", number(&self.memory, Some("GiB"), cx), None, cx))
            .child(row(
                "Disk:",
                number(&self.disk, Some("GiB"), cx),
                editing.then_some("A disk can get larger, but not smaller."),
                cx,
            ))
            .child(row("Runtime:", runtime, fixed, cx))
            .child(row("Virtualization:", vm_type, fixed, cx))
            .child(row(
                "Rosetta:",
                rosetta,
                Some("Runs x86_64 programs fast. It needs Apple Virtualization."),
                cx,
            ))
            .child(row(
                "Folders:",
                mounts,
                Some("With no folders, Colima shares your home folder."),
                cx,
            ))
            .when_some(restart_note, |d, note| {
                d.child(div().text_size(px(text::CALLOUT)).text_color(muted).child(note))
            })
            .when_some(error, |d, e| d.child(div().text_size(px(text::CALLOUT)).text_color(red).child(e)));
        let t = cx.theme();
        let title: SharedString = match &self.mode {
            Mode::New => "New Machine".into(),
            Mode::Edit { name, .. } => format!("Settings of “{name}”").into(),
        };
        let confirm_label = match &self.mode {
            Mode::New => "Create",
            Mode::Edit { running: true, .. } => "Save and Restart",
            Mode::Edit { .. } => "Save",
        };
        v_flex()
            .w_full()
            .p_4()
            .gap_2()
            .bg(t.background)
            .border_1()
            .border_color(t.border)
            .rounded(t.radius_lg)
            .shadow_xl()
            .child(div().text_size(px(text::TITLE3)).font_weight(FontWeight::SEMIBOLD).child(title))
            .child(fields)
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("machine-form-cancel")
                            .label("Cancel")
                            .h(px(NATIVE_BUTTON_HEIGHT))
                            .on_click(|_, window, cx| modal::close(true, window, cx)),
                    )
                    .child(
                        Button::new("machine-form-confirm")
                            .label(confirm_label)
                            .primary()
                            .h(px(NATIVE_BUTTON_HEIGHT))
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.confirm(cx) {
                                    modal::close(true, window, cx);
                                }
                            })),
                    ),
            )
    }
}
