//! The grouped, virtualized image list.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, Disableable as _, Icon, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use crate::docker::images::{self, ImageRow, split_reference};
use crate::docker::stats::format_bytes;
use crate::state::container_store::ContainerStore;
use crate::state::image_store::{
    ImageItem, ImageSort, ImageStore, ListOptions, UNTAGGED_GROUP, build_list, image_users,
};
use crate::ui::container_list::{
    CHILD_INSET, Entrance, GUTTER, ICON_SIZE, ROW_HEIGHT, ROW_INSET, ROW_TRAILING_INSET, Tree, divider_line, tree_lines,
};
use crate::ui::image_icon::ImageLogo;
use crate::ui::key_nav::{self, CursorList, Stop};
use crate::ui::native::MenuEntry;
use crate::ui::{
    ENTER_DURATION, SNAPPY_SPRING, TOOLBAR_HEIGHT, ago, delayed_reveal, drag_area, native, spinner, tabular_nums, text,
    toast,
};

actions!(
    image_list,
    [PullImage, RemoveSelected, FocusSearch, ClearSelection, ToggleUnusedOnly]
);

const CONTEXT: &str = "ImageList";
/// The list context, and the context of the keys that all lists share.
const KEY_CONTEXT: &str = "ImageList NavList";
const SIZE_WIDTH: f32 = 90.;
const ACTIONS_WIDTH: f32 = 40.;
/// The most containers that the subtitle of a row names.
const USERS_SHOWN: usize = 2;
const ROW_GROUP: &str = "image-row";
const HEADER_GROUP: &str = "image-group";
/// The prefix of the entrance key of a pull row. A reference cannot
/// contain a NUL character, so the key does not match a row.
const PULL_KEY: &str = "\0pull:";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-n", PullImage, Some(CONTEXT)),
        KeyBinding::new("cmd-backspace", RemoveSelected, Some(CONTEXT)),
        KeyBinding::new("cmd-f", FocusSearch, Some(CONTEXT)),
        KeyBinding::new("escape", ClearSelection, Some(CONTEXT)),
        KeyBinding::new("cmd-shift-u", ToggleUnusedOnly, Some(CONTEXT)),
    ]);
}

pub enum ImageListEvent {
    /// `pointer` is true when a click made the selection, and false for a key.
    Selected { key: Option<String>, pointer: bool },
    /// Show a container in the container list.
    RevealContainer(String),
}

/// The name of a row. A row in a group shows only its tag, because the
/// group shows the repository. An untagged image shows its short ID.
pub fn display_name(key: &str, grouped: bool, image: &ImageRow) -> String {
    if image.tags.is_empty() {
        return image.short_id().to_string();
    }
    match split_reference(key) {
        (_, Some(tag)) if grouped => tag.to_string(),
        _ => key.to_string(),
    }
}

/// True when Docker refuses to remove `key`: the image of the last tag, or
/// of an untagged image, has a container. A container keeps the image
/// also when it is stopped.
pub fn remove_blocked(image: &ImageRow, in_use: bool) -> bool {
    in_use && image.tags.len() <= 1
}

pub struct ImageList {
    store: Entity<ImageStore>,
    containers: Entity<ContainerStore>,
    search: Entity<InputState>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    collapsed: HashSet<String>,
    items: Rc<Vec<ImageItem>>,
    /// The ID and the name of the containers that use each image, by
    /// image ID, sorted by name. `rebuild` sets it, so the rows do not read
    /// all the containers.
    users: HashMap<String, Vec<(String, String)>>,
    /// The rows that come into the list now, with the entrance and an
    /// animation number. A row stays here for one `ENTER_DURATION` only.
    entering: HashMap<String, (Entrance, usize)>,
    entrances: usize,
    /// The row and pull keys at the last rebuild. It is `None` until the
    /// first full list arrives, so the first list does not fade in row by
    /// row.
    known: Option<HashSet<String>>,
    selected: Option<String>,
    /// The row with the hover. The pointer and the arrow keys move it.
    cursor: key_nav::ListCursor,
    /// An image to select when it comes into the list. A link from a
    /// container can ask for an image before the first list arrives.
    pending_reveal: Option<String>,
    search_open: bool,
    /// Show only the images that no container uses.
    unused_only: bool,
    sort: ImageSort,
    /// Extra space at the left of the toolbar. The sidebar button uses this
    /// space when the sidebar is collapsed.
    leading_inset: Pixels,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ImageListEvent> for ImageList {}

impl Focusable for ImageList {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ImageList {
    pub fn new(
        store: Entity<ImageStore>,
        containers: Entity<ContainerStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search images"));
        let subs = vec![
            cx.observe(&store, |this, _, cx| this.rebuild(cx)),
            // The containers tell which images are in use.
            cx.observe(&containers, |this, _, cx| this.rebuild(cx)),
            cx.subscribe(&search, |this, _, ev: &InputEvent, cx| {
                if let InputEvent::Change = ev {
                    this.rebuild(cx);
                }
            }),
        ];
        let mut this = ImageList {
            store,
            containers,
            search,
            focus: cx.focus_handle().tab_stop(true),
            scroll: UniformListScrollHandle::new(),
            // The untagged images are often old builds, so their group
            // starts collapsed.
            collapsed: HashSet::from([UNTAGGED_GROUP.to_string()]),
            items: Rc::new(Vec::new()),
            users: HashMap::new(),
            entering: HashMap::new(),
            entrances: 0,
            known: None,
            selected: None,
            cursor: key_nav::ListCursor::default(),
            pending_reveal: None,
            search_open: false,
            unused_only: false,
            sort: ImageSort::default(),
            leading_inset: px(0.),
            _subs: subs,
        };
        this.rebuild(cx);
        this
    }

    pub fn leading_inset(&self) -> Pixels {
        self.leading_inset
    }

    pub fn set_leading_inset(&mut self, inset: Pixels, cx: &mut Context<Self>) {
        self.leading_inset = inset;
        cx.notify();
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let filter = self.search.read(cx).value();
        let store = self.store.read(cx);
        let containers = self.containers.read(cx);
        let users = image_users(containers.set.rows.values());
        let options = ListOptions {
            filter: &filter,
            unused_only: self.unused_only,
            sort: self.sort,
            collapsed: &self.collapsed,
            users: &users,
        };
        self.items = Rc::new(build_list(store.rows.values(), store.pulls.keys(), &options));
        self.users = users
            .into_iter()
            .map(|(id, list)| (id.to_string(), list.into_iter().map(|c| (c.id.clone(), c.name.clone())).collect()))
            .collect();
        // A row or a pull that appears after the first list fades in. The
        // engine fade covers a new connection, so it starts again.
        let mut new = Vec::new();
        if store.is_loading() {
            self.known = None;
        } else {
            let keys: HashSet<String> = store
                .rows
                .values()
                .flat_map(|r| if r.tags.is_empty() { vec![r.id.clone()] } else { r.tags.clone() })
                .chain(store.pulls.keys().map(|r| format!("{PULL_KEY}{r}")))
                .collect();
            if let Some(known) = &self.known {
                new = keys.difference(known).cloned().collect();
            }
            self.known = Some(keys);
        }
        let gone = self.selected.as_ref().is_some_and(|sel| store.image(sel).is_none());
        if gone {
            self.select(None, false, cx);
        }
        self.enter(new, Entrance::New, cx);
        if let Some(key) = self.pending_reveal.take() {
            self.reveal(&key, cx);
        }
        cx.notify();
    }

    /// Plays an entrance on `keys`, and removes the mark when it ends.
    fn enter(&mut self, keys: Vec<String>, entrance: Entrance, cx: &mut Context<Self>) {
        if keys.is_empty() {
            return;
        }
        self.entrances += 1;
        let n = self.entrances;
        for key in keys {
            self.entering.insert(key, (entrance, n));
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ENTER_DURATION).await;
            let _ = this.update(cx, |this, _| this.entering.retain(|_, (_, m)| *m != n));
        })
        .detach();
    }

    fn select(&mut self, key: Option<String>, pointer: bool, cx: &mut Context<Self>) {
        if self.selected == key {
            return;
        }
        self.selected = key.clone();
        cx.emit(ImageListEvent::Selected { key, pointer });
        cx.notify();
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.select(None, true, cx);
    }

    /// Selects an image, and scrolls to it. A collapsed group of the image
    /// expands. `key` can be a reference or an image ID. The ID of a tagged
    /// image selects its first tag.
    pub fn reveal(&mut self, key: &str, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        let Some(image) = store.image(key) else {
            // The list is not loaded yet. `rebuild` tries again.
            if store.is_loading() {
                self.pending_reveal = Some(key.to_string());
            }
            return;
        };
        let key = match image.tags.first() {
            Some(first) if key == image.id => first.clone(),
            _ => key.to_string(),
        };
        let group = if image.tags.is_empty() { UNTAGGED_GROUP.to_string() } else { split_reference(&key).0.to_string() };
        // The filter would hide an image that a container uses.
        let hidden = self.unused_only && self.users.contains_key(&image.id);
        if self.collapsed.remove(&group) || hidden {
            self.unused_only &= !hidden;
            self.rebuild(cx);
        }
        let ix = self
            .items
            .iter()
            .position(|item| matches!(item, ImageItem::Row { key: row, .. } if *row == key));
        if let Some(ix) = ix {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
        self.select(Some(key), false, cx);
    }

    /// Asks for a reference, then pulls it. A reference that is not valid
    /// asks again, with the reason.
    fn request_pull(&mut self, value: String, problem: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let request = native::TextRequest {
            message: "Pull Image".into(),
            info: problem
                .unwrap_or("Type the name of an image, for example “nginx” or “ghcr.io/owner/app:v1”. With no tag, Docker pulls “latest”.")
                .into(),
            value,
            placeholder: "nginx:latest".into(),
            ok: "Pull".into(),
        };
        native::ask_text(request, window, cx, |this, reference, window, cx| {
            let reference = reference.trim().to_string();
            match images::check_reference(&reference) {
                Err(problem) => this.request_pull(reference, Some(problem), window, cx),
                Ok(()) => {
                    let reference = images::normalize_reference(&reference);
                    this.store.update(cx, |s, cx| s.pull(&reference, cx));
                    this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
            }
        });
    }

    fn toggle_group(&mut self, key: &str, cx: &mut Context<Self>) {
        let expanded = self.collapsed.remove(key);
        if !expanded {
            self.collapsed.insert(key.to_string());
        }
        self.rebuild(cx);
        // The members fade in below their group. A collapse is immediate,
        // so the rows below move up with no wait.
        if expanded {
            let members = self.group_members(key, cx);
            self.enter(members, Entrance::Expand, cx);
        }
    }

    fn users_of(&self, image_id: &str) -> &[(String, String)] {
        self.users.get(image_id).map_or(&[], Vec::as_slice)
    }

    /// Removes a tag or an image after the user confirms.
    pub fn request_remove(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(image) = self.store.read(cx).image(&key).cloned() else { return };
        let users = self.users_of(&image.id);
        if remove_blocked(&image, !users.is_empty()) {
            // Docker refuses the remove, so tell the user why before the call.
            let list = users.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", ");
            // The sheet has one button, so the answer is not necessary.
            drop(window.prompt(
                PromptLevel::Info,
                "The image is in use",
                Some(&format!("Remove the containers that use it first: {list}.")),
                &[PromptButton::ok("OK")],
                cx,
            ));
            return;
        }
        let (title, detail, button) = if image.tags.len() > 1 {
            let others: Vec<&str> = image.tags.iter().filter(|t| **t != key).map(String::as_str).collect();
            (
                format!("Remove the tag “{key}”?"),
                format!("The image stays, because it has other tags: {}.", others.join(", ")),
                "Remove Tag",
            )
        } else if image.tags.is_empty() {
            (
                format!("Remove the image {}?", image.short_id()),
                format!("Docker deletes the image ({}). You cannot undo this.", format_bytes(image.size)),
                "Remove",
            )
        } else {
            (
                format!("Remove “{key}”?"),
                format!(
                    "Docker deletes the image ({}). You can pull it again if it is in a registry.",
                    format_bytes(image.size)
                ),
                "Remove",
            )
        };
        let store = self.store.clone();
        let answer = window.prompt(
            PromptLevel::Warning,
            &title,
            Some(&detail),
            &[PromptButton::Cancel("Cancel".into()), PromptButton::new(button)],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(1) {
                store.update(cx, |s, cx| s.remove(&key, cx));
            }
        })
        .detach();
    }

    /// Removes all the images that no container uses, after the user
    /// confirms.
    fn request_prune(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.store.read(cx).rows.keys().filter(|id| !self.users.contains_key(*id)).count();
        if count == 0 {
            drop(window.prompt(
                PromptLevel::Info,
                "No unused images",
                Some("All the images are in use by a container."),
                &[PromptButton::ok("OK")],
                cx,
            ));
            return;
        }
        let noun = if count == 1 { "image" } else { "images" };
        let store = self.store.clone();
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Remove {count} unused {noun}?"),
            Some(&format!(
                "Docker deletes the {noun} that no container uses. This includes tagged images and images that you built. You cannot undo this."
            )),
            &[PromptButton::Cancel("Cancel".into()), PromptButton::new("Remove")],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(1) {
                store.update(cx, |s, cx| s.prune(cx));
            }
        })
        .detach();
    }

    fn toggle_unused_only(&mut self, cx: &mut Context<Self>) {
        self.unused_only = !self.unused_only;
        self.rebuild(cx);
    }

    fn toggle_sort(&mut self, cx: &mut Context<Self>) {
        self.sort = match self.sort {
            ImageSort::Name => ImageSort::Size,
            ImageSort::Size => ImageSort::Name,
        };
        self.rebuild(cx);
    }

    /// The keys of the rows of a group, sorted: the tags of a repository,
    /// or the IDs of the untagged images.
    fn group_members(&self, key: &str, cx: &App) -> Vec<String> {
        let rows = &self.store.read(cx).rows;
        let mut keys: Vec<String> = if key == UNTAGGED_GROUP {
            rows.values().filter(|r| r.tags.is_empty()).map(|r| r.id.clone()).collect()
        } else {
            rows.values()
                .flat_map(|r| &r.tags)
                .filter(|t| split_reference(t).0 == key)
                .cloned()
                .collect()
        };
        keys.sort();
        keys
    }

    /// The members of a group that a remove can delete: the tags and
    /// images that no container blocks.
    fn removable_members(&self, key: &str, cx: &App) -> Vec<String> {
        let store = self.store.read(cx);
        self.group_members(key, cx)
            .into_iter()
            .filter(|k| store.image(k).is_some_and(|i| !self.users.contains_key(&i.id)))
            .collect()
    }

    fn show_group_menu(&mut self, key: String, at: Option<Point<Pixels>>, window: &mut Window, cx: &mut Context<Self>) {
        let members = self.group_members(&key, cx);
        let removable = self.removable_members(&key, cx).len();
        let untagged = key == UNTAGGED_GROUP;
        let entries = vec![
            MenuEntry::Item { label: "Remove Unused Images…".into(), enabled: removable > 0 },
            MenuEntry::Separator,
            MenuEntry::item(if untagged { "Copy IDs" } else { "Copy Names" }),
        ];
        native::show_context_menu_at(entries, at, window, cx, move |this, ix, window, cx| match ix {
            0 => this.request_group_remove(key.clone(), window, cx),
            2 => {
                let noun = match (untagged, members.len() == 1) {
                    (true, true) => "image ID",
                    (true, false) => "image IDs",
                    (false, true) => "image name",
                    (false, false) => "image names",
                };
                toast::copy(members.join("\n"), format!("{} {noun}", members.len()), cx)
            }
            _ => {}
        });
    }

    /// Removes the members of a group that no container uses, after the
    /// user confirms.
    fn request_group_remove(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        let removable = self.removable_members(&key, cx);
        if removable.is_empty() {
            return;
        }
        let count = removable.len();
        let noun = if count == 1 { "image" } else { "images" };
        let title = if key == UNTAGGED_GROUP {
            format!("Remove {count} unused untagged {noun}?")
        } else {
            format!("Remove {count} unused {noun} of “{key}”?")
        };
        let store = self.store.clone();
        let answer = window.prompt(
            PromptLevel::Warning,
            &title,
            Some(&format!("Docker deletes the {noun} that no container uses. You cannot undo this.")),
            &[PromptButton::Cancel("Cancel".into()), PromptButton::new("Remove")],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(1) {
                store.update(cx, |s, cx| s.remove_many(&removable, cx));
            }
        })
        .detach();
    }

    fn show_row_menu(&mut self, key: String, at: Option<Point<Pixels>>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(image) = self.store.read(cx).image(&key).cloned() else {
            return;
        };
        let users = self.users_of(&image.id).to_vec();
        let pulling = self.store.read(cx).pulls.contains_key(&key);
        type Handler = Box<dyn Fn(&mut ImageList, &mut Window, &mut Context<ImageList>)>;
        let mut entries: Vec<(MenuEntry, Option<Handler>)> = Vec::new();
        for (id, container) in users.iter().cloned() {
            entries.push((
                MenuEntry::item(format!("Show Container “{container}”")),
                Some(Box::new(move |_, _, cx| cx.emit(ImageListEvent::RevealContainer(id.clone())))),
            ));
        }
        if !users.is_empty() {
            entries.push((MenuEntry::Separator, None));
        }
        let tagged = !image.tags.is_empty();
        if tagged {
            let reference = key.clone();
            entries.push((
                MenuEntry::Item { label: "Pull Again".into(), enabled: !pulling },
                Some(Box::new(move |this, _, cx| this.store.update(cx, |s, cx| s.pull(&reference, cx)))),
            ));
            entries.push((MenuEntry::Separator, None));
            let name = key.clone();
            entries.push((
                MenuEntry::item("Copy Name"),
                Some(Box::new(move |_, _, cx| toast::copy(name.clone(), name.clone(), cx))),
            ));
        }
        let id = image.id.clone();
        entries.push((
            MenuEntry::item("Copy ID"),
            Some(Box::new(move |_, _, cx| toast::copy(id.clone(), images::short_id(&id).to_string(), cx))),
        ));
        entries.push((MenuEntry::Separator, None));
        let label = if image.tags.len() > 1 { "Remove Tag…" } else { "Remove…" };
        entries.push((
            MenuEntry::Item { label: label.into(), enabled: !remove_blocked(&image, !users.is_empty()) },
            Some(Box::new(move |this, window, cx| this.request_remove(key.clone(), window, cx))),
        ));
        let (entries, handlers): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
        native::show_context_menu_at(entries, at, window, cx, move |this, ix, window, cx| {
            if let Some(Some(handler)) = handlers.get(ix) {
                handler(this, window, cx);
            }
        });
    }

    fn render_item(&self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let item = match &self.items[ix] {
            ImageItem::Pull { reference } => self.render_pull(ix, reference, cx),
            ImageItem::Group { key, total, unused, collapsed } => {
                self.render_group(ix, key, *total, *unused, *collapsed, cx)
            }
            ImageItem::Row { key, grouped } => self.render_row(ix, key, *grouped, window, cx),
        };
        // A line separates the pulls from the images, and the images from
        // the group of untagged images.
        let prev = ix.checked_sub(1).map(|i| &self.items[i]);
        let divider = match (&self.items[ix], prev) {
            (ImageItem::Pull { .. }, _) | (_, None) => false,
            (_, Some(ImageItem::Pull { .. })) => true,
            (ImageItem::Group { key, .. }, Some(_)) => key == UNTAGGED_GROUP,
            _ => false,
        };
        let next_is_child = matches!(self.items.get(ix + 1), Some(ImageItem::Row { grouped: true, .. }));
        let tree = match &self.items[ix] {
            ImageItem::Group { .. } if next_is_child => Some(Tree::Stem),
            ImageItem::Row { grouped: true, .. } if next_is_child => Some(Tree::Branch),
            ImageItem::Row { grouped: true, .. } => Some(Tree::Last),
            _ => None,
        };
        let line = cx.theme().muted_foreground.opacity(0.35);
        let entering = match &self.items[ix] {
            ImageItem::Row { key, .. } => self.entering.get(key).map(|e| (key.clone(), *e)),
            ImageItem::Pull { reference } => {
                let key = format!("{PULL_KEY}{reference}");
                self.entering.get(&key).map(|e| (key, *e))
            }
            ImageItem::Group { .. } => None,
        };
        div()
            .relative()
            .w_full()
            .h(px(ROW_HEIGHT))
            .pl(px(ROW_INSET))
            .pr(px(ROW_TRAILING_INSET))
            .py_px()
            .when(divider, |d| d.pt(px(4.)).child(divider_line(cx)))
            .when_some(tree, |d, tree| d.children(tree_lines(tree, line)))
            .child(item)
            .map(|d| match entering {
                Some((key, (entrance, n))) => d
                    .with_animation(
                        ElementId::named_usize(format!("enter-{key}"), n),
                        Animation::new(ENTER_DURATION).with_easing(ease_out_quint()),
                        move |d, t| {
                            let d = d.opacity(t);
                            if entrance == Entrance::Expand {
                                d.top(px(-4. * (1. - t)))
                            } else {
                                d
                            }
                        },
                    )
                    .into_any_element(),
                None => d.into_any_element(),
            })
    }

    fn render_pull(&self, ix: usize, reference: &str, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let Some(pull) = self.store.read(cx).pulls.get(reference) else {
            return div().h(px(ROW_HEIGHT)).into_any_element();
        };
        let fraction = pull.progress.fraction();
        let (current, total) = pull.progress.bytes();
        let status = match fraction {
            Some(f) if f >= 1. => "Extracting…".to_string(),
            _ if total > 0 => format!("Downloading · {} of {}", format_bytes(current), format_bytes(total)),
            _ => pull.progress.status.clone().unwrap_or_else(|| "Connecting to the registry…".into()),
        };
        let progress = div()
            .w(px(SIZE_WIDTH))
            .flex_shrink_0()
            .flex()
            .justify_end()
            .child(match fraction {
                Some(f) => div()
                    .w_full()
                    .h(px(4.))
                    .rounded_full()
                    .bg(t.muted)
                    .overflow_hidden()
                    .child(
                        div()
                            .h_full()
                            .rounded_full()
                            .bg(native::accent(cx))
                            // The bar moves on a spring, so the jumps
                            // between the engine messages are smooth.
                            .with_spring(
                                ElementId::Name(format!("pull-bar-{reference}").into()),
                                SpringAnimation::new(SNAPPY_SPRING).to(f),
                                |d, f: f32| d.w(relative(f.clamp(0., 1.))),
                            ),
                    )
                    .into_any_element(),
                None => spinner().xsmall().into_any_element(),
            });
        let cancel = reference.to_string();
        h_flex()
            .id(("pull", ix))
            .h_full()
            .flex_1()
            .pl(px(GUTTER))
            .pr_2()
            .gap_3()
            .text_size(px(text::BODY))
            .rounded(px(8.))
            .child(ImageLogo::new(Some(reference), None).size(px(ICON_SIZE)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(div().truncate().font_weight(FontWeight::MEDIUM).child(reference.to_string()))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(text::CALLOUT))
                            .font_features(tabular_nums())
                            .text_color(t.muted_foreground)
                            .child(status),
                    ),
            )
            .child(progress)
            .child(
                h_flex().w(px(ACTIONS_WIDTH)).flex_shrink_0().justify_end().child(
                    Button::new(("cancel-pull", ix))
                        .icon(IconName::X)
                        .ghost()
                        .xsmall()
                        .tab_stop(false)
                        .tooltip("Stop the pull")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.store.update(cx, |s, cx| s.cancel_pull(&cancel, cx));
                        })),
                ),
            )
            .into_any_element()
    }

    fn render_group(
        &self,
        ix: usize,
        key: &str,
        total: usize,
        unused: usize,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = cx.theme();
        let untagged = key == UNTAGGED_GROUP;
        let store = self.store.read(cx);
        let members: Vec<&ImageRow> = store
            .rows
            .values()
            .filter(|r| {
                if untagged {
                    r.tags.is_empty()
                } else {
                    r.tags.iter().any(|t| split_reference(t).0 == key)
                }
            })
            .collect();
        let size: u64 = members.iter().map(|r| r.size).sum();
        let noun = match (untagged, total == 1) {
            (true, true) => "image",
            (true, false) => "images",
            (false, true) => "tag",
            (false, false) => "tags",
        };
        let mut summary = format!("{total} {noun}");
        if unused == total {
            summary.push_str(" · none in use");
        } else if unused > 0 {
            summary.push_str(&format!(" · {unused} not in use"));
        }
        // The logo of the repository comes from its first tag.
        let logo_ref = (!untagged).then(|| members.iter().flat_map(|r| &r.tags).find(|t| split_reference(t).0 == key)).flatten();
        let icon = members.iter().find_map(|r| r.icon.clone());
        let toggle = key.to_string();
        let menu_key = key.to_string();
        let cursor = self.cursor.is_group(key);
        let hover_key = key.to_string();
        h_flex()
            .id(("group", ix))
            .group(HEADER_GROUP)
            .relative()
            .size_full()
            .pr_2()
            .gap_3()
            .rounded(px(8.))
            // The pointer and the arrow keys move the same hover.
            .when(cursor, |d| d.bg(native::hover(cx)).child(key_nav::track_bounds(self.cursor.bounds.clone())))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                key_nav::hover_row(this, key_nav::Cursor::Group(hover_key.clone()), *hovered, cx)
            }))
            .child(
                div().w(px(GUTTER - 12.)).flex().justify_end().child(
                    Icon::new(IconName::ChevronRight)
                        .xsmall()
                        .text_color(t.muted_foreground)
                        .with_spring(
                            "chevron",
                            SpringAnimation::new(SNAPPY_SPRING).to(if collapsed { 0. } else { 1. }),
                            |icon, turn: f32| icon.rotate(radians(turn * std::f32::consts::FRAC_PI_2)),
                        ),
                ),
            )
            .child(
                ImageLogo::new(logo_ref.map(String::as_str), icon.as_deref())
                    .size(px(ICON_SIZE))
                    .dimmed(unused == total),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(text::BODY))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if untagged { "Untagged Images".to_string() } else { key.to_string() }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(text::CALLOUT))
                            .font_features(tabular_nums())
                            .text_color(t.muted_foreground)
                            .child(summary),
                    ),
            )
            .child(size_cell(Some(size), cx))
            .child(div().w(px(ACTIONS_WIDTH)).flex_shrink_0())
            .on_click(cx.listener(move |this, _, window, cx| {
                this.focus.focus(window, cx);
                this.toggle_group(&toggle, cx)
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, window, cx| this.show_group_menu(menu_key.clone(), None, window, cx)),
            )
            .into_any_element()
    }

    fn render_row(&self, ix: usize, key: &str, grouped: bool, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let store = self.store.read(cx);
        let Some(image) = store.image(key) else {
            return div().h(px(ROW_HEIGHT)).into_any_element();
        };
        let t = cx.theme();
        let removing = store.removing.contains(key);
        let selected = self.selected.as_deref() == Some(key);
        let hovered = self.cursor.is_row(key);
        let emphasized = window.is_window_active() && self.focus.contains_focused(window, cx);
        let on_accent = selected && emphasized && native::has_emphasized_selection(cx);
        let secondary = if on_accent { gpui::white().opacity(0.8) } else { t.muted_foreground };
        let users = self.users_of(&image.id);
        let in_use = !users.is_empty();
        let mut subtitle = if in_use {
            let more = users.len().saturating_sub(USERS_SHOWN);
            let names: Vec<&str> = users[..users.len().min(USERS_SHOWN)].iter().map(|(_, n)| n.as_str()).collect();
            let mut s = format!("Used by {}", names.join(", "));
            if more > 0 {
                s.push_str(&format!(" and {more} more"));
            }
            s
        } else {
            "Not in use".to_string()
        };
        subtitle.push_str(&format!(" · {}", ago(image.created)));
        let blocked = remove_blocked(image, in_use);

        let actions = if removing {
            h_flex()
                .gap_1()
                .text_size(px(text::CALLOUT))
                .text_color(t.muted_foreground)
                .child(delayed_reveal(("removing", ix), div().child(spinner().xsmall())))
                .into_any_element()
        } else {
            let remove_key = key.to_string();
            h_flex()
                .when(!selected && !hovered, |d| d.invisible())
                .child(
                    Button::new(("remove", ix))
                        .icon(IconName::Trash)
                        .when(selected, |b| b.outline())
                        .when(!selected, |b| b.ghost())
                        .xsmall()
                        // Command-Delete and the context menu remove the
                        // row, so Tab does not stop on each row.
                        .tab_stop(false)
                        .disabled(blocked)
                        .tooltip(if blocked { "In use. Remove the containers first." } else { "Remove (⌘⌫)" })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.request_remove(remove_key.clone(), window, cx);
                        })),
                )
                .into_any_element()
        };

        let reference = (!image.tags.is_empty()).then_some(key);
        let row_key = key.to_string();
        let hover_key = key.to_string();
        h_flex()
            .id(("row", ix))
            .group(ROW_GROUP)
            .relative()
            .h_full()
            .flex_1()
            .map(|d| if grouped { d.ml(px(CHILD_INSET - ROW_INSET)).pl(px(12.)) } else { d.pl(px(GUTTER)) })
            .pr_2()
            .gap_3()
            .text_size(px(text::BODY))
            .rounded(px(8.))
            .map(|d| {
                if selected {
                    d.bg(native::selection(emphasized, cx))
                } else if hovered {
                    d.bg(native::hover(cx))
                } else {
                    d
                }
            })
            // The context menu key opens the menu of the hovered row, or of
            // the selected row when no row has the hover.
            .when(hovered || (selected && self.cursor.at.is_none()), |d| {
                d.child(key_nav::track_bounds(self.cursor.bounds.clone()))
            })
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                key_nav::hover_row(this, key_nav::Cursor::Row(hover_key.clone()), *hovered, cx)
            }))
            .when(on_accent, |d| d.text_color(gpui::white()))
            .when(removing, |d| d.opacity(0.7))
            .child(
                ImageLogo::new(reference, image.icon.as_deref())
                    .size(px(ICON_SIZE))
                    .dimmed(!in_use && !on_accent),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .when(image.tags.is_empty(), |d| d.font_family(t.mono_font_family.clone()))
                            .when(!in_use, |d| d.text_color(secondary))
                            .child(display_name(key, grouped, image)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(text::CALLOUT))
                            .text_color(secondary)
                            .child(subtitle),
                    ),
            )
            .child(size_cell(Some(image.size), cx).when(on_accent, |d| d.text_color(secondary)))
            .child(h_flex().w(px(ACTIONS_WIDTH)).flex_shrink_0().justify_end().child(actions))
            .on_click({
                let row_key = row_key.clone();
                cx.listener(move |this, _, window, cx| {
                    this.focus.focus(window, cx);
                    this.select(Some(row_key.clone()), true, cx);
                })
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, window, cx| {
                    this.focus.focus(window, cx);
                    this.select(Some(row_key.clone()), true, cx);
                    this.show_row_menu(row_key.clone(), None, window, cx);
                }),
            )
            .into_any_element()
    }

    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = !self.search_open;
        if self.search_open {
            self.search.update(cx, |s, cx| s.focus(window, cx));
        } else {
            // A hidden search must not filter the list.
            self.search.update(cx, |s, cx| s.set_value("", window, cx));
            self.focus.focus(window, cx);
        }
        cx.notify();
    }
}

/// The size column.
fn size_cell(size: Option<u64>, cx: &App) -> Div {
    div()
        .w(px(SIZE_WIDTH))
        .flex_shrink_0()
        .flex()
        .justify_end()
        .text_size(px(text::CALLOUT))
        .font_features(tabular_nums())
        .text_color(cx.theme().muted_foreground)
        .when_some(size, |d, size| d.child(format_bytes(size)))
}

impl CursorList for ImageList {
    fn stops(&self) -> Vec<Stop> {
        key_nav::grouped_stops(self.items.iter(), |item| match item {
            ImageItem::Group { key, collapsed, .. } => Some(Ok((key.as_str(), *collapsed))),
            ImageItem::Row { key, grouped } => Some(Err((key.as_str(), *grouped))),
                ImageItem::Pull { .. } => None,
        })
    }

    fn list_cursor(&self) -> &key_nav::ListCursor {
        &self.cursor
    }

    fn list_cursor_mut(&mut self) -> &mut key_nav::ListCursor {
        &mut self.cursor
    }

    fn selected_key(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    fn select_key(&mut self, key: String, cx: &mut Context<Self>) {
        self.select(Some(key), false, cx);
    }

    fn scroll(&self) -> &UniformListScrollHandle {
        &self.scroll
    }

    fn toggle_group_at(&mut self, key: &str, cx: &mut Context<Self>) {
        self.toggle_group(key, cx);
    }

    fn show_menu_at(&mut self, stop: &Stop, at: Option<Point<Pixels>>, window: &mut Window, cx: &mut Context<Self>) {
        match stop {
            Stop::Group { key, .. } => self.show_group_menu(key.clone(), at, window, cx),
            Stop::Row { key, .. } => self.show_row_menu(key.clone(), at, window, cx),
            Stop::Skip => {}
        }
    }
}

impl Render for ImageList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        let store = self.store.read(cx);
        let total = store.rows.len();
        let loading = store.is_loading();
        let pruning = store.pruning;
        let pulls = store.pulls.len();
        let in_use = store.rows.keys().filter(|id| self.users.contains_key(*id)).count();
        let count = self.items.len();
        let mut summary = format!("{total} {}", if total == 1 { "image" } else { "images" });
        if total > 0 {
            summary.push_str(&format!(" · {in_use} in use"));
        }
        if pulls > 0 {
            summary.push_str(&format!(" · {pulls} {}", if pulls == 1 { "pull" } else { "pulls" }));
        }
        if self.unused_only {
            summary.push_str(" · in use hidden");
        }

        let body = if count == 0 {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(t.muted_foreground)
                .child(if loading {
                    delayed_reveal("images-loading", div().child(spinner())).into_any_element()
                } else if total == 0 {
                    div().child("No images").into_any_element()
                } else if self.unused_only && self.search.read(cx).value().trim().is_empty() {
                    div().child("All the images are in use").into_any_element()
                } else {
                    div().child("No images match the search").into_any_element()
                })
                .into_any_element()
        } else {
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    uniform_list(
                        "images",
                        count,
                        cx.processor(|this, range: Range<usize>, window, cx| {
                            range.map(|ix| this.render_item(ix, window, cx)).collect::<Vec<_>>()
                        }),
                    )
                    .size_full()
                    .track_scroll(&self.scroll),
                )
                .child(Scrollbar::vertical(&self.scroll))
                .into_any_element()
        };

        let root = key_nav::list_actions(v_flex(), cx);
        root.key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .on_action(cx.listener(|this, _: &RemoveSelected, window, cx| {
                if let Some(key) = this.selected.clone() {
                    this.request_remove(key, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ClearSelection, _, cx| {
                this.cursor.at = None;
                this.select(None, false, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &PullImage, window, cx| this.request_pull(String::new(), None, window, cx)))
            .on_action(cx.listener(|this, _: &ToggleUnusedOnly, _, cx| this.toggle_unused_only(cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.search_open = true;
                this.search.update(cx, |s, cx| s.focus(window, cx));
                cx.notify();
            }))
            .child(
                h_flex()
                    .h(px(TOOLBAR_HEIGHT + 8.))
                    .flex_shrink_0()
                    .px_5()
                    .pl(px(20.) + self.leading_inset)
                    .gap_1()
                    .border_b_1()
                    .border_color(t.border)
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(text::TITLE3))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Images"),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(text::CALLOUT))
                                    .font_features(tabular_nums())
                                    .text_color(t.muted_foreground)
                                    .child(summary),
                            ),
                    )
                    .child(drag_area("image-list-drag").flex_1().h_full())
                    .when(self.search_open, |d| {
                        d.child(
                            div().w(px(220.)).min_w(px(120.)).mr_1().child(
                                Input::new(&self.search)
                                    .small()
                                    .prefix(Icon::new(IconName::Search).small())
                                    .cleanable(true),
                            ),
                        )
                    })
                    .child(
                        Button::new("pull-image")
                            .icon(Icon::new(IconName::Download))
                            .custom(native::toolbar_button(cx))
                            .small()
                            .tooltip("Pull an image (⌘N)")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.request_pull(String::new(), None, window, cx)
                            })),
                    )
                    .child(
                        Button::new("prune-images")
                            .map(|b| if pruning { b.icon(spinner()) } else { b.icon(IconName::BrushCleaning) })
                            .custom(native::toolbar_button(cx))
                            .small()
                            .disabled(pruning || total == 0)
                            .tooltip("Remove unused images…")
                            .on_click(cx.listener(|this, _, window, cx| this.request_prune(window, cx))),
                    )
                    .child(
                        Button::new("sort-by-size")
                            .icon(Icon::new(IconName::ArrowDownWideNarrow))
                            .custom(native::toolbar_button(cx))
                            .small()
                            .selected(self.sort == ImageSort::Size)
                            .tooltip("Sort by size, the largest first")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_sort(cx))),
                    )
                    .child(
                        Button::new("unused-only")
                            .icon(Icon::new(IconName::ListFilter))
                            .custom(native::toolbar_button(cx))
                            .small()
                            .selected(self.unused_only)
                            .tooltip("Show unused images only (⇧⌘U)")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_unused_only(cx))),
                    )
                    .child(
                        Button::new("toggle-search")
                            .icon(Icon::new(IconName::Search))
                            .custom(native::toolbar_button(cx))
                            .small()
                            .selected(self.search_open)
                            .tooltip("Search (⌘F)")
                            .on_click(cx.listener(|this, _, window, cx| this.toggle_search(window, cx))),
                    ),
            )
            .child(div().h_2().flex_shrink_0())
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::{display_name, remove_blocked};
    use crate::docker::images::ImageRow;

    fn image(tags: &[&str]) -> ImageRow {
        ImageRow {
            id: "sha256:858f009f9709ce576feb".into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            digests: Vec::new(),
            created: 0,
            size: 0,
            icon: None,
        }
    }

    #[test]
    fn names_show_the_tag_in_a_group() {
        let tagged = image(&["node:24-alpine"]);
        assert_eq!(display_name("node:24-alpine", true, &tagged), "24-alpine");
        assert_eq!(display_name("node:24-alpine", false, &tagged), "node:24-alpine");
        let untagged = image(&[]);
        assert_eq!(display_name(&untagged.id, true, &untagged), "858f009f9709");
    }

    #[test]
    fn a_container_blocks_only_the_last_tag() {
        assert!(remove_blocked(&image(&["redis:7"]), true));
        assert!(remove_blocked(&image(&[]), true));
        assert!(!remove_blocked(&image(&["redis:7", "redis:latest"]), true));
        assert!(!remove_blocked(&image(&["redis:7"]), false));
    }
}
