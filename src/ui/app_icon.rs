//! The Dock icon. It takes its colors from the accent color and the
//! appearance, so it changes with the theme. The bundle icon in Finder
//! cannot change, so it has the colors of the default theme (see
//! `scripts/icon.sh`).

use gpui::{App, AsyncApp, Global, Hsla, Rgba, hsla};

/// The icon SVG. It has `{{name}}` tokens for the colors.
const TEMPLATE: &str = include_str!("../../assets/icon/colima-gui.svg");

/// The size of the icon in pixels. The largest Dock icon is 256 pt on a 2x
/// screen.
const SIZE: u32 = 512;

/// The colors of the hot container in the light appearance: lava, as in
/// the volcano of the name.
const LAVA: [&str; 3] = ["#FFF3C4", "#FFB020", "#FF7A00"];

/// The accent color and the appearance of the current Dock icon.
#[derive(Clone, Copy, PartialEq)]
struct IconKey {
    rgb: u32,
    dark: bool,
}

impl Global for IconKey {}

/// Sets the Dock icon for `accent` and the appearance. The app renders the
/// icon in the background, and only when the colors change.
pub fn update(accent: Hsla, dark: bool, set_icon: fn(&[u8]), cx: &mut App) {
    let key = IconKey { rgb: rgb(accent), dark };
    if cx.try_global::<IconKey>() == Some(&key) {
        return;
    }
    cx.set_global(key);
    let render = cx.background_executor().spawn(async move { png(accent, dark) });
    cx.spawn(async move |cx: &mut AsyncApp| {
        let Some(png) = render.await else {
            return;
        };
        // A newer theme can come while the icon renders. Its icon then wins.
        cx.update(|cx| {
            if cx.try_global::<IconKey>() == Some(&key) {
                set_icon(&png);
            }
        });
    })
    .detach();
}

/// Renders the icon as a PNG.
fn png(accent: Hsla, dark: bool) -> Option<Vec<u8>> {
    let svg = svg(accent, dark);
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(SIZE, SIZE)?;
    let scale = SIZE as f32 / tree.size().width();
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap.encode_png().ok()
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
    fn renders_all_hues() {
        for hue in [0., 0.1, 0.38, 0.51, 0.59, 0.78, 0.97] {
            for dark in [false, true] {
                let png = png(hsla(hue, 1., 0.5, 1.), dark).expect("the icon renders");
                assert!(png.starts_with(b"\x89PNG"));
            }
        }
        // Graphite has no saturation.
        assert!(png(hsla(0., 0., 0.55, 1.), true).is_some());
    }
}
