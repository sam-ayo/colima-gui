//! Keyboard control for all the areas of the window: the sidebar, the
//! lists, and the detail panes.
//!
//! The arrow keys move a keyboard cursor in the focused area. The cursor
//! has the same fill as a row below the pointer. Return or Space does the
//! action of a click on the row. Tab and Shift-Tab move the focus through
//! the areas and the buttons.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::ActiveTheme as _;
use gpui_component::button::Button;

use super::native;

actions!(
    key_nav,
    [
        MoveUp,
        MoveDown,
        MoveFirst,
        MoveLast,
        PageUp,
        PageDown,
        MoveLeft,
        MoveRight,
        /// Does the action of a click on the row at the cursor.
        Activate,
        /// Expands or collapses the group at the cursor.
        Toggle,
        /// Shows the context menu of the row at the cursor.
        ShowMenu,
        /// Goes back one level: from a field to its pane, or from a pane to
        /// the list.
        Back,
        /// Shows the enclosing folder in a file list.
        Parent,
        FocusNext,
        FocusPrev,
        FocusSidebar,
        FocusList,
        FocusDetail,
        NextTab,
        PrevTab,
    ]
);

/// The key context of the sidebar.
pub const SIDEBAR: &str = "Sidebar";
/// The key context that the container, volume, and image lists add.
pub const LIST: &str = "NavList";
/// The key context of the detail panes.
pub const DETAIL: &str = "DetailPane";

/// The number of rows that Page Up and Page Down move in a list.
pub const PAGE_ROWS: isize = 10;
/// The pixels that one arrow key scrolls in a log or JSON view.
pub const LINE_SCROLL: f32 = 18. * 3.;
/// The part of the visible height that Page Up and Page Down scroll in a
/// view of text. The rest stays on screen, so the reader keeps the place.
const PAGE_FRACTION: f32 = 0.9;
/// The space that a revealed row keeps from the edge of its scroll area.
const REVEAL_MARGIN: f32 = 12.;

pub fn bind_keys(cx: &mut App) {
    // An area that has a text field in it lets the field use the keys.
    let list = format!("{LIST} && !Input");
    let detail = format!("{DETAIL} && !Input");
    let mut bindings = Vec::new();
    for context in [SIDEBAR, list.as_str(), detail.as_str()] {
        let ctx = Some(context);
        bindings.extend([
            KeyBinding::new("up", MoveUp, ctx),
            KeyBinding::new("down", MoveDown, ctx),
            KeyBinding::new("home", MoveFirst, ctx),
            KeyBinding::new("end", MoveLast, ctx),
            KeyBinding::new("pageup", PageUp, ctx),
            KeyBinding::new("pagedown", PageDown, ctx),
            KeyBinding::new("left", MoveLeft, ctx),
            KeyBinding::new("right", MoveRight, ctx),
            KeyBinding::new("enter", Activate, ctx),
            KeyBinding::new("shift-f10", ShowMenu, ctx),
            KeyBinding::new("ctrl-enter", ShowMenu, ctx),
        ]);
    }
    bindings.extend([
        KeyBinding::new("cmd-up", MoveFirst, Some(SIDEBAR)),
        KeyBinding::new("cmd-down", MoveLast, Some(SIDEBAR)),
        KeyBinding::new("space", Activate, Some(SIDEBAR)),
        KeyBinding::new("escape", FocusList, Some(SIDEBAR)),
        KeyBinding::new("cmd-up", MoveFirst, Some(list.as_str())),
        KeyBinding::new("cmd-down", MoveLast, Some(list.as_str())),
        KeyBinding::new("space", Toggle, Some(list.as_str())),
        // Finder uses Command-Up Arrow for the enclosing folder.
        KeyBinding::new("cmd-up", Parent, Some(detail.as_str())),
        KeyBinding::new("cmd-down", MoveLast, Some(detail.as_str())),
        KeyBinding::new("space", Activate, Some(detail.as_str())),
        // Escape in a field of the pane goes back to the pane.
        KeyBinding::new("escape", Back, Some(DETAIL)),
        // A single-line text field passes Tab on, so Tab also leaves a field.
        KeyBinding::new("tab", FocusNext, None),
        KeyBinding::new("shift-tab", FocusPrev, None),
        // The tabs of the detail pane, as in Safari and Xcode.
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PrevTab, None),
        KeyBinding::new("cmd-shift-]", NextTab, None),
        KeyBinding::new("cmd-shift-[", PrevTab, None),
    ]);
    cx.bind_keys(bindings);
}

/// A move of a keyboard cursor.
#[derive(Clone, Copy)]
pub enum Move {
    By(isize),
    First,
    Last,
}

impl Move {
    /// The position after the move in `count` positions. A cursor with no
    /// position goes to the first position on a move down, and to the last
    /// position on a move up.
    pub fn apply(self, current: Option<usize>, count: usize) -> Option<usize> {
        if count == 0 {
            return None;
        }
        let last = count as isize - 1;
        Some(match (self, current) {
            (Move::First, _) => 0,
            (Move::Last, _) => count - 1,
            (Move::By(d), None) if d > 0 => 0,
            (Move::By(_), None) => count - 1,
            (Move::By(d), Some(i)) => (i as isize + d).clamp(0, last) as usize,
        })
    }
}

/// A position of the keyboard cursor in a grouped list.
pub enum Stop {
    /// A group row. `key` is the group key.
    Group { key: String, collapsed: bool },
    /// A row. `key` is the row key, and `group` is the key of its group.
    Row { key: String, group: Option<String> },
    /// A row that the cursor skips, for example a pull that runs.
    Skip,
}

impl Stop {
    pub fn is_skip(&self) -> bool {
        matches!(self, Stop::Skip)
    }

    /// The cursor on this row.
    fn cursor(&self) -> Option<Cursor> {
        match self {
            Stop::Group { key, .. } => Some(Cursor::Group(key.clone())),
            Stop::Row { key, .. } => Some(Cursor::Row(key.clone())),
            Stop::Skip => None,
        }
    }
}

/// The row of a grouped list that has the hover. The pointer and the arrow
/// keys move it. It is not the selection: Return selects the row.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Cursor {
    Group(String),
    Row(String),
}

/// The hover of a grouped list.
#[derive(Default)]
pub struct ListCursor {
    pub at: Option<Cursor>,
    /// True when a key moved the hover last. The pointer then does not
    /// remove it when it leaves the row.
    pub keyed: bool,
    /// The bounds of the hovered row, for a context menu that a key opens.
    pub bounds: BoundsCell,
}

impl ListCursor {
    pub fn is_row(&self, key: &str) -> bool {
        matches!(&self.at, Some(Cursor::Row(k)) if k == key)
    }

    pub fn is_group(&self, key: &str) -> bool {
        matches!(&self.at, Some(Cursor::Group(k)) if k == key)
    }

    /// The group that a key put the hover on. The list actions then apply
    /// to the group.
    pub fn keyed_group(&self) -> Option<&str> {
        match &self.at {
            Some(Cursor::Group(k)) if self.keyed => Some(k),
            _ => None,
        }
    }

    /// Follows the pointer. A row that the pointer enters gets the hover.
    /// A row that the pointer leaves loses it, unless a key moved it there.
    /// Returns true when the hover changed.
    pub fn hover(&mut self, cursor: Cursor, hovered: bool) -> bool {
        if hovered {
            let changed = self.at.as_ref() != Some(&cursor) || self.keyed;
            self.at = Some(cursor);
            self.keyed = false;
            changed
        } else if self.at.as_ref() == Some(&cursor) && !self.keyed {
            self.at = None;
            true
        } else {
            false
        }
    }
}

/// Finds the position of `cursor` in `stops`. With no cursor, the selected
/// row is the start position.
pub fn cursor_position(stops: &[Stop], cursor: Option<&Cursor>, selected: Option<&str>) -> Option<usize> {
    let find = |c: &Cursor| stops.iter().position(|s| s.cursor().as_ref() == Some(c));
    match cursor {
        Some(c) => find(c),
        None => selected.and_then(|key| find(&Cursor::Row(key.to_string()))),
    }
}

/// Moves a cursor through `stops`, and skips the rows that take no cursor.
/// The result is an index in `stops`.
pub fn move_in(stops: &[Stop], current: Option<usize>, m: Move) -> Option<usize> {
    let targets: Vec<usize> = (0..stops.len()).filter(|&i| !stops[i].is_skip()).collect();
    let at = current.and_then(|c| targets.iter().position(|&i| i == c));
    m.apply(at, targets.len()).map(|i| targets[i])
}

/// A grouped list with a hover that the arrow keys move. The arrow keys do
/// not change the selection. Return selects the hovered row.
pub trait CursorList: Sized + 'static {
    /// The rows of the list, in order.
    fn stops(&self) -> Vec<Stop>;
    fn list_cursor(&self) -> &ListCursor;
    fn list_cursor_mut(&mut self) -> &mut ListCursor;
    fn selected_key(&self) -> Option<&str>;
    /// Selects a row, as a key does, so the detail pane opens immediately.
    fn select_key(&mut self, key: String, cx: &mut Context<Self>);
    fn scroll(&self) -> &UniformListScrollHandle;
    fn toggle_group_at(&mut self, key: &str, cx: &mut Context<Self>);
    /// Shows the context menu of a row. `at` is the position in the window,
    /// or `None` for the pointer position.
    fn show_menu_at(&mut self, stop: &Stop, at: Option<Point<Pixels>>, window: &mut Window, cx: &mut Context<Self>);
}

/// Gives the hover to a row or takes it away, as the pointer enters or
/// leaves the row.
pub fn hover_row<V: CursorList>(this: &mut V, cursor: Cursor, hovered: bool, cx: &mut Context<V>) {
    if this.list_cursor_mut().hover(cursor, hovered) {
        cx.notify();
    }
}

fn list_position<V: CursorList>(this: &V) -> (Vec<Stop>, Option<usize>) {
    let stops = this.stops();
    let current = cursor_position(&stops, this.list_cursor().at.as_ref(), this.selected_key());
    (stops, current)
}

fn put_cursor<V: CursorList>(this: &mut V, stops: &[Stop], ix: usize, cx: &mut Context<V>) {
    this.scroll().scroll_to_item(ix, ScrollStrategy::Nearest);
    let cursor = this.list_cursor_mut();
    cursor.at = stops[ix].cursor();
    cursor.keyed = true;
    cx.notify();
}

fn list_move<V: CursorList>(this: &mut V, m: Move, cx: &mut Context<V>) {
    let (stops, current) = list_position(this);
    if let Some(ix) = move_in(&stops, current, m) {
        put_cursor(this, &stops, ix, cx);
    }
}

/// Left Arrow collapses the hovered group, or goes from a row to its
/// group, as in the Finder. Out of a group, it goes to the sidebar.
fn list_left<V: CursorList>(this: &mut V, window: &mut Window, cx: &mut Context<V>) {
    let (stops, current) = list_position(this);
    match current.map(|i| &stops[i]) {
        Some(Stop::Group { key, collapsed: false }) => this.toggle_group_at(&key.clone(), cx),
        Some(Stop::Row { group: Some(group), .. }) => {
            let group = Cursor::Group(group.clone());
            if let Some(ix) = cursor_position(&stops, Some(&group), None) {
                put_cursor(this, &stops, ix, cx);
            }
        }
        _ => window.dispatch_action(Box::new(FocusSidebar), cx),
    }
}

/// Right Arrow expands the hovered group, or goes into it. On a row, it
/// selects the row and goes to the detail pane.
fn list_right<V: CursorList>(this: &mut V, window: &mut Window, cx: &mut Context<V>) {
    let (stops, current) = list_position(this);
    match current.map(|i| &stops[i]) {
        Some(Stop::Group { key, collapsed: true }) => this.toggle_group_at(&key.clone(), cx),
        Some(Stop::Group { .. }) => list_move(this, Move::By(1), cx),
        Some(Stop::Row { key, .. }) => {
            this.select_key(key.clone(), cx);
            window.dispatch_action(Box::new(FocusDetail), cx);
        }
        _ => {}
    }
}

/// Return expands or collapses a group, and selects a row. Return on the
/// selected row goes to the detail pane.
fn list_activate<V: CursorList>(this: &mut V, window: &mut Window, cx: &mut Context<V>) {
    let (stops, current) = list_position(this);
    match current.map(|i| &stops[i]) {
        Some(Stop::Group { key, .. }) => this.toggle_group_at(&key.clone(), cx),
        Some(Stop::Row { key, .. }) if this.selected_key() == Some(key.as_str()) => {
            window.dispatch_action(Box::new(FocusDetail), cx)
        }
        Some(Stop::Row { key, .. }) => this.select_key(key.clone(), cx),
        _ => {}
    }
}

/// Space expands or collapses a group, and selects a row.
fn list_toggle<V: CursorList>(this: &mut V, cx: &mut Context<V>) {
    let (stops, current) = list_position(this);
    match current.map(|i| &stops[i]) {
        Some(Stop::Group { key, .. }) => this.toggle_group_at(&key.clone(), cx),
        Some(Stop::Row { key, .. }) => this.select_key(key.clone(), cx),
        _ => {}
    }
}

fn list_menu<V: CursorList>(this: &mut V, window: &mut Window, cx: &mut Context<V>) {
    let (stops, current) = list_position(this);
    if let Some(ix) = current {
        let at = menu_point(this.list_cursor().bounds.get());
        this.show_menu_at(&stops[ix], at, window, cx);
    }
}

/// Adds the keyboard actions of a grouped list to its root element.
pub fn list_actions<V: CursorList>(el: Div, cx: &Context<V>) -> Div {
    el.on_action(cx.listener(|this, _: &MoveUp, _, cx| list_move(this, Move::By(-1), cx)))
        .on_action(cx.listener(|this, _: &MoveDown, _, cx| list_move(this, Move::By(1), cx)))
        .on_action(cx.listener(|this, _: &MoveFirst, _, cx| list_move(this, Move::First, cx)))
        .on_action(cx.listener(|this, _: &MoveLast, _, cx| list_move(this, Move::Last, cx)))
        .on_action(cx.listener(|this, _: &PageUp, _, cx| list_move(this, Move::By(-PAGE_ROWS), cx)))
        .on_action(cx.listener(|this, _: &PageDown, _, cx| list_move(this, Move::By(PAGE_ROWS), cx)))
        .on_action(cx.listener(|this, _: &MoveLeft, window, cx| list_left(this, window, cx)))
        .on_action(cx.listener(|this, _: &MoveRight, window, cx| list_right(this, window, cx)))
        .on_action(cx.listener(|this, _: &Activate, window, cx| list_activate(this, window, cx)))
        .on_action(cx.listener(|this, _: &Toggle, _, cx| list_toggle(this, cx)))
        .on_action(cx.listener(|this, _: &ShowMenu, window, cx| list_menu(this, window, cx)))
}

/// The stops of a list in which the grouped rows follow their group row.
/// `item` gives the kind of each item: a group key and its collapse state,
/// a row key and its `grouped` flag, or `None` for a row with no stop.
pub fn grouped_stops<'a, T: 'a>(
    items: impl IntoIterator<Item = &'a T>,
    item: impl Fn(&'a T) -> Option<Result<(&'a str, bool), (&'a str, bool)>>,
) -> Vec<Stop> {
    let mut group: Option<String> = None;
    items
        .into_iter()
        .map(|it| match item(it) {
            Some(Ok((key, collapsed))) => {
                group = Some(key.to_string());
                Stop::Group { key: key.to_string(), collapsed }
            }
            Some(Err((key, grouped))) => Stop::Row {
                key: key.to_string(),
                group: if grouped { group.clone() } else { None },
            },
            None => Stop::Skip,
        })
        .collect()
}

/// Scrolls a list of text lines, such as a log or JSON. The arrow keys
/// scroll a few lines, and Page Up and Page Down scroll a page. End goes to
/// the last line. With `follow`, the list then follows new lines again.
pub fn scroll_lines(list: &ListState, m: Move, follow: bool) {
    match m {
        Move::First => list.scroll_to(ListOffset { item_ix: 0, offset_in_item: px(0.) }),
        Move::Last if follow => list.set_follow_mode(FollowMode::Tail),
        Move::Last => list.scroll_to_end(),
        Move::By(d) if d.abs() >= PAGE_ROWS => {
            let page = list.viewport_bounds().size.height * PAGE_FRACTION;
            list.scroll_by(if d < 0 { -page } else { page });
        }
        Move::By(d) => list.scroll_by(px(LINE_SCROLL) * d as f32),
    }
}

/// A detail pane that the keys control.
pub trait DetailKeys: Sized + 'static {
    fn nav(&self) -> &KeyNav;
    /// Moves the cursor, or scrolls the current tab.
    fn key_move(&mut self, m: Move, cx: &mut Context<Self>);
    /// Command-Up Arrow. A file list shows the enclosing folder. The other
    /// tabs go to the top.
    fn key_parent(&mut self, cx: &mut Context<Self>) {
        self.key_move(Move::First, cx);
    }
    /// Return or Space on the pane. The default does the action of the
    /// cursor row.
    fn key_activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.nav().activate(window, cx);
    }
    /// Escape or Left Arrow. Returns true when the pane used the key, for
    /// example to close a popover. Otherwise the focus goes back.
    /// The position of the current tab, from 0, and the number of tabs.
    fn tab_position(&self) -> (usize, usize);
    /// Shows the tab at a position, from 0.
    fn show_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>);
    fn key_back(&mut self, _: &mut Context<Self>) -> bool {
        false
    }
}

/// Left Arrow and Right Arrow show the previous tab and the next tab. Left
/// Arrow on the first tab goes back to the list.
fn detail_tab_step<V: DetailKeys>(this: &mut V, delta: isize, window: &mut Window, cx: &mut Context<V>) {
    let (ix, count) = this.tab_position();
    let next = ix as isize + delta;
    if next < 0 {
        detail_back(this, window, cx);
        return;
    }
    if (next as usize) < count {
        this.key_back(cx);
        this.nav().focus.clone().focus(window, cx);
        this.show_tab(next as usize, window, cx);
        cx.notify();
    }
}

fn detail_back<V: DetailKeys>(this: &mut V, window: &mut Window, cx: &mut Context<V>) {
    if this.key_back(cx) {
        return;
    }
    // From a field or a button of the pane, go back to the pane. From the
    // pane, go back to the list.
    let focus = this.nav().focus.clone();
    if focus.is_focused(window) {
        window.dispatch_action(Box::new(FocusList), cx);
    } else {
        focus.focus(window, cx);
        cx.notify();
    }
}

/// Adds the keyboard actions of a detail pane to its root element.
pub fn detail_actions<V: DetailKeys>(el: Div, cx: &Context<V>) -> Div {
    let moves = |m: Move| {
        cx.listener(move |this: &mut V, _: &(), window: &mut Window, cx: &mut Context<V>| {
            // A key in a button of the pane moves the cursor of the pane.
            this.nav().focus.clone().focus(window, cx);
            this.key_move(m, cx);
            cx.notify();
        })
    };
    let (up, down, first, last) = (moves(Move::By(-1)), moves(Move::By(1)), moves(Move::First), moves(Move::Last));
    let (page_up, page_down) = (moves(Move::By(-PAGE_ROWS)), moves(Move::By(PAGE_ROWS)));
    el.key_context(DETAIL)
        .on_action(move |_: &MoveUp, window, cx| up(&(), window, cx))
        .on_action(move |_: &MoveDown, window, cx| down(&(), window, cx))
        .on_action(move |_: &MoveFirst, window, cx| first(&(), window, cx))
        .on_action(move |_: &MoveLast, window, cx| last(&(), window, cx))
        .on_action(move |_: &PageUp, window, cx| page_up(&(), window, cx))
        .on_action(move |_: &PageDown, window, cx| page_down(&(), window, cx))
        .on_action(cx.listener(|this, _: &Parent, _, cx| {
            this.key_parent(cx);
            cx.notify();
        }))
        .on_action(cx.listener(|this, _: &Activate, window, cx| {
            // A focused button of the pane takes the key itself.
            if this.nav().focus.is_focused(window) {
                this.key_activate(window, cx);
            } else {
                cx.propagate();
            }
        }))
        .on_action(cx.listener(|this, _: &MoveLeft, window, cx| detail_tab_step(this, -1, window, cx)))
        .on_action(cx.listener(|this, _: &MoveRight, window, cx| detail_tab_step(this, 1, window, cx)))
        .on_action(cx.listener(|this, _: &Back, window, cx| detail_back(this, window, cx)))
}

/// The bounds of an element in the last frame. The cursor row keeps its
/// bounds here, so a context menu can open next to it.
pub type BoundsCell = Rc<Cell<Option<Bounds<Pixels>>>>;

/// An empty element that records the bounds of its parent. Put it in an
/// element with `.relative()`.
pub fn track_bounds(cell: BoundsCell) -> impl IntoElement {
    canvas(move |bounds, _, _| cell.set(Some(bounds)), |_, _, _, _| {})
        .absolute()
        .top_0()
        .left_0()
        .size_full()
}

/// The point where a context menu opens for a row: below the row, near
/// its left edge.
pub fn menu_point(bounds: Option<Bounds<Pixels>>) -> Option<Point<Pixels>> {
    bounds.map(|b| point(b.left() + px(16.), b.bottom()))
}

pub type NavHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Makes a handler from a function of the view. The function runs when the
/// user clicks the row or presses Return on it.
pub fn handler<V: 'static>(
    cx: &Context<V>,
    f: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> NavHandler {
    let listener = cx.listener(move |this, _: &(), window, cx| f(this, window, cx));
    Rc::new(move |window, cx| listener(&(), window, cx))
}

/// The keyboard cursor of a detail pane. The pane adds its rows and
/// buttons in the order of the screen, each time that it renders. The arrow
/// keys then move through them.
pub struct KeyNav {
    pub focus: FocusHandle,
    /// The scroll area of the rows. The cursor row scrolls into view.
    pub scroll: ScrollHandle,
    /// The row at the cursor, and true when a key moved it last. The
    /// pointer moves the cursor too, so the arrow keys start from the row
    /// below the pointer.
    cursor: Rc<Cell<(Option<usize>, bool)>>,
    /// The pane, so a pointer hover can render it again.
    view: EntityId,
    /// The handlers of the rows in the last render.
    items: RefCell<Vec<NavHandler>>,
    /// True while the pane has the focus, so the cursor shows.
    shown: Cell<bool>,
    /// True after a move, until the cursor row is in view.
    reveal: Rc<Cell<bool>>,
}

impl KeyNav {
    pub fn new<V: 'static>(cx: &mut Context<V>) -> Self {
        KeyNav {
            focus: cx.focus_handle().tab_stop(true),
            scroll: ScrollHandle::new(),
            cursor: Rc::default(),
            view: cx.entity_id(),
            items: RefCell::default(),
            shown: Cell::new(false),
            reveal: Rc::default(),
        }
    }

    /// Starts a render. The rows of the last render go away.
    pub fn begin(&self, window: &Window) {
        self.items.borrow_mut().clear();
        self.shown.set(self.focus.is_focused(window));
    }

    /// Removes the cursor, and scrolls back to the top. Use this when the
    /// pane shows a different tab.
    pub fn reset(&mut self) {
        self.cursor.set((None, false));
        self.scroll.set_offset(point(px(0.), px(0.)));
    }

    /// True while the pane has the focus. The cursor shows only then.
    pub fn is_shown(&self) -> bool {
        self.shown.get()
    }

    /// Puts the cursor on the first row, if it has no row.
    pub fn start(&mut self) {
        if self.cursor.get().0.is_none() && !self.items.borrow().is_empty() {
            self.cursor.set((Some(0), true));
            self.reveal.set(true);
        }
    }

    /// Scrolls the rows by most of a page, and keeps the cursor. `pages`
    /// is 1 for Page Down and -1 for Page Up.
    pub fn scroll_page(&self, pages: f32) {
        let page = self.scroll.bounds().size.height * PAGE_FRACTION;
        let mut offset = self.scroll.offset();
        offset.y = (offset.y - page * pages).min(px(0.)).max(-self.scroll.max_offset().y);
        self.scroll.set_offset(offset);
    }

    pub fn move_cursor(&mut self, m: Move) {
        let count = self.items.borrow().len();
        self.cursor.set((m.apply(self.cursor.get().0, count), true));
        self.reveal.set(true);
    }

    /// Does the action of the row at the cursor. Returns false when there
    /// is no row at the cursor.
    pub fn activate(&self, window: &mut Window, cx: &mut App) -> bool {
        let handler = self.cursor.get().0.and_then(|ix| self.items.borrow().get(ix).cloned());
        match handler {
            Some(handler) => {
                // The pane is in an update here, and a handler from
                // `handler` updates the pane again. Run it after the update.
                window.defer(cx, move |window, cx| handler(window, cx));
                true
            }
            None => false,
        }
    }

    /// Adds a row. Returns its position, and true when the cursor is on it
    /// and shows.
    fn push(&self, handler: NavHandler) -> (usize, bool) {
        let mut items = self.items.borrow_mut();
        let ix = items.len();
        items.push(handler);
        (ix, self.shown.get() && self.cursor.get().0 == Some(ix))
    }

    /// Gives the cursor to a row that the pointer enters. A row that the
    /// pointer leaves loses it, unless a key moved it there. While the pane
    /// has the focus, only the cursor row has the hover fill, so the pointer
    /// and the keys never show two rows. The row must not set its own hover
    /// style, because gpui permits only one hover style on an element.
    fn follow_pointer(&self, row: Stateful<Div>, ix: usize, on: bool, cx: &App) -> Stateful<Div> {
        let cursor = self.cursor.clone();
        let view = self.view;
        let fill = native::hover(cx);
        row.relative()
            .when(on, |d| d.bg(fill).child(self.reveal_tracker()))
            .when(!self.shown.get() && !on, |d| d.hover(move |s| s.bg(fill)))
            .on_hover(move |hovered, _, cx| {
                let (at, keyed) = cursor.get();
                if *hovered {
                    cursor.set((Some(ix), false));
                } else if at == Some(ix) && !keyed {
                    cursor.set((None, false));
                } else {
                    return;
                }
                cx.notify(view);
            })
    }

    /// Records the bounds of the cursor row, and scrolls it into view after
    /// a move.
    fn reveal_tracker(&self) -> impl IntoElement + use<> {
        let scroll = self.scroll.clone();
        let reveal = self.reveal.clone();
        canvas(
            move |bounds, window, _| {
                if !reveal.replace(false) {
                    return;
                }
                let view = scroll.bounds();
                let mut offset = scroll.offset();
                let margin = px(REVEAL_MARGIN);
                if bounds.top() < view.top() {
                    offset.y += view.top() - bounds.top() + margin;
                } else if bounds.bottom() > view.bottom() {
                    offset.y -= bounds.bottom() - view.bottom() + margin;
                } else {
                    return;
                }
                offset.y = offset.y.min(px(0.)).max(-scroll.max_offset().y);
                scroll.set_offset(offset);
                window.refresh();
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// Adds a row that does `handler` on a click or on Return. The cursor
    /// row gets the hover fill.
    pub fn row(&self, row: Stateful<Div>, handler: NavHandler, cx: &App) -> Stateful<Div> {
        let (ix, on) = self.push(handler.clone());
        self.follow_pointer(row, ix, on, cx).on_click(move |_, window, cx| handler(window, cx))
    }

    /// Adds a row that has its own click action, for example the trigger
    /// of a popover. Return does `handler`.
    pub fn row_with_own_click(&self, row: Stateful<Div>, handler: NavHandler, cx: &App) -> Stateful<Div> {
        let (ix, on) = self.push(handler);
        self.follow_pointer(row, ix, on, cx)
    }

    /// Adds a button. A disabled button takes no cursor. The cursor button
    /// gets a focus ring. Set the size of the button on the wrapper that
    /// this returns, and give the button the full width.
    pub fn button(&self, button: Button, enabled: bool, handler: NavHandler, cx: &App) -> Div {
        // The pointer does not move the cursor to a button. A button has its
        // own hover style.
        let on = enabled && self.push(handler.clone()).1;
        let ring = native::accent(cx).opacity(0.6);
        let radius = cx.theme().radius;
        div()
            .relative()
            .child(button.on_click(move |_, window, cx| handler(window, cx)))
            .when(on, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(-3.))
                        .bottom(px(-3.))
                        .left(px(-3.))
                        .right(px(-3.))
                        .rounded(radius + px(3.))
                        .border_2()
                        .border_color(ring),
                )
                .child(self.reveal_tracker())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{Cursor, ListCursor, Move, Stop, cursor_position, move_in};

    #[test]
    fn moves_stay_in_the_list() {
        assert_eq!(Move::By(1).apply(None, 3), Some(0));
        assert_eq!(Move::By(-1).apply(None, 3), Some(2));
        assert_eq!(Move::By(1).apply(Some(2), 3), Some(2));
        assert_eq!(Move::By(-10).apply(Some(2), 3), Some(0));
        assert_eq!(Move::Last.apply(Some(0), 3), Some(2));
        assert_eq!(Move::First.apply(None, 0), None);
    }

    #[test]
    fn the_cursor_skips_rows_with_no_stop() {
        let stops = vec![
            Stop::Skip,
            Stop::Group { key: "web".into(), collapsed: false },
            Stop::Row { key: "a".into(), group: Some("web".into()) },
            Stop::Skip,
            Stop::Row { key: "b".into(), group: None },
        ];
        assert_eq!(move_in(&stops, None, Move::By(1)), Some(1));
        assert_eq!(move_in(&stops, Some(2), Move::By(1)), Some(4));
        assert_eq!(move_in(&stops, Some(1), Move::By(-1)), Some(1));
        assert_eq!(move_in(&stops, None, Move::Last), Some(4));
        let group = Cursor::Group("web".into());
        assert_eq!(cursor_position(&stops, None, Some("a")), Some(2));
        assert_eq!(cursor_position(&stops, Some(&group), Some("a")), Some(1));
        assert_eq!(cursor_position(&stops, None, None), None);
    }

    #[test]
    fn the_pointer_moves_the_hover() {
        let mut cursor = ListCursor::default();
        assert!(cursor.hover(Cursor::Row("a".into()), true));
        assert!(cursor.is_row("a"));
        assert!(cursor.hover(Cursor::Row("a".into()), false));
        assert_eq!(cursor.at, None);
        // A key moved the hover, so the pointer that leaves keeps it.
        cursor.at = Some(Cursor::Row("b".into()));
        cursor.keyed = true;
        assert!(!cursor.hover(Cursor::Row("b".into()), false));
        assert!(cursor.is_row("b"));
    }
}
