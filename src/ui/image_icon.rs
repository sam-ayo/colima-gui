//! A logo for each container, found from its image name.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::ActiveTheme;

use crate::docker::hub_logo;
use crate::runtime::Backend;

enum Logo {
    Loading,
    Ready(Arc<Image>),
    Missing,
}

/// The Docker Hub logos, by repository. A logo downloads one time, on the
/// first render of an image that uses it.
pub struct LogoStore {
    logos: HashMap<String, Logo>,
    client: reqwest::Client,
}

impl Global for LogoStore {}

impl LogoStore {
    pub fn init(cx: &mut App) {
        cx.set_global(LogoStore {
            logos: HashMap::new(),
            client: hub_logo::client(),
        });
    }

    /// Returns the logo for `image`, or `None` while it downloads or if
    /// there is no logo.
    fn logo(image: &str, cx: &mut App) -> Option<Arc<Image>> {
        let repo = hub_logo::hub_repo(image)?;
        let store = cx.global_mut::<LogoStore>();
        match store.logos.get(&repo) {
            Some(Logo::Ready(logo)) => return Some(logo.clone()),
            Some(_) => return None,
            None => {}
        }
        store.logos.insert(repo.clone(), Logo::Loading);
        let client = store.client.clone();
        let rx = Backend::global(cx).spawn({
            let repo = repo.clone();
            async move { hub_logo::logo(&client, &repo).await }
        });
        cx.spawn(async move |cx| {
            let bytes = rx.await.ok().flatten();
            let logo = bytes
                .and_then(|b| Some(Image::from_bytes(sniff_format(&b)?, b)))
                .map_or(Logo::Missing, |img| Logo::Ready(Arc::new(img)));
            cx.update(|cx| {
                cx.global_mut::<LogoStore>().logos.insert(repo, logo);
                cx.refresh_windows();
            })
        })
        .detach();
        None
    }
}

/// Finds the format from the first bytes. Docker Hub and Gravatar send
/// PNG or JPEG, but the content type is not always correct.
fn sniff_format(bytes: &[u8]) -> Option<ImageFormat> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some(ImageFormat::Png),
        [0xFF, 0xD8, ..] => Some(ImageFormat::Jpeg),
        [b'G', b'I', b'F', b'8', ..] => Some(ImageFormat::Gif),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(ImageFormat::Webp),
        _ => None,
    }
}

/// A logo with a status badge. A stopped container shows a faded logo.
#[derive(IntoElement)]
pub struct ContainerIcon {
    image: String,
    status: Hsla,
    running: bool,
    size: Pixels,
}

impl ContainerIcon {
    pub fn new(image: &str, status: Hsla, running: bool) -> Self {
        ContainerIcon {
            image: image.to_string(),
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
        let logo = LogoStore::logo(&self.image, cx);
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
            .when(!self.running, |d| d.opacity(0.55))
            .map(|d| match logo {
                // A white tile for all themes, because the logos are made
                // for a light background.
                Some(logo) => d.bg(gpui::white()).child(
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
        assert_eq!(sniff_format(b"{\"message\":\"not found\"}"), None);
    }
}
