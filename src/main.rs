mod assets;
mod colima;
mod docker;
mod runtime;
mod state;
mod terminal;
mod ui;
mod updater;

use gpui::*;
use gpui_component::Root;

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
        .with_assets(assets::Assets)
        .run(|cx: &mut App| {
            cx.set_global(backend);
            cx.set_global(early);
            gpui_component::init(cx);
            ui::image_icon::LogoStore::init(cx);
            ui::sync_reduce_motion(cx);
            ui::key_nav::bind_keys(cx);
            ui::app_view::bind_keys(cx);
            ui::container_list::bind_keys(cx);
            ui::volume_list::bind_keys(cx);
            ui::image_list::bind_keys(cx);
            ui::network_list::bind_keys(cx);
            ui::native::init_menus(cx);
            ui::quit::init(cx);
            updater::init(cx);
            cx.on_window_closed(|cx, _| cx.quit()).detach();

            let bounds = Bounds::centered(None, size(px(1240.), px(780.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                // The sidebar and the toolbars fill the title bar area. The
                // views move the window themselves (see `ui::drag_area`).
                titlebar: Some(TitlebarOptions {
                    title: Some(ui::native::APP_NAME.into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(18.), px(18.))),
                }),
                app_owns_titlebar_drag: true,
                window_min_size: Some(size(px(900.), px(500.))),
                ..Default::default()
            };
            cx.open_window(options, |window, cx| {
                ui::native::sync_theme(window, cx);
                ui::native::autosave_frame(window, cx);
                let view = cx.new(|cx| ui::app_view::AppView::new(window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("failed to open the window");
            cx.activate(true);
        });
}
