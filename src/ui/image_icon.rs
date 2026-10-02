//! A logo for each container, found from its image name and labels.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{ActiveTheme, Icon};
use gpui_kit_assets::IconName;

use crate::docker::image_logo::{self, Source};
use crate::runtime::Backend;
use crate::ui::SNAPPY_SPRING;

enum Logo {
    Loading,
    Ready(Arc<Image>),
    Missing,
}

/// The logos, by source. A logo downloads one time, on the first render
/// of a container that uses it.
pub struct LogoStore {
    logos: HashMap<Source, Logo>,
    client: reqwest::Client,
}

impl Global for LogoStore {}

impl LogoStore {
    pub fn init(cx: &mut App) {
        cx.set_global(LogoStore {
            logos: HashMap::new(),
            client: image_logo::client(),
        });
    }

    /// Returns the logo for a container, or `None` while it downloads or
    /// if there is no logo.
    fn logo(image: &str, icon: Option<&str>, cx: &mut App) -> Option<Arc<Image>> {
        let source = Source::find(image, icon)?;
        let store = cx.global_mut::<LogoStore>();
        match store.logos.get(&source) {
            Some(Logo::Ready(logo)) => return Some(logo.clone()),
            Some(_) => return None,
            None => {}
        }
        store.logos.insert(source.clone(), Logo::Loading);
        let client = store.client.clone();
        let rx = Backend::global(cx).spawn({
            let source = source.clone();
            async move { image_logo::logo(&client, &source).await }
        });
        cx.spawn(async move |cx| {
            let bytes = rx.await.ok().flatten();
            let logo = bytes
                .and_then(|b| Some(Image::from_bytes(sniff_format(&b)?, b)))
                .map_or(Logo::Missing, |img| Logo::Ready(Arc::new(img)));
            cx.update(|cx| {
                cx.global_mut::<LogoStore>().logos.insert(source, logo);
                cx.refresh_windows();
            })
        })
        .detach();
        None
    }
}

/// Finds the format from the first bytes, because a server does not always
/// send the correct content type.
fn sniff_format(bytes: &[u8]) -> Option<ImageFormat> {
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
    let text = text.trim_start();
    if text.starts_with("<svg") || (text.starts_with("<?xml") && text.contains("<svg")) {
        return Some(ImageFormat::Svg);
    }
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some(ImageFormat::Png),
        [0xFF, 0xD8, ..] => Some(ImageFormat::Jpeg),
        [b'G', b'I', b'F', b'8', ..] => Some(ImageFormat::Gif),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(ImageFormat::Webp),
        _ => None,
    }
}

/// The opacity of the tile of a stopped container or an unused image.
const STOPPED_OPACITY: f32 = 0.55;

/// A logo with a status badge. A stopped container shows a faded logo.
#[derive(IntoElement)]
pub struct ContainerIcon {
    id: ElementId,
    image: String,
    icon: Option<String>,
    status: Hsla,
    running: bool,
    size: Pixels,
}

impl ContainerIcon {
    /// `id` keeps the fade between the running and the stopped look. Give
    /// the same id to the same container. `icon` is the value of the
    /// `dev.orbstack.icon` label.
    pub fn new(
        id: impl Into<ElementId>,
        image: &str,
        icon: Option<&str>,
        status: Hsla,
        running: bool,
    ) -> Self {
        ContainerIcon {
            id: id.into(),
            image: image.to_string(),
            icon: icon.map(str::to_string),
            status,
            running,
            size: px(28.),
        }
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for ContainerIcon {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let logo = LogoStore::logo(&self.image, self.icon.as_deref(), cx);
        let t = cx.theme();
        let badge = self.size * 0.36;
        div()
            .relative()
            .flex_shrink_0()
            .size(self.size)
            .rounded(t.radius)
            .border_1()
            .border_color(t.border)
            .flex()
            .items_center()
            .justify_center()
            .map(|d| match logo {
                // A white tile for all themes, because the logos are made
                // for a light background.
                Some(logo) => d.bg(crate::ui::native::paper(cx)).child(
                    img(logo)
                        .size(self.size * 0.8)
                        .object_fit(ObjectFit::Contain)
                        .grayscale(!self.running),
                ),
                None => d.bg(t.muted).child(
                    svg()
                        .path("icons/container.svg")
                        .size(self.size * 0.6)
                        .text_color(t.muted_foreground),
                ),
            })
            .child(
                div()
                    .absolute()
                    .right(-badge * 0.25)
                    .bottom(-badge * 0.25)
                    .size(badge)
                    .rounded_full()
                    .border_2()
                    .border_color(t.background)
                    .bg(self.status),
            )
            // A start or a stop fades the tile to its new look. The spring
            // starts at its target, so a row that scrolls into view does
            // not fade.
            .with_spring(
                self.id,
                SpringAnimation::new(SNAPPY_SPRING).to(if self.running { 1. } else { 0. }),
                |d, lit: f32| d.opacity(STOPPED_OPACITY + (1. - STOPPED_OPACITY) * lit),
            )
    }
}

/// The logo of an image, with no status badge. An image that no container
/// uses shows a faded logo.
#[derive(IntoElement)]
pub struct ImageLogo {
    /// A reference such as "redis:7-alpine". An untagged image has none,
    /// and shows the generic icon.
    reference: Option<String>,
    icon: Option<String>,
    dimmed: bool,
    size: Pixels,
}

impl ImageLogo {
    /// `icon` is the value of the `dev.orbstack.icon` label.
    pub fn new(reference: Option<&str>, icon: Option<&str>) -> Self {
        ImageLogo {
            reference: reference.map(str::to_string),
            icon: icon.map(str::to_string),
            dimmed: false,
            size: px(32.),
        }
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }

    pub fn dimmed(mut self, dimmed: bool) -> Self {
        self.dimmed = dimmed;
        self
    }
}

impl RenderOnce for ImageLogo {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let logo = self
            .reference
            .as_deref()
            .and_then(|r| LogoStore::logo(r, self.icon.as_deref(), cx));
        let t = cx.theme();
        div()
            .flex_shrink_0()
            .size(self.size)
            .rounded(px(8.))
            .border_1()
            .border_color(t.border)
            .flex()
            .items_center()
            .justify_center()
            .map(|d| match logo {
                // A white tile for all themes, as for the containers.
                Some(logo) => d.bg(crate::ui::native::paper(cx)).child(
                    img(logo)
                        .size(self.size * 0.8)
                        .object_fit(ObjectFit::Contain)
                        .grayscale(self.dimmed),
                ),
                None => d.bg(t.muted).child(
                    Icon::new(IconName::Layers)
                        .size(self.size * 0.5)
                        .text_color(t.muted_foreground),
                ),
            })
            .when(self.dimmed, |d| d.opacity(STOPPED_OPACITY))
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that brings `gpui::test`, which replaces `#[test]`.
    use super::sniff_format;
    use gpui::ImageFormat;

    #[test]
    fn sniffs_image_formats() {
        assert_eq!(sniff_format(b"\x89PNG\r\n"), Some(ImageFormat::Png));
        assert_eq!(sniff_format(&[0xFF, 0xD8, 0xFF]), Some(ImageFormat::Jpeg));
        assert_eq!(sniff_format(b"RIFF\0\0\0\0WEBPVP8"), Some(ImageFormat::Webp));
        assert_eq!(sniff_format(b"<?xml version=\"1.0\"?>\n<svg xmlns=\"\">"), Some(ImageFormat::Svg));
        assert_eq!(sniff_format(b"<svg viewBox=\"0 0 1 1\">"), Some(ImageFormat::Svg));
        assert_eq!(sniff_format(b"<!DOCTYPE html><html>"), None);
        assert_eq!(sniff_format(b"{\"message\":\"not found\"}"), None);
    }
}
