//! A logo for each container, found from its image name.

use std::borrow::Cow;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::ActiveTheme;

use super::brand_data::BRANDS;

/// Serves the embedded brand logos at `brand/<slug>.svg`, and all other
/// paths from the icon set of gpui-kit.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(slug) = path.strip_prefix("brand/").and_then(|p| p.strip_suffix(".svg")) {
            return Ok(BRANDS
                .iter()
                .find(|(s, _, _)| *s == slug)
                .map(|(_, _, data)| Cow::Borrowed(*data)));
        }
        gpui_kit_assets::AllAssets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_kit_assets::AllAssets.list(path)
    }
}

/// Image name parts (repository, namespace) to a brand slug.
const ALIASES: &[(&str, &str)] = &[
    ("nginx", "nginx"),
    ("redis", "redis"),
    ("redis-stack", "redis"),
    ("mongo", "mongodb"),
    ("mongodb", "mongodb"),
    ("postgres", "postgresql"),
    ("postgresql", "postgresql"),
    ("postgis", "postgresql"),
    ("mysql", "mysql"),
    ("mariadb", "mariadb"),
    ("alpine", "alpinelinux"),
    ("ubuntu", "ubuntu"),
    ("debian", "debian"),
    ("fedora", "fedora"),
    ("node", "nodedotjs"),
    ("python", "python"),
    ("golang", "go"),
    ("rabbitmq", "rabbitmq"),
    ("elasticsearch", "elasticsearch"),
    ("kibana", "kibana"),
    ("logstash", "logstash"),
    ("opensearch", "opensearch"),
    ("grafana", "grafana"),
    ("prometheus", "prometheus"),
    ("traefik", "traefikproxy"),
    ("caddy", "caddy"),
    ("httpd", "apache"),
    ("tomcat", "apachetomcat"),
    ("docker", "docker"),
    ("dind", "docker"),
    ("azurite", "microsoftazure"),
    ("azure-storage", "microsoftazure"),
    ("dotnet", "dotnet"),
    ("minio", "minio"),
    ("php", "php"),
    ("ruby", "ruby"),
    ("rust", "rust"),
    ("openjdk", "openjdk"),
    ("eclipse-temurin", "openjdk"),
    ("amazoncorretto", "openjdk"),
    ("kafka", "apachekafka"),
    ("keycloak", "keycloak"),
    ("jenkins", "jenkins"),
    ("wordpress", "wordpress"),
    ("nextcloud", "nextcloud"),
    ("bun", "bun"),
    ("deno", "deno"),
    ("localstack", "amazonwebservices"),
    ("influxdb", "influxdb"),
    ("clickhouse", "clickhouse"),
    ("clickhouse-server", "clickhouse"),
    ("gitea", "gitea"),
    ("gitlab", "gitlab"),
    ("gitlab-ce", "gitlab"),
    ("gitlab-ee", "gitlab"),
    ("vault", "vault"),
    ("consul", "consul"),
    ("etcd", "etcd"),
    ("portainer", "portainer"),
    ("portainer-ce", "portainer"),
    ("home-assistant", "homeassistant"),
    ("pihole", "pihole"),
    ("n8n", "n8n"),
    ("ollama", "ollama"),
    ("supabase", "supabase"),
    ("neo4j", "neo4j"),
    ("cassandra", "apachecassandra"),
    ("couchbase", "couchbase"),
    ("couchdb", "apachecouchdb"),
    ("selenium", "selenium"),
    ("playwright", "playwright"),
];

fn alias(name: &str) -> Option<&'static str> {
    ALIASES.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

/// Finds the brand slug for an image reference such as
/// `mcr.microsoft.com/azure-storage/azurite:3.37.0` or `redis:7-alpine`.
/// Tries the repository name, then the namespaces, then the parts of the
/// repository name (so `cp-kafka` finds Kafka). Returns `None` for an
/// image that it does not know, for example a local build.
pub fn brand_for_image(image: &str) -> Option<&'static str> {
    let image = image.split('@').next().unwrap_or(image).to_lowercase();
    let mut parts: Vec<&str> = image.split('/').collect();
    // Remove the tag from the last part.
    if let Some(last) = parts.last_mut() {
        *last = last.split(':').next().unwrap_or(last);
    }
    // Remove the registry host.
    if parts.len() > 1 && (parts[0].contains(['.', ':']) || parts[0] == "localhost") {
        parts.remove(0);
    }
    let (repo, namespaces) = parts.split_last()?;
    alias(repo)
        .or_else(|| namespaces.iter().rev().find_map(|n| alias(n)))
        .or_else(|| repo.split(['-', '_', '.']).find_map(alias))
}

/// Converts `0xRRGGBB` to a color that has contrast with the background.
/// A black or gray brand color becomes the theme text color. A dark color
/// gets lighter on a dark theme, and a light color gets darker on a light
/// theme. The hue does not change.
fn brand_color(rgb: u32, cx: &App) -> Hsla {
    let mut color: Hsla = gpui::rgb(rgb).into();
    let dark = cx.theme().mode.is_dark();
    if color.s < 0.15 && (color.l < 0.3 || color.l > 0.8) {
        return cx.theme().foreground;
    }
    if dark && color.l < 0.5 {
        color.l = 0.6;
    } else if !dark && color.l > 0.7 {
        color.l = 0.45;
    }
    color
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
        let t = cx.theme();
        let brand = brand_for_image(&self.image)
            .and_then(|slug| BRANDS.iter().find(|(s, _, _)| *s == slug));
        let (path, color): (SharedString, Hsla) = match brand {
            Some((slug, rgb, _)) => (format!("brand/{slug}.svg").into(), brand_color(*rgb, cx)),
            None => ("icons/container.svg".into(), t.muted_foreground),
        };
        let badge = self.size * 0.36;
        div()
            .relative()
            .flex_shrink_0()
            .size(self.size)
            .rounded(t.radius)
            // A tint of the logo color, not a dark tile, so the logo stands out.
            .bg(color.opacity(if self.running { 0.28 } else { 0.12 }))
            .border_1()
            .border_color(color.opacity(if self.running { 0.55 } else { 0.25 }))
            .flex()
            .items_center()
            .justify_center()
            .child(
                svg()
                    .path(path)
                    .size(self.size * 0.6)
                    .text_color(color)
                    .when(!self.running, |s| s.opacity(0.55)),
            )
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
    use super::{ALIASES, Assets, BRANDS, brand_for_image};
    use gpui::AssetSource as _;

    #[test]
    fn finds_brands_from_image_names() {
        let cases = [
            ("nginx:alpine", Some("nginx")),
            ("redis:7-alpine", Some("redis")),
            ("library/postgres:16", Some("postgresql")),
            ("docker.io/library/mysql:8", Some("mysql")),
            ("mongodb/mongodb-atlas-local:8", Some("mongodb")),
            ("mcr.microsoft.com/azure-storage/azurite:3.37.0", Some("microsoftazure")),
            ("confluentinc/cp-kafka:7.6.0", Some("apachekafka")),
            ("bitnami/postgresql@sha256:abc", Some("postgresql")),
            ("localhost:5000/team/node:20", Some("nodedotjs")),
            ("enaimco-api-server", None),
            ("ghcr.io/acme/billing", None),
            ("", None),
        ];
        for (image, want) in cases {
            assert_eq!(brand_for_image(image), want, "{image}");
        }
    }

    #[test]
    fn every_alias_has_a_logo() {
        for (_, slug) in ALIASES {
            assert!(BRANDS.iter().any(|(s, _, _)| s == slug), "no logo for {slug}");
        }
    }

    #[test]
    fn asset_source_serves_brand_and_kit_icons() {
        let a = Assets;
        assert!(a.load("brand/nginx.svg").unwrap().is_some());
        assert!(a.load("brand/unknown.svg").unwrap().is_none());
        assert!(a.load("icons/container.svg").unwrap().is_some());
    }
}
