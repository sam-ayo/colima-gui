//! Container logos from public sources, with a cache on disk.
//!
//! The sources, in order:
//! 1. The `dev.orbstack.icon` label, an icon URL that the image or the
//!    Compose file sets.
//! 2. Docker Hub: the repository logo of an official image, or the avatar
//!    of the publisher.
//! 3. The GitHub avatar of the owner of a `ghcr.io` image, or the Quay
//!    avatar of the namespace of a `quay.io` image.
//! 4. The GitHub avatar of the company that runs the registry, for
//!    registries such as `mcr.microsoft.com`.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Result, bail};
use reqwest::{Client, StatusCode};

/// A cached result that is older than this gets a new download.
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

const DOCKER_HUB_HOSTS: &[&str] = &["docker.io", "index.docker.io", "registry-1.docker.io"];

/// Registries that one company runs, to the GitHub account of the company.
const REGISTRY_OWNERS: &[(&str, &str)] = &[
    ("mcr.microsoft.com", "microsoft"),
    ("registry.k8s.io", "kubernetes"),
    ("k8s.gcr.io", "kubernetes"),
    ("public.ecr.aws", "aws"),
    ("docker.elastic.co", "elastic"),
    ("container-registry.oracle.com", "oracle"),
    ("registry.access.redhat.com", "RedHatOfficial"),
    ("registry.redhat.io", "RedHatOfficial"),
    ("nvcr.io", "NVIDIA"),
    ("cgr.dev", "chainguard-dev"),
];

/// The place to get the logo of a container from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// An icon URL from the container label.
    Url(String),
    /// A Docker Hub repository, `namespace/name`.
    Hub(String),
    /// A GitHub account.
    GitHub(String),
    /// A Quay namespace.
    Quay(String),
}

impl Source {
    /// Finds the source for a container. `icon` is the value of the
    /// `dev.orbstack.icon` label. Returns `None` for an image from an
    /// unknown registry.
    pub fn find(image: &str, icon: Option<&str>) -> Option<Source> {
        if let Some(url) = icon.map(str::trim)
            && (url.starts_with("https://") || url.starts_with("http://"))
        {
            return Some(Source::Url(url.to_string()));
        }
        let image = image.split('@').next().unwrap_or(image).to_lowercase();
        let mut parts: Vec<&str> = image.split('/').collect();
        // Remove the tag from the last part.
        let last = parts.last_mut()?;
        *last = last.split(':').next().unwrap_or(last);
        if parts.iter().any(|p| p.is_empty()) {
            return None;
        }
        let host = (parts.len() > 1 && (parts[0].contains(['.', ':']) || parts[0] == "localhost"))
            .then(|| parts.remove(0));
        match (host, parts.as_slice()) {
            (None, path) => hub(path),
            (Some(h), path) if DOCKER_HUB_HOSTS.contains(&h) => hub(path),
            // Amazon copies the official Docker Hub images here.
            (Some("public.ecr.aws"), ["docker", "library", name]) => {
                Some(Source::Hub(format!("library/{name}")))
            }
            (Some("ghcr.io"), [owner, _, ..]) => Some(Source::GitHub(owner.to_string())),
            (Some("quay.io"), [namespace, _, ..]) => Some(Source::Quay(namespace.to_string())),
            (Some(h), _) => REGISTRY_OWNERS
                .iter()
                .find(|(host, _)| *host == h)
                .map(|(_, owner)| Source::GitHub(owner.to_string())),
        }
    }

    /// The name of the cache file.
    fn cache_key(&self) -> String {
        match self {
            Source::Url(url) => {
                let mut h = DefaultHasher::new();
                url.hash(&mut h);
                format!("url__{:016x}", h.finish())
            }
            Source::Hub(repo) => format!("hub__{}", repo.replace('/', "__")),
            Source::GitHub(owner) => format!("github__{owner}"),
            Source::Quay(namespace) => format!("quay__{namespace}"),
        }
    }
}

/// A Docker Hub path has one part (an official image) or two parts.
fn hub(path: &[&str]) -> Option<Source> {
    match path {
        [name] => Some(Source::Hub(format!("library/{name}"))),
        [namespace, name] => Some(Source::Hub(format!("{namespace}/{name}"))),
        _ => None,
    }
}

pub fn client() -> Client {
    Client::builder()
        .user_agent(concat!("colima-gui/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(15))
        .build()
        .expect("failed to make the HTTP client")
}

fn cache_path(source: &Source) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join("Library/Caches/colima-gui/logos").join(source.cache_key()))
}

/// Returns the logo bytes for `source`, or `None` if there is no logo.
/// An empty cache file records that there is no logo. If the source does
/// not answer, an old cache file is better than no logo.
pub async fn logo(client: &Client, source: &Source) -> Option<Vec<u8>> {
    let path = cache_path(source);
    let cached = match &path {
        Some(p) => tokio::fs::read(p).await.ok().zip(age(p).await),
        None => None,
    };
    if let Some((bytes, age)) = &cached
        && *age < MAX_AGE
    {
        return (!bytes.is_empty()).then(|| bytes.clone());
    }
    match fetch(client, source).await {
        Ok(bytes) => {
            if let Some(p) = &path {
                let data = bytes.as_deref().unwrap_or_default();
                if let Some(dir) = p.parent() {
                    let _ = tokio::fs::create_dir_all(dir).await;
                }
                if let Err(e) = tokio::fs::write(p, data).await {
                    tracing::warn!("cannot write the logo cache {}: {e}", p.display());
                }
            }
            bytes
        }
        Err(e) => {
            tracing::debug!("cannot get the logo for {source:?}: {e:#}");
            cached.map(|(bytes, _)| bytes).filter(|b| !b.is_empty())
        }
    }
}

async fn age(path: &PathBuf) -> Option<Duration> {
    let modified = tokio::fs::metadata(path).await.ok()?.modified().ok()?;
    Some(SystemTime::now().duration_since(modified).unwrap_or_default())
}

/// Gets the image at `url`. Returns `None` for a 404.
async fn get(client: &Client, url: &str) -> Result<Option<Vec<u8>>> {
    let resp = client.get(url).send().await?;
    match resp.status() {
        StatusCode::NOT_FOUND => Ok(None),
        s if s.is_success() => Ok(Some(resp.bytes().await?.to_vec())),
        s => bail!("{url}: HTTP {s}"),
    }
}

/// Gets the JSON at `url`. Returns `None` for a 404.
async fn get_json(client: &Client, url: &str) -> Result<Option<serde_json::Value>> {
    let resp = client.get(url).send().await?;
    match resp.status() {
        StatusCode::NOT_FOUND => Ok(None),
        s if s.is_success() => Ok(Some(resp.json().await?)),
        s => bail!("{url}: HTTP {s}"),
    }
}

async fn fetch(client: &Client, source: &Source) -> Result<Option<Vec<u8>>> {
    match source {
        Source::Url(url) => get(client, url).await,
        Source::Hub(repo) => fetch_hub(client, repo).await,
        Source::GitHub(owner) => get(client, &format!("https://github.com/{owner}.png?size=128")).await,
        Source::Quay(namespace) => {
            let url = format!("https://quay.io/api/v1/organization/{namespace}");
            let hash = get_json(client, &url).await?;
            match hash.as_ref().and_then(|p| p["avatar"]["hash"].as_str()) {
                Some(hash) => get(client, &gravatar(hash)).await,
                None => Ok(None),
            }
        }
    }
}

async fn fetch_hub(client: &Client, repo: &str) -> Result<Option<Vec<u8>>> {
    let url = format!(
        "https://hub.docker.com/api/media/repos_logo/v1/{}",
        repo.replace('/', "%2F")
    );
    if let Some(bytes) = get(client, &url).await? {
        return Ok(Some(bytes));
    }
    let Some((namespace, _)) = repo.split_once('/') else {
        return Ok(None);
    };
    if namespace == "library" {
        return Ok(None);
    }
    for kind in ["orgs", "users"] {
        let url = format!("https://hub.docker.com/v2/{kind}/{namespace}/");
        let Some(profile) = get_json(client, &url).await? else {
            continue;
        };
        return match profile["gravatar_url"].as_str().and_then(gravatar_hash) {
            Some(hash) => get(client, &gravatar(hash)).await,
            None => Ok(None),
        };
    }
    Ok(None)
}

/// Finds the hash in a Gravatar URL such as
/// `https://www.gravatar.com/avatar/<hash>?s=80&r=g&d=mm`.
fn gravatar_hash(url: &str) -> Option<&str> {
    let path = url.split('?').next()?;
    path.rsplit_once("/avatar/").map(|(_, h)| h).filter(|h| !h.is_empty())
}

/// A Gravatar URL that returns a 404 in place of the default silhouette.
fn gravatar(hash: &str) -> String {
    format!("https://www.gravatar.com/avatar/{hash}?s=128&r=g&d=404")
}

#[cfg(test)]
mod tests {
    use super::{Source, client, fetch, gravatar_hash};

    fn hub(repo: &str) -> Option<Source> {
        Some(Source::Hub(repo.into()))
    }

    fn github(owner: &str) -> Option<Source> {
        Some(Source::GitHub(owner.into()))
    }

    #[test]
    fn finds_sources_from_image_names() {
        let cases = [
            ("nginx:alpine", hub("library/nginx")),
            ("Redis:7-alpine", hub("library/redis")),
            ("library/postgres:16", hub("library/postgres")),
            ("docker.io/library/mysql:8", hub("library/mysql")),
            ("mongodb/mongodb-atlas-local:8", hub("mongodb/mongodb-atlas-local")),
            ("bitnami/postgresql@sha256:abc", hub("bitnami/postgresql")),
            ("enaimco-api-server", hub("library/enaimco-api-server")),
            ("public.ecr.aws/docker/library/node:20", hub("library/node")),
            ("ghcr.io/home-assistant/home-assistant:stable", github("home-assistant")),
            ("mcr.microsoft.com/azure-storage/azurite:3.37.0", github("microsoft")),
            ("docker.elastic.co/elasticsearch/elasticsearch:8", github("elastic")),
            ("quay.io/prometheus/node-exporter", Some(Source::Quay("prometheus".into()))),
            ("localhost:5000/team/node:20", None),
            ("registry.example.com/acme/billing", None),
            ("ghcr.io/acme", None),
            ("a/b/c", None),
            ("", None),
        ];
        for (image, want) in cases {
            assert_eq!(Source::find(image, None), want, "{image}");
        }
    }

    #[test]
    fn the_icon_label_comes_first() {
        let url = "https://example.com/logo.png";
        assert_eq!(Source::find("redis", Some(url)), Some(Source::Url(url.into())));
        assert_eq!(Source::find("redis", Some("logo.png")), hub("library/redis"));
    }

    #[test]
    fn finds_gravatar_hashes() {
        assert_eq!(gravatar_hash("https://www.gravatar.com/avatar/abc?s=80&r=g&d=mm"), Some("abc"));
        assert_eq!(gravatar_hash(""), None);
    }

    #[tokio::test]
    #[ignore = "needs the network"]
    async fn fetches_a_logo_from_each_source() {
        let c = client();
        let found = [
            Source::Url("https://github.com/docker.png?size=64".into()),
            Source::Hub("library/redis".into()),
            Source::Hub("confluentinc/cp-kafka".into()),
            Source::GitHub("home-assistant".into()),
            Source::Quay("prometheus".into()),
        ];
        for source in found {
            let bytes = fetch(&c, &source).await.unwrap();
            assert!(bytes.is_some_and(|b| !b.is_empty()), "no logo for {source:?}");
        }
        let missing = Source::Hub("library/no-such-image-cgui".into());
        assert_eq!(fetch(&c, &missing).await.unwrap(), None);
    }
}
