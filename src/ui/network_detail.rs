//! The detail pane of a network: a summary, the containers, and the inspect
//! data.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use crate::docker::containers::State;
use crate::docker::networks::{self, NetworkInfo};
use crate::runtime::Backend;
use crate::state::container_store::ContainerStore;
use crate::state::network_store::NetworkStore;
use crate::ui::container_detail::{card, field, list_row, loading, section, segmented_control};
use crate::ui::json_view::{self, Json};
use crate::ui::key_nav::{self, DetailKeys, KeyNav, Move};
use crate::ui::{ENTER_DURATION, NATIVE_BUTTON_HEIGHT, TOOLBAR_HEIGHT, ago, drag_area, native, text, toast};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkTab {
    #[default]
    Info,
    Inspect,
}

impl NetworkTab {
    const ALL: [NetworkTab; 2] = [NetworkTab::Info, NetworkTab::Inspect];

    fn label(self) -> &'static str {
        match self {
            NetworkTab::Info => "Info",
            NetworkTab::Inspect => "Inspect",
        }
    }
}

pub enum NetworkDetailEvent {
    Close,
    /// Show a container in the container list.
    RevealContainer(String),
    /// Ask the user, then remove the network.
    Remove(String),
}

enum Load<T> {
    Loading,
    Loaded(T),
    Failed(String),
}

/// A container on the network.
struct Member {
    id: String,
    name: String,
    state: State,
    status: String,
    /// The IPv4 address without the prefix length. Only a running container
    /// has one.
    ip: Option<String>,
}

pub struct NetworkDetail {
    name: String,
    store: Entity<NetworkStore>,
    containers: Entity<ContainerStore>,
    tab: NetworkTab,
    /// The number of tab changes. Each change fades in the new tab body.
    tab_changes: usize,
    info: Load<Rc<NetworkInfo>>,
    inspect_json: Rc<Json>,
    inspect_list: ListState,
    /// The store revision of the current inspect data.
    revision: u64,
    nav: KeyNav,
    _subs: Vec<Subscription>,
}

impl EventEmitter<NetworkDetailEvent> for NetworkDetail {}

impl Focusable for NetworkDetail {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.nav.focus.clone()
    }
}

impl DetailKeys for NetworkDetail {
    fn nav(&self) -> &KeyNav {
        &self.nav
    }

    fn tab_position(&self) -> (usize, usize) {
        let ix = NetworkTab::ALL.iter().position(|&t| t == self.tab).unwrap_or(0);
        (ix, NetworkTab::ALL.len())
    }

    fn show_tab(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) {
        self.set_tab_at(ix, cx);
    }

    fn key_move(&mut self, m: Move, _: &mut Context<Self>) {
        match self.tab {
            NetworkTab::Info => match m {
                Move::By(d) if d.abs() >= key_nav::PAGE_ROWS => self.nav.scroll_page(d.signum() as f32),
                _ => self.nav.move_cursor(m),
            },
            NetworkTab::Inspect => key_nav::scroll_lines(&self.inspect_list, m, false),
        }
    }

    fn key_parent(&mut self, cx: &mut Context<Self>) {
        self.key_move(Move::First, cx);
    }

    fn key_activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.nav.activate(window, cx);
    }
}

impl NetworkDetail {
    pub fn new(
        name: String,
        store: Entity<NetworkStore>,
        containers: Entity<ContainerStore>,
        tab: NetworkTab,
        cx: &mut Context<Self>,
    ) -> Self {
        // A container that joins or leaves the network changes only the
        // inspect data. Each new list from the store reads it again.
        let subs = vec![
            cx.observe(&store, |this: &mut Self, store, cx| {
                if store.read(cx).revision != this.revision {
                    this.load_info(cx);
                }
            }),
            // The states of the containers on the network.
            cx.observe(&containers, |_, _, cx| cx.notify()),
        ];
        let revision = store.read(cx).revision;
        let mut this = NetworkDetail {
            name,
            store,
            containers,
            tab,
            tab_changes: 0,
            info: Load::Loading,
            inspect_json: Rc::new(Json::default()),
            inspect_list: ListState::new(0, ListAlignment::Top, px(200.)),
            revision,
            nav: KeyNav::new(cx),
            _subs: subs,
        };
        this.load_info(cx);
        this
    }

    pub fn tab(&self) -> NetworkTab {
        self.tab
    }

    /// Shows the tab at a position, from 0. A position with no tab does
    /// nothing.
    pub fn set_tab_at(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(&tab) = NetworkTab::ALL.get(ix) {
            self.set_tab(tab, cx);
        }
    }

    /// Shows the next tab, or the previous tab for a negative `delta`.
    pub fn cycle_tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = NetworkTab::ALL.len() as isize;
        let ix = NetworkTab::ALL.iter().position(|&t| t == self.tab).unwrap_or(0) as isize;
        self.set_tab(NetworkTab::ALL[(ix + delta).rem_euclid(count) as usize], cx);
    }

    /// Takes the focus from the list. The cursor goes to the first row.
    pub fn focus_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.nav.focus.focus(window, cx);
        self.nav.start();
        cx.notify();
    }

    fn set_tab(&mut self, tab: NetworkTab, cx: &mut Context<Self>) {
        if tab != self.tab {
            self.tab_changes += 1;
            self.nav.reset();
        }
        self.tab = tab;
        cx.notify();
    }

    fn load_info(&mut self, cx: &mut Context<Self>) {
        let Some(docker) = self.containers.read(cx).docker().cloned() else { return };
        self.revision = self.store.read(cx).revision;
        let name = self.name.clone();
        let rx = Backend::global(cx).spawn(async move { networks::inspect(&docker, &name).await });
        cx.spawn(async move |this, cx| {
            let result = rx.await;
            let _ = this.update(cx, |this, cx| {
                this.info = match result {
                    Ok(Ok(info)) => {
                        let json = Json::new(info.json.clone());
                        // A reload keeps the scroll position of the Inspect tab.
                        if this.inspect_list.item_count() != json.lines.len() {
                            this.inspect_list.reset_with_uniform_height(json.lines.len(), px(json_view::LINE_HEIGHT));
                        }
                        this.inspect_json = Rc::new(json);
                        Load::Loaded(Rc::new(info))
                    }
                    // Keep the old data on screen. The network can go away
                    // before the pane closes.
                    Ok(Err(_)) | Err(_) if matches!(this.info, Load::Loaded(_)) => return,
                    Ok(Err(e)) => Load::Failed(e.to_string()),
                    Err(_) => Load::Failed("cancelled".into()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// The containers on the network, running first. The engine gives the
    /// addresses of the running containers only.
    fn members(&self, info: Option<&NetworkInfo>, cx: &App) -> Vec<Member> {
        let ip_of = |id: &str| {
            info?
                .endpoints
                .iter()
                .find(|e| e.container_id == id)
                .and_then(|e| e.ipv4.as_deref())
                .map(|ip| ip.split('/').next().unwrap_or(ip).to_string())
        };
        let mut members: Vec<Member> = self
            .containers
            .read(cx)
            .set
            .rows
            .values()
            .filter(|c| c.networks.contains(&self.name))
            .map(|c| Member {
                id: c.id.clone(),
                name: c.name.clone(),
                state: c.state,
                status: c.status.clone(),
                ip: ip_of(&c.id),
            })
            .collect();
        members.sort_by(|a, b| b.state.is_running().cmp(&a.state.is_running()).then_with(|| a.name.cmp(&b.name)));
        members
    }

    fn render_info(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        let mono = t.mono_font_family.clone();
        let muted = t.muted_foreground;
        let store = self.store.read(cx);
        let Some(row) = store.rows.get(&self.name).cloned() else {
            return div().into_any_element();
        };
        let removing = store.removing.contains(&self.name);
        let info = match &self.info {
            Load::Loaded(info) => Some(info.clone()),
            _ => None,
        };
        let created = info
            .as_ref()
            .and_then(|i| i.created.as_deref())
            .and_then(|c| chrono::DateTime::parse_from_rfc3339(c).ok())
            .map(|c| ago(c.timestamp()));
        let yes_no = |on: bool| if on { "Yes" } else { "No" };
        let addresses = |list: &[String]| if list.is_empty() { "–".to_string() } else { list.join(", ") };
        let id = row.id.clone();

        let summary = card(
            [
                field("Name", div().font_family(mono.clone()).truncate().child(row.name.clone()), cx),
                field(
                    "Network ID",
                    h_flex()
                        .gap_1()
                        .child(div().flex_1().min_w_0().truncate().font_family(mono.clone()).child(row.short_id().to_string()))
                        .child(self.nav.button(
                            Button::new("copy-network-id")
                                .icon(Icon::new(IconName::Copy))
                                .ghost()
                                .xsmall()
                                .tooltip("Copy the full ID"),
                            true,
                            Rc::new(move |_, cx| toast::copy(id.clone(), id[..id.len().min(12)].to_string(), cx)),
                            cx,
                        )),
                    cx,
                ),
                field("Driver", div().child(row.driver.clone()), cx),
                field("Subnet", div().font_family(mono.clone()).child(addresses(&row.subnets)), cx),
                field("Gateway", div().font_family(mono.clone()).child(addresses(&row.gateways)), cx),
                field("IPv6", div().child(yes_no(row.ipv6)), cx),
                field("Internal", div().child(yes_no(row.internal)), cx),
                field("Created", div().child(created.unwrap_or_else(|| "–".into())), cx),
            ],
            cx,
        );

        let compose = row.compose_project.clone().map(|project| {
            section("Compose", cx).child(card(
                [
                    Some(field("Project", div().child(project), cx)),
                    row.compose_network.clone().map(|n| field("Network", div().font_family(mono.clone()).child(n), cx)),
                ]
                .into_iter()
                .flatten(),
                cx,
            ))
        });

        let members = self.members(info.as_deref(), cx);
        let in_use = members.iter().any(|m| m.state.is_running());
        let containers = section("Containers", cx).child(if members.is_empty() {
            card([list_row().text_color(muted).child("No container is on this network.")], cx)
        } else {
            card(
                members.into_iter().enumerate().map(|(ix, m)| {
                    let dot = super::state_color(m.state, cx);
                    let (detail, mono_detail) = match m.ip {
                        Some(ip) => (ip, true),
                        None => (m.status, false),
                    };
                    let row = list_row()
                        .id(("member", ix))
                        .gap_2p5()
                        .cursor_pointer()
                        .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(dot))
                        .child(div().flex_1().min_w_0().truncate().child(m.name))
                        .child(
                            div()
                                .flex_shrink_0()
                                .max_w(relative(0.5))
                                .truncate()
                                .text_color(muted)
                                .when(mono_detail, |d| d.font_family(mono.clone()))
                                .child(detail),
                        )
                        .child(Icon::new(IconName::ChevronRight).xsmall().text_color(muted));
                    let id = m.id;
                    self.nav
                        .row(
                            row,
                            key_nav::handler(cx, move |_, _, cx| {
                                cx.emit(NetworkDetailEvent::RevealContainer(id.clone()))
                            }),
                            cx,
                        )
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

        let built_in = row.is_built_in();
        let blocked = built_in || in_use || removing;
        let tooltip = if built_in {
            "Docker makes this network. You cannot remove it."
        } else if in_use {
            "Stop the containers on the network first"
        } else {
            "Delete the network"
        };
        let remove = self.nav.button(
            Button::new("remove-network")
                .outline()
                .small()
                .h(px(NATIVE_BUTTON_HEIGHT))
                .w_full()
                .icon(Icon::new(IconName::Trash))
                .label("Remove…")
                .disabled(blocked)
                .tooltip(tooltip),
            !blocked,
            key_nav::handler(cx, |this, _, cx| cx.emit(NetworkDetailEvent::Remove(this.name.clone()))),
            cx,
        );

        // The content is in an inner column, so a long list scrolls and
        // the cards do not shrink to the height of the pane.
        div()
            .id("network-info")
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
                    .children(compose)
                    .child(containers)
                    .children(labels)
                    .children(options)
                    .child(div().pt_1().child(remove)),
            )
            .into_any_element()
    }

    fn render_inspect(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme();
        match &self.info {
            Load::Loading => loading("network-inspect-loading"),
            Load::Failed(e) => div().p_3().text_color(t.red).child(e.clone()).into_any_element(),
            Load::Loaded(_) => json_view::render(&self.inspect_json, &self.inspect_list, cx),
        }
    }
}

impl Render for NetworkDetail {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.nav.begin(window);
        let t = cx.theme().clone();
        let row = self.store.read(cx).rows.get(&self.name).cloned();

        let body = match self.tab {
            NetworkTab::Info => self.render_info(cx),
            NetworkTab::Inspect => self.render_inspect(cx),
        };
        let body = if self.tab_changes > 0 {
            div()
                .size_full()
                .child(body)
                .with_animation(
                    ("network-tab-fade", self.tab_changes),
                    Animation::new(ENTER_DURATION).with_easing(ease_out_quint()),
                    |d, t| d.opacity(t),
                )
                .into_any_element()
        } else {
            body
        };

        let header = row.as_ref().map(|r| {
            let kind = if r.is_built_in() { "Built-in network".to_string() } else { format!("{} network", r.driver) };
            let subtitle = [r.compose_project.clone(), Some(kind), r.subnets.first().cloned()]
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
                        .child(Icon::new(IconName::Globe).size(px(24.)).text_color(native::accent(cx))),
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

        let labels: Vec<&'static str> = NetworkTab::ALL.iter().map(|t| t.label()).collect();
        let selected = NetworkTab::ALL.iter().position(|&tab| tab == self.tab).unwrap_or(0);
        let view = cx.weak_entity();
        let tabs = segmented_control(
            "network-tabs",
            labels,
            selected,
            Rc::new(move |ix, _, cx| {
                let _ = view.update(cx, |this, cx| this.set_tab(NetworkTab::ALL[ix], cx));
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
                    .child(drag_area("network-detail-drag-left").flex_1().h_full())
                    .child(tabs)
                    .child(drag_area("network-detail-drag-right").flex_1().h_full())
                    .child(
                        Button::new("close-network-detail")
                            .icon(Icon::new(IconName::X))
                            .custom(native::toolbar_button(cx))
                            .xsmall()
                            .tooltip("Close (Esc)")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(NetworkDetailEvent::Close))),
                    ),
            )
            .children(header)
            .child(div().flex_1().min_h_0().child(body))
    }
}
