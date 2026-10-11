//! The Dock icon. It takes its colors from the accent color and the
//! appearance, so it changes with the theme. It is dim when no Colima
//! machine runs.
//!
//! The app saves the icons of the theme to a folder (see `cache_dir`). The
//! Dock tile plug-in (`macos/DockTile`) shows them when the app does not
//! run, and the app shows them at launch, before it renders a new icon. The
//! bundle icon has the colors of the default theme (see `scripts/icon.sh`).
//! Finder shows it, and the Dock shows it if the folder is empty.

use std::path::PathBuf;

use gpui::{App, AsyncApp, Global, Hsla, Rgba, hsla};

/// The icon SVG. It has `{{name}}` tokens for the colors.
const TEMPLATE: &str = include_str!("../../assets/icon/colima-gui.svg");

/// The size of the icon in pixels. The largest Dock icon is 256 pt on a 2x
/// screen.
const SIZE: u32 = 512;

/// The colors of the hot container in the light appearance: lava, as in
/// the volcano of the name.
const LAVA: [&str; 3] = ["#FFF3C4", "#FFB020", "#FF7A00"];

/// The opacity of the icon when no machine runs.
const DIM_OPACITY: f32 = 0.6;
/// The part of the color that the dim icon loses. 1 gives a gray icon.
const DIM_DESATURATE: f32 = 0.6;

/// The colors and the Colima state of the Dock icon.
#[derive(Clone, Copy, PartialEq)]
struct IconKey {
    /// The accent color in the light appearance and in the dark appearance,
    /// as 0xRRGGBB.
    rgb: [u32; 2],
    dark: bool,
    running: bool,
}

impl Global for IconKey {}

/// The accent colors of the theme. The icon of the app and the saved icons
/// of the plug-in use them.
#[derive(Clone, Copy)]
struct Accents([Hsla; 2]);

impl Global for Accents {}

/// True if a Colima machine runs.
#[derive(Clone, Copy)]
struct Running(bool);

impl Global for Running {}

/// Shows the saved icon at once, so that the Dock does not show the bundle
/// icon until the app renders a new icon. Call it at launch, after the app
/// sets its appearance. `dark` is the appearance of the app.
pub fn init(dark: bool, cx: &mut App) {
    let running = crate::colima::any_running();
    cx.set_global(Running(running));
    let saved = cache_dir().and_then(|dir| std::fs::read(dir.join(file_name(dark, running))).ok());
    if let Some(png) = saved {
        super::native::set_dock_icon(&png);
    }
}

/// Sets the theme of the Dock icon. `accents` has the accent color in the
/// light appearance and in the dark appearance. `dark` is the appearance of
/// the app.
pub fn set_theme(accents: [Hsla; 2], dark: bool, cx: &mut App) {
    cx.set_global(Accents(accents));
    update(dark, cx);
}

/// Sets the Colima state of the Dock icon.
pub fn set_running(running: bool, cx: &mut App) {
    if cx.try_global::<Running>().is_some_and(|r| r.0 == running) {
        return;
    }
    cx.set_global(Running(running));
    if let Some(key) = cx.try_global::<IconKey>() {
        update(key.dark, cx);
    }
}

/// Renders the Dock icon in the background, but only when the colors or the
/// state change. Then saves the icons for the plug-in if the theme changed.
fn update(dark: bool, cx: &mut App) {
    let Some(&Accents(accents)) = cx.try_global::<Accents>() else {
        return;
    };
    let running = cx.try_global::<Running>().is_some_and(|r| r.0);
    let key = IconKey { rgb: accents.map(rgb), dark, running };
    if cx.try_global::<IconKey>() == Some(&key) {
        return;
    }
    cx.set_global(key);
    let accent = accents[dark as usize];
    let render = cx.background_executor().spawn(async move { png(accent, dark, running) });
    let background = cx.background_executor().clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let Some(png) = render.await else {
            return;
        };
        // A newer theme can come while the icon renders. Its icon then wins.
        let current = cx.update(|cx| {
            let current = cx.try_global::<IconKey>() == Some(&key);
            if current {
                super::native::set_dock_icon(&png);
            }
            current
        });
        if current {
            background.spawn(async move { save(accents) }).await;
        }
    })
    .detach();
}

/// The name of the saved icon for an appearance and a state. The plug-in
/// uses the same names.
fn file_name(dark: bool, running: bool) -> String {
    format!(
        "{}-{}.png",
        if dark { "dark" } else { "light" },
        if running { "running" } else { "stopped" }
    )
}

/// The folder of the saved icons:
/// `~/Library/Application Support/colima-gui/DockIcon`. The name of the
/// folder in Application Support is the bundle ID.
fn cache_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(home.join("Library/Application Support/colima-gui/DockIcon"))
}

/// Saves the icons of all appearances and states. A stamp file names the
/// colors, the app version, and a hash of the SVG of the saved icons, so
/// the app writes them again only after a change.
fn save(accents: [Hsla; 2]) {
    if let Some(dir) = cache_dir() {
        save_to(&dir, accents);
    }
}

fn save_to(dir: &std::path::Path, accents: [Hsla; 2]) {
    let [light, dark] = accents.map(rgb);
    let design = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        TEMPLATE.hash(&mut hasher);
        hasher.finish()
    };
    let stamp = format!("{} {design:016X} {light:06X} {dark:06X}\n", env!("CARGO_PKG_VERSION"));
    let stamp_file = dir.join("stamp");
    if std::fs::read_to_string(&stamp_file).is_ok_and(|s| s == stamp) {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!("cannot make {}: {e}", dir.display());
        return;
    }
    for dark in [false, true] {
        for running in [false, true] {
            let Some(png) = png(accents[dark as usize], dark, running) else {
                return;
            };
            // Write to a temporary file first, so the plug-in never reads a
            // part of a file.
            let file = dir.join(file_name(dark, running));
            let part = file.with_extension("part");
            if let Err(e) = std::fs::write(&part, png).and_then(|()| std::fs::rename(&part, &file)) {
                tracing::warn!("cannot save {}: {e}", file.display());
                return;
            }
        }
    }
    let _ = std::fs::write(stamp_file, stamp);
}

/// Renders the icon as a PNG. The icon is dim if `running` is false.
fn png(accent: Hsla, dark: bool, running: bool) -> Option<Vec<u8>> {
    let svg = svg(accent, dark);
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(SIZE, SIZE)?;
    let scale = SIZE as f32 / tree.size().width();
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    if !running {
        dim(pixmap.data_mut());
    }
    pixmap.encode_png().ok()
}

/// Makes the pixels gray in part, then makes them transparent in part. The
/// pixels are RGBA with premultiplied alpha, so a gray or a scale of all
/// four channels keeps them valid.
fn dim(pixels: &mut [u8]) {
    for px in pixels.as_chunks_mut::<4>().0 {
        let [r, g, b] = [px[0], px[1], px[2]].map(f32::from);
        let gray = 0.299 * r + 0.587 * g + 0.114 * b;
        for c in &mut px[..3] {
            let value = f32::from(*c) + (gray - f32::from(*c)) * DIM_DESATURATE;
            *c = (value * DIM_OPACITY).round() as u8;
        }
        px[3] = (f32::from(px[3]) * DIM_OPACITY).round() as u8;
    }
}

/// The icon SVG with the colors for `accent` and the appearance.
fn svg(accent: Hsla, dark: bool) -> String {
    colors(accent, dark)
        .iter()
        .fold(TEMPLATE.to_owned(), |svg, (name, value)| svg.replace(&format!("{{{{{name}}}}}"), value))
}

/// The values of the template tokens. All colors have the hue of the
/// accent. A gray accent, such as Graphite, gives a gray icon.
fn colors(accent: Hsla, dark: bool) -> [(&'static str, String); 12] {
    let hue = accent.h * 360.;
    let saturation = accent.s.min(1.);
    // The background moves to a near hue at the end of the gradient: to red
    // for the warm hues, and to violet for the cool hues. Then it does not
    // go to olive or to brown.
    let shift = if (160. ..=300.).contains(&hue) { 25. } else { -15. };
    let color = |dh: f32, s: f32, l: f32| hex(hsla((hue + dh).rem_euclid(360.) / 360., s * saturation, l, 1.));
    let white = || "#FFFFFF".to_owned();
    if dark {
        // A dark tile with a glow of the accent color, and a hot container
        // in the accent color.
        [
            ("bg_start", color(0., 0.35, 0.22)),
            ("bg_middle", color(shift / 2., 0.28, 0.11)),
            ("bg_end", color(shift, 0.25, 0.05)),
            ("glass_top", white()),
            ("glass_left", color(0., 0.70, 0.90)),
            ("glass_right", color(0., 0.55, 0.78)),
            ("hot_top", color(0., 1., 0.85)),
            ("hot_left", color(0., 1., 0.60)),
            ("hot_right", color(shift / 3., 0.95, 0.47)),
            ("shadow", "#000000".to_owned()),
            ("glow", color(0., 1., 0.55)),
            ("glow_opacity", "0.55".to_owned()),
        ]
    } else {
        // A tile in the accent color, and a lava container.
        [
            ("bg_start", color(-shift / 2., 0.95, 0.68)),
            ("bg_middle", color(0., 0.80, 0.50)),
            ("bg_end", color(shift, 0.65, 0.26)),
            ("glass_top", white()),
            ("glass_left", color(0., 0.55, 0.93)),
            ("glass_right", color(0., 0.45, 0.83)),
            ("hot_top", LAVA[0].to_owned()),
            ("hot_left", LAVA[1].to_owned()),
            ("hot_right", LAVA[2].to_owned()),
            ("shadow", color(shift, 0.6, 0.12)),
            ("glow", white()),
            ("glow_opacity", "0".to_owned()),
        ]
    }
}

fn hex(color: Hsla) -> String {
    format!("#{:06X}", rgb(color))
}

/// The color as 0xRRGGBB.
fn rgb(color: Hsla) -> u32 {
    let c = Rgba::from(color);
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u32;
    byte(c.r) << 16 | byte(c.g) << 8 | byte(c.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_all_tokens() {
        for dark in [false, true] {
            assert!(!svg(hsla(0.5, 1., 0.4, 1.), dark).contains("{{"));
        }
    }

    #[test]
    fn dim_keeps_premultiplied_pixels_valid() {
        let mut pixels = [255, 40, 0, 255, 10, 10, 10, 20, 0, 0, 0, 0];
        dim(&mut pixels);
        for px in pixels.as_chunks::<4>().0 {
            assert!(px[..3].iter().all(|&c| c <= px[3]));
        }
        assert_eq!(pixels[3], 153);
    }

    #[test]
    fn saves_all_icons_once() {
        let dir = std::env::temp_dir().join(format!("colima-gui-icon-{}", std::process::id()));
        let accents = [hsla(0.5, 1., 0.4, 1.), hsla(0.5, 1., 0.5, 1.)];
        save_to(&dir, accents);
        let names = ["light-running.png", "light-stopped.png", "dark-running.png", "dark-stopped.png"];
        for name in names {
            assert!(std::fs::read(dir.join(name)).unwrap().starts_with(b"\x89PNG"), "{name}");
        }
        // The stamp agrees, so a second save does not write the icons again.
        std::fs::remove_file(dir.join(names[0])).unwrap();
        save_to(&dir, accents);
        assert!(!dir.join(names[0]).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn renders_all_hues() {
        for hue in [0., 0.1, 0.38, 0.51, 0.59, 0.78, 0.97] {
            for dark in [false, true] {
                for running in [false, true] {
                    let png = png(hsla(hue, 1., 0.5, 1.), dark, running).expect("the icon renders");
                    assert!(png.starts_with(b"\x89PNG"));
                }
            }
        }
        // Graphite has no saturation.
        assert!(png(hsla(0., 0., 0.55, 1.), true, true).is_some());
    }
}
