//! The detail pane of an image: a summary, the layers, and the inspect
//! data.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme, Disableable as _, Icon, Selectable, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use crate::docker::images::{self, ImageInfo, ImageRow, Layer, split_reference};
use crate::docker::stats::format_bytes;
use crate::runtime::Backend;
use crate::state::container_store::ContainerStore;
use crate::state::image_store::ImageStore;
use crate::ui::json_view::{self, Json};
use crate::ui::key_nav::{self, DetailKeys, KeyNav, Move};
use crate::ui::container_detail::{card, field, list_row, loading, section, segmented_control};
use crate::ui::image_icon::ImageLogo;
use crate::ui::image_list::remove_blocked;
use crate::ui::{
    ENTER_DURATION, NATIVE_BUTTON_HEIGHT, TOOLBAR_HEIGHT, ago, drag_area, native, spinner, tabular_nums,
    text, toast,
};

/// The most lines of a long build step. A click on the row shows all of it.
const LAYER_LINES: usize = 3;
/// The widths of the step number, the instruction badge, and the size
/// column of a layer row. Fixed widths keep the columns aligned.
const STEP_WIDTH: f32 = 18.;
const BADGE_WIDTH: f32 = 84.;
const SIZE_WIDTH: f32 = 72.;
/// The maximum width of the popover with the full step. A longer step
/// wraps onto more lines.
const POPOVER_WIDTH: f32 = 420.;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageTab {
    #[default]
    Info,
    Layers,
    Inspect,
}

impl ImageTab {
    const ALL: [ImageTab; 3] = [ImageTab::Info, ImageTab::Layers, ImageTab::Inspect];

    fn label(self) -> &'static str {
        match self {
            ImageTab::Info => "Info",
            ImageTab::Layers => "Layers",
            ImageTab::Inspect => "Inspect",
        }
    }
}

pub enum ImageDetailEvent {
    Close,
    /// Show a container in the container list.
    RevealContainer(String),
    /// Ask the user, then remove the tag or the image.
    Remove(String),
}

enum Load<T> {
    Loading,
    Loaded(T),
    Failed(String),
}

pub struct ImageDetail {
    /// A reference such as "redis:7-alpine", or the ID of an untagged
    /// image.
    key: String,
    store: Entity<ImageStore>,
    containers: Entity<ContainerStore>,
    tab: ImageTab,
    /// The number of tab changes. Each change fades in the new tab body.
    tab_changes: usize,
    info: Load<Rc<ImageInfo>>,
    inspect_json: Rc<Json>,
    inspect_list: ListState,
    /// `None` until the Layers tab shows one time.
    layers: Option<Load<Rc<Vec<Layer>>>>,
    /// The image ID that the data is for. A pull of the same tag can give
    /// the tag a new image, and the pane then loads the data again.
    loaded_id: Option<String>,
    /// The layer row that shows its full step in a popover.
    open_layer: Option<usize>,
    nav: KeyNav,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ImageDetailEvent> for ImageDetail {}

impl Focusable for ImageDetail {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.nav.focus.clone()
    }
}

impl DetailKeys for ImageDetail {
    fn nav(&self) -> &KeyNav {
        &self.nav
    }

    fn tab_position(&self) -> (usize, usize) {
        let ix = ImageTab::ALL.iter().position(|&t| t == self.tab).unwrap_or(0);
        (ix, ImageTab::ALL.len())
    }

    fn show_tab(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) {
        self.set_tab_at(ix, cx);
    }

    fn key_move(&mut self, m: Move, _: &mut Context<Self>) {
        match self.tab {
            ImageTab::Info | ImageTab::Layers => match m {
                Move::By(d) if d.abs() >= key_nav::PAGE_ROWS => self.nav.scroll_page(d.signum() as f32),
                _ => {
                    // The popover stays with its row, so a move closes it.
                    self.open_layer = None;
                    self.nav.move_cursor(m);
                }
            },
            ImageTab::Inspect => key_nav::scroll_lines(&self.inspect_list, m, false),
        }
    }

    fn key_back(&mut self, cx: &mut Context<Self>) -> bool {
        if self.open_layer.take().is_some() {
            cx.notify();
            return true;
        }
        false
    }
}

impl ImageDetail {
    pub fn new(
        key: String,
        store: Entity<ImageStore>,
        containers: Entity<ContainerStore>,
        tab: ImageTab,
        cx: &mut Context<Self>,
    ) -> Self {
        let subs = vec![cx.observe(&store, |this, _, cx| this.reload_if_changed(cx))];
        let mut this = ImageDetail {
            key,
            store,
            containers,
            tab: ImageTab::Info,
            tab_changes: 0,
            info: Load::Loading,
            inspect_json: Rc::new(Json::default()),
            inspect_list: ListState::new(0, ListAlignment::Top, px(200.)),
            layers: None,
            loaded_id: None,
            open_layer: None,
            nav: KeyNav::new(cx),
            _subs: subs,
        };
        this.reload_if_changed(cx);
        this.set_tab(tab, cx);
        // The pane has its own entrance, so the first tab does not fade.
        this.tab_changes = 0;
        this
    }

    pub fn tab(&self) -> ImageTab {
        self.tab
    }

    fn image(&self, cx: &App) -> Option<ImageRow> {
        self.store.read(cx).image(&self.key).cloned()
    }

    /// Loads the data when the key points to a different image.
    fn reload_if_changed(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.image(cx).map(|i| i.id) else { return };
        if self.loaded_id.as_ref() == Some(&id) {
            return;
        }
        self.loaded_id = Some(id);
        self.load_info(cx);
        if self.layers.is_some() {
            self.load_layers(cx);
        }
    }

    /// Shows the tab at a position, from 0. A position with no tab does
    /// nothing.
    pub fn set_tab_at(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(&tab) = ImageTab::ALL.get(ix) {
            self.set_tab(tab, cx);
        }
    }

    /// Shows the next tab, or the previous tab for a negative `delta`.
    pub fn cycle_tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = ImageTab::ALL.len() as isize;
        let ix = ImageTab::ALL.iter().position(|&t| t == self.tab).unwrap_or(0) as isize;
        self.set_tab(ImageTab::ALL[(ix + delta).rem_euclid(count) as usize], cx);
    }

    /// Takes the focus from the list. The cursor goes to the first row.
    pub fn focus_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.nav.focus.focus(window, cx);
        self.nav.start();
        cx.notify();
    }

    fn set_tab(&mut self, tab: ImageTab, cx: &mut Context<Self>) {
        if tab != self.tab {
            self.tab_changes += 1;
            self.nav.reset();
            self.open_layer = None;
        }
        self.tab = tab;
        if tab == ImageTab::Layers && self.layers.is_none() {
            self.load_layers(cx);
        }
        cx.notify();
    }

    fn load_info(&mut self, cx: &mut Context<Self>) {
        let Some(docker) = self.containers.read(cx).docker().cloned() else { return };
        let Some(id) = self.loaded_id.clone() else { return };
        let rx = Backend::global(cx).spawn(async move { images::inspect(&docker, &id).await });
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

    fn load_layers(&mut self, cx: &mut Context<Self>) {
        let Some(docker) = self.containers.read(cx).docker().cloned() else { return };
        let Some(id) = self.loaded_id.clone() else {
            self.layers = Some(Load::Loading);
            return;
        };
        self.layers = Some(Load::Loading);
        let rx = Backend::global(cx).spawn(async move { images::history(&docker, &id).await });
        cx.spawn(async move |this, cx| {
            let result = rx.await;
            let _ = this.update(cx, |this, cx| {
                this.layers = Some(match result {
                    Ok(Ok(layers)) => Load::Loaded(Rc::new(layers)),
                    Ok(Err(e)) => Load::Failed(e.to_string()),
                    Err(_) => Load::Failed("cancelled".into()),
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// The containers that use the image: the ID, the name, the state, and
    /// the status.
    fn users(&self, image_id: &str, cx: &App) -> Vec<(String, String, crate::docker::containers::State, String)> {
        let mut users: Vec<_> = self
            .containers
            .read(cx)
            .set
            .rows
            .values()
            .filter(|c| c.image_id == image_id)
            .map(|c| (c.id.clone(), c.name.clone(), c.state, c.status.clone()))
            .collect();
        users.sort_by(|a, b| a.1.cmp(&b.1));
        users
    }

    fn render_info(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let mono = t.mono_font_family.clone();
        let muted = t.muted_foreground;
        let Some(image) = self.image(cx) else {
            return div().into_any_element();
        };
        let info = match &self.info {
            Load::Loaded(info) => Some(info.clone()),
            _ => None,
        };
        let copy_button = |id: &'static str, value: String, shown: String, tip: &'static str| {
            self.nav.button(
                Button::new(id).icon(Icon::new(IconName::Copy)).ghost().xsmall().tooltip(tip),
                true,
                Rc::new(move |_, cx| toast::copy(value.clone(), shown.clone(), cx)),
                cx,
            )
        };
        let short_id = image.short_id().to_string();
        let mut rows = vec![
            field(
                "Image ID",
                h_flex()
                    .gap_1()
                    .child(div().flex_1().min_w_0().truncate().font_family(mono.clone()).child(short_id.clone()))
                    .child(copy_button("copy-id", image.id.clone(), short_id, "Copy the full ID")),
                cx,
            ),
        ];
        if image.tags.len() > 1 {
            rows.push(field(
                "Tags",
                v_flex()
                    .font_family(mono.clone())
                    .children(image.tags.iter().map(|t| div().truncate().child(t.clone()))),
                cx,
            ));
        }
        rows.push(field("Size", div().font_features(tabular_nums()).child(format_bytes(image.size)), cx));
        rows.push(field("Created", div().child(ago(image.created)), cx));
        if let Some(platform) = info.as_ref().and_then(|i| i.platform.clone()) {
            rows.push(field("Platform", div().font_family(mono.clone()).child(platform), cx));
        }
        if let Some(author) = info.as_ref().and_then(|i| i.author.clone()) {
            rows.push(field("Author", div().truncate().child(author), cx));
        }
        if let Some(digest) = image.digests.first().cloned() {
            let hash = digest.split_once('@').map_or(digest.as_str(), |(_, h)| h).to_string();
            rows.push(field(
                "Digest",
                h_flex()
                    .gap_1()
                    .child(div().flex_1().min_w_0().truncate().font_family(mono.clone()).child(hash.clone()))
                    .child(copy_button("copy-digest", digest.clone(), digest, "Copy the reference with the digest")),
                cx,
            ));
        }
        let summary = card(rows, cx);

        let config = info.as_ref().and_then(|info| {
            let mono_value = |v: String| div().font_family(mono.clone()).child(v);
            let rows: Vec<Div> = [
                info.entrypoint.clone().map(|v| field("Entrypoint", mono_value(v), cx)),
                info.command.clone().map(|v| field("Command", mono_value(v), cx)),
                info.working_dir.clone().map(|v| field("Working folder", mono_value(v), cx)),
                info.user.clone().map(|v| field("User", mono_value(v), cx)),
                (!info.exposed_ports.is_empty())
                    .then(|| field("Ports", mono_value(info.exposed_ports.join(", ")), cx)),
            ]
            .into_iter()
            .flatten()
            .collect();
            (!rows.is_empty()).then(|| section("Configuration", cx).child(card(rows, cx)))
        });

        let users = self.users(&image.id, cx);
        let in_use = !users.is_empty();
        let used_by = section("Used by", cx).child(if users.is_empty() {
            card([list_row().text_color(muted).child("No container uses this image.")], cx)
        } else {
            card(
                users.into_iter().enumerate().map(|(ix, (id, name, state, status))| {
                    let dot = super::state_color(state, cx);
                    let row = list_row()
                        .id(("user", ix))
                        .gap_2p5()
                        .cursor_pointer()
                        .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(dot))
                        .child(div().flex_1().min_w_0().truncate().child(name))
                        .child(div().flex_shrink_0().max_w(relative(0.5)).truncate().text_color(muted).child(status))
                        .child(Icon::new(IconName::ChevronRight).xsmall().text_color(muted));
                    let row = self.nav.row(
                        row,
                        key_nav::handler(cx, move |_, _, cx| cx.emit(ImageDetailEvent::RevealContainer(id.clone()))),
                        cx,
                    );
                    // `card` takes plain rows.
                    div().child(row)
                }),
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
                            .child(div().w(relative(0.4)).flex_shrink_0().text_color(muted).child(k.clone()))
                            .child(div().flex_1().min_w_0().child(if v.is_empty() { "–".to_string() } else { v.clone() }))
                    }),
                    cx,
                ))
            })
        };
        let env = info.as_ref().and_then(|i| pairs("Environment", &i.env, cx));
        let labels = info.as_ref().and_then(|i| pairs("Labels", &i.labels, cx));

        let store = self.store.read(cx);
        let tagged = !image.tags.is_empty();
        let pulling = store.pulls.contains_key(&self.key);
        let blocked = remove_blocked(&image, in_use);
        let pull = tagged.then(|| {
            self.nav
                .button(
                    Button::new("pull-again")
                        .outline()
                        .small()
                        .h(px(NATIVE_BUTTON_HEIGHT))
                        .w_full()
                        .icon(Icon::new(IconName::Download))
                        .when(pulling, |b| b.icon(spinner()))
                        .label(if pulling { "Pulling…" } else { "Pull Again" })
                        .loading(pulling)
                        .tooltip("Get the newest image for this tag from the registry"),
                    !pulling,
                    key_nav::handler(cx, |this, _, cx| {
                        let key = this.key.clone();
                        this.store.update(cx, |s, cx| s.pull(&key, cx));
                    }),
                    cx,
                )
                .flex_1()
        });
        let removing = store.removing.contains(&self.key);
        let remove = self
            .nav
            .button(
                Button::new("remove-image")
                    .outline()
                    .small()
                    .h(px(NATIVE_BUTTON_HEIGHT))
                    .w_full()
                    .icon(Icon::new(IconName::Trash))
                    .label(if image.tags.len() > 1 { "Remove Tag…" } else { "Remove…" })
                    .disabled(blocked || removing)
                    .tooltip(if blocked {
                        "Remove the containers that use the image first"
                    } else if image.tags.len() > 1 {
                        "Remove this tag. The image stays for its other tags."
                    } else {
                        "Delete the image"
                    }),
                !blocked && !removing,
                key_nav::handler(cx, |this, _, cx| cx.emit(ImageDetailEvent::Remove(this.key.clone()))),
                cx,
            )
            .flex_1();
        let buttons = h_flex().gap_3().pt_1().children(pull).child(remove);

        // The content is in an inner column, so a long list scrolls and
        // the cards do not shrink to the height of the pane.
        div()
            .id("image-info")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.nav.scroll)
            .child(
                v_flex()
                    .px_5()
                    .pb_5()
                    .gap_5()
                    .text_size(px(text::BODY))
                    .child(summary)
                    .children(config)
                    .child(used_by)
                    .children(env)
                    .children(labels)
                    .child(buttons),
            )
            .into_any_element()
    }

    fn render_layers(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let muted = t.muted_foreground;
        let layers = match &self.layers {
            None | Some(Load::Loading) => return loading("layers-loading"),
            Some(Load::Failed(e)) => return div().px_5().text_color(t.red).child(e.clone()).into_any_element(),
            Some(Load::Loaded(layers)) => layers.clone(),
        };
        let with_data = layers.iter().filter(|l| l.size > 0).count();
        let total: u64 = layers.iter().map(|l| l.size).sum();
        let summary = format!(
            "{} {} · {} with data · {}",
            layers.len(),
            if layers.len() == 1 { "step" } else { "steps" },
            with_data,
            format_bytes(total)
        );
        let mono = t.mono_font_family.clone();
        // The history starts with the newest step. Show the oldest step
        // first, so the steps read in the order of the Dockerfile.
        let rows = layers.iter().rev().enumerate().map(|(ix, layer)| {
            let (keyword, rest) = split_instruction(&layer.command);
            let tip = layer.command.clone();
            let empty = layer.size == 0;
            let step = ix + 1;
            let badge = div()
                .w(px(BADGE_WIDTH))
                .flex_shrink_0()
                .flex()
                .when_some(keyword, |d, keyword| {
                    let color = instruction_color(keyword, cx);
                    d.child(
                        div()
                            .px_1p5()
                            .rounded(px(4.))
                            .bg(color.opacity(0.14))
                            .text_color(color)
                            .font_family(mono.clone())
                            .text_size(px(text::SUBHEADLINE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(keyword.to_string()),
                    )
                });
            let size = v_flex()
                .w(px(SIZE_WIDTH))
                .flex_shrink_0()
                .items_end()
                .text_size(px(text::CALLOUT))
                .font_features(tabular_nums())
                .child(
                    div()
                        .when(empty, |d| d.text_color(muted))
                        .when(!empty, |d| d.font_weight(FontWeight::MEDIUM))
                        .child(format_bytes(layer.size)),
                );
            let row = list_row()
                .id(("layer", ix))
                .gap_3()
                .items_start()
                .cursor_pointer()
                .child(
                    div()
                        .w(px(STEP_WIDTH))
                        .flex_shrink_0()
                        .pt(px(1.))
                        .text_right()
                        .text_size(px(text::SUBHEADLINE))
                        .font_features(tabular_nums())
                        .text_color(muted.opacity(0.7))
                        .child(step.to_string()),
                )
                .child(badge)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(mono.clone())
                        .text_size(px(text::CALLOUT))
                        .line_clamp(LAYER_LINES)
                        .when(empty, |d| d.text_color(muted))
                        .child(if rest.is_empty() { "–".to_string() } else { rest.to_string() }),
                )
                .child(size);
            // A long step is cut, so a click on the row or Return shows all
            // of it.
            let row = self.nav.row_with_own_click(
                row,
                key_nav::handler(cx, move |this, _, cx| {
                    this.open_layer = if this.open_layer == Some(ix) { None } else { Some(ix) };
                    cx.notify();
                }),
                cx,
            );
            let mono = mono.clone();
            Popover::new(("layer-step", ix))
                .trigger(LayerRow { row, open: false })
                .trigger_style(StyleRefinement::default().w_full())
                .open(self.open_layer == Some(ix))
                .on_open_change(cx.listener(move |this, open: &bool, window, cx| {
                    if *open {
                        this.open_layer = Some(ix);
                    } else if this.open_layer == Some(ix) {
                        this.open_layer = None;
                        // The keys go back to the pane after the popover.
                        this.nav.focus.focus(window, cx);
                    }
                    cx.notify();
                }))
                .content(move |_, _, _| {
                    div()
                        .max_w(px(POPOVER_WIDTH))
                        .font_family(mono.clone())
                        .text_size(px(text::CALLOUT))
                        .child(tip.clone())
                })
        });
        div()
            .id("image-layers")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.nav.scroll)
            .child(
                v_flex()
                    .px_5()
                    .pb_5()
                    .gap_2()
                    .text_size(px(text::BODY))
                    .child(
                        div()
                            .text_size(px(text::CALLOUT))
                            .font_features(tabular_nums())
                            .text_color(muted)
                            .child(summary),
                    )
                    .child(card(rows.map(|r| div().child(r)), cx)),
            )
            .into_any_element()
    }

    fn render_inspect(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        match &self.info {
            Load::Loading => loading("image-inspect-loading"),
            Load::Failed(e) => div().p_3().text_color(t.red).child(e.clone()).into_any_element(),
            Load::Loaded(_) => json_view::render(&self.inspect_json, &self.inspect_list, cx),
        }
    }
}

/// A layer row that opens a popover. The row keeps the hover fill while
/// its popover is open.
#[derive(IntoElement)]
struct LayerRow {
    row: Stateful<Div>,
    open: bool,
}

impl Selectable for LayerRow {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl RenderOnce for LayerRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        self.row.when(self.open, |d| d.bg(native::hover(cx)))
    }
}

/// Splits a step into its Dockerfile instruction and the rest, so
/// "RUN apk add curl" gives ("RUN", "apk add curl"). A step with no
/// instruction gives `None` and all the text.
fn split_instruction(command: &str) -> (Option<&str>, &str) {
    let (word, rest) = command.split_once(char::is_whitespace).unwrap_or((command, ""));
    match word {
        "ADD" | "ARG" | "CMD" | "COPY" | "ENTRYPOINT" | "ENV" | "EXPOSE" | "HEALTHCHECK" | "LABEL" | "MAINTAINER"
        | "ONBUILD" | "RUN" | "SHELL" | "STOPSIGNAL" | "USER" | "VOLUME" | "WORKDIR" => (Some(word), rest.trim_start()),
        _ => (None, command),
    }
}

/// The color of an instruction badge. Steps that add files, run
/// commands, set the start command, and set metadata get different colors.
fn instruction_color(keyword: &str, cx: &App) -> Hsla {
    let t = cx.theme();
    match keyword {
        "RUN" => t.blue,
        "ADD" | "COPY" => t.green,
        "CMD" | "ENTRYPOINT" => t.magenta,
        "ARG" | "ENV" | "LABEL" => t.cyan,
        _ => t.muted_foreground,
    }
}

impl Render for ImageDetail {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.nav.begin(window);
        let t = cx.theme().clone();
        let image = self.image(cx);

        let body = match self.tab {
            ImageTab::Info => self.render_info(cx),
            ImageTab::Layers => self.render_layers(cx),
            ImageTab::Inspect => self.render_inspect(cx),
        };
        let body = if self.tab_changes > 0 {
            div()
                .size_full()
                .child(body)
                .with_animation(
                    ("image-tab-fade", self.tab_changes),
                    Animation::new(ENTER_DURATION).with_easing(ease_out_quint()),
                    |d, t| d.opacity(t),
                )
                .into_any_element()
        } else {
            body
        };

        let header = image.as_ref().map(|image| {
            let in_use = self.containers.read(cx).set.rows.values().any(|c| c.image_id == image.id);
            let (title, tag) = if image.tags.is_empty() {
                ("Untagged image".to_string(), Some(image.short_id().to_string()))
            } else {
                let (repo, tag) = split_reference(&self.key);
                (repo.to_string(), tag.map(str::to_string))
            };
            let subtitle = [tag, Some(format_bytes(image.size)), Some(ago(image.created))]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            let reference = (!image.tags.is_empty()).then_some(self.key.as_str());
            h_flex()
                .px_5()
                .pt_3()
                .pb_5()
                .gap_3()
                .child(ImageLogo::new(reference, image.icon.as_deref()).size(px(48.)).dimmed(!in_use))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_size(px(text::TITLE3))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
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

        let labels: Vec<&'static str> = ImageTab::ALL.iter().map(|t| t.label()).collect();
        let selected = ImageTab::ALL.iter().position(|&tab| tab == self.tab).unwrap_or(0);
        let view = cx.weak_entity();
        let tabs = segmented_control(
            "image-tabs",
            labels,
            selected,
            Rc::new(move |ix, _, cx| {
                let _ = view.update(cx, |this, cx| this.set_tab(ImageTab::ALL[ix], cx));
            }),
            cx,
        );

        key_nav::detail_actions(v_flex(), cx)
            .track_focus(&self.nav.focus)
            .size_full()
            .border_l_1()
            .border_color(t.border)
            .bg(t.sidebar.opacity(0.5))
            .child(
                h_flex()
                    .h(px(TOOLBAR_HEIGHT + 8.))
                    .flex_shrink_0()
                    .px_3()
                    .child(drag_area("image-detail-drag-left").flex_1().h_full())
                    .child(tabs)
                    .child(drag_area("image-detail-drag-right").flex_1().h_full())
                    .child(
                        Button::new("close-image-detail")
                            .icon(Icon::new(IconName::X))
                            .custom(native::toolbar_button(cx))
                            .xsmall()
                            .tooltip("Close (Esc)")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ImageDetailEvent::Close))),
                    ),
            )
            .children(header)
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::split_instruction;

    #[test]
    fn steps_split_into_instructions() {
        assert_eq!(split_instruction("RUN apk add  curl"), (Some("RUN"), "apk add  curl"));
        assert_eq!(split_instruction("CMD [\"sh\"]"), (Some("CMD"), "[\"sh\"]"));
        assert_eq!(split_instruction("EXPOSE"), (Some("EXPOSE"), ""));
        assert_eq!(split_instruction("ADD\tfile:abc in /"), (Some("ADD"), "file:abc in /"));
        assert_eq!(split_instruction("apt-get update"), (None, "apt-get update"));
        assert_eq!(split_instruction(""), (None, ""));
    }
}
