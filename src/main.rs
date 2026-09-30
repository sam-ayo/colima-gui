mod colima;
mod docker;
mod runtime;
mod state;
mod ui;

use gpui::*;
use gpui_component::{Root, Theme};

actions!(colima_gui, [Quit]);

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn".into()),
        )
        .init();
    tracing::debug!("process start");
    let backend = runtime::Backend::new();
    let early = state::container_store::EarlyConnect::start(&backend);

    gpui_platform::application()
        .with_assets(gpui_kit_assets::AllAssets)
        .run(|cx: &mut App| {
            cx.set_global(backend);
            cx.set_global(early);
            gpui_component::init(cx);
            ui::image_icon::LogoStore::init(cx);
            ui::sync_reduce_motion(cx);
            ui::container_list::bind_keys(cx);
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.on_window_closed(|cx, _| cx.quit()).detach();

            let bounds = Bounds::centered(None, size(px(1240.), px(780.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                // The sidebar and the toolbars fill the title bar area. The
                // views move the window themselves (see `ui::drag_area`).
                titlebar: Some(TitlebarOptions {
                    title: Some("Colima".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(18.), px(18.))),
                }),
                app_owns_titlebar_drag: true,
                window_min_size: Some(size(px(900.), px(500.))),
                ..Default::default()
            };
            cx.open_window(options, |window, cx| {
                Theme::sync_system_appearance(Some(window), cx);
                let view = cx.new(|cx| ui::app_view::AppView::new(window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("failed to open the window");
            cx.activate(true);
        });
}
