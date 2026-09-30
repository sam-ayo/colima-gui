//! The app assets: the Lucide icons from `gpui-kit-assets`, and the brand
//! logos in `assets/brand`.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub const DOCKER_LOGO: &str = "brand/docker.svg";

const BRAND: &[(&str, &[u8])] = &[(DOCKER_LOGO, include_bytes!("../assets/brand/docker.svg"))];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match BRAND.iter().find(|(p, _)| *p == path) {
            Some((_, bytes)) => Ok(Some(Cow::Borrowed(bytes))),
            None => gpui_kit_assets::AllAssets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out = gpui_kit_assets::AllAssets.list(path)?;
        out.extend(
            BRAND
                .iter()
                .filter(|(p, _)| p.starts_with(path))
                .map(|(p, _)| SharedString::from(*p)),
        );
        Ok(out)
    }
}
