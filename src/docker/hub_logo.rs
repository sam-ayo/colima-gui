//! Image logos from Docker Hub, with a cache on disk.
//!
//! An official image (`library/*`) gets its repository logo. Another
//! Docker Hub image gets the avatar of its publisher. An image from a
//! different registry gets no logo.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Result, bail};
use reqwest::{Client, StatusCode};

/// A cached result that is older than this gets a new download.
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Finds the Docker Hub repository (`namespace/name`) for an image
/// reference such as `redis:7-alpine` or `docker.io/bitnami/redis@sha256:…`.
/// Returns `None` for an image from a different registry.
pub fn hub_repo(image: &str) -> Option<String> {
    let image = image.split('@').next().unwrap_or(image).to_lowercase();
    let mut parts: Vec<&str> = image.split('/').collect();
    if parts.len() > 1 && (parts[0].contains(['.', ':']) || parts[0] == "localhost") {
        if !matches!(parts[0], "docker.io" | "index.docker.io" | "registry-1.docker.io") {
            return None;
        }
        parts.remove(0);
    }
    // Remove the tag from the last part.
    let last = parts.last_mut()?;
    *last = last.split(':').next().unwrap_or(last);
    if parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    match parts.as_slice() {
        [name] => Some(format!("library/{name}")),
        [namespace, name] => Some(format!("{namespace}/{name}")),
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

fn cache_path(repo: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join("Library/Caches/colima-gui/logos").join(repo.replace('/', "__")))
}

/// Returns the logo bytes for `repo`, or `None` if Docker Hub has no logo.
/// An empty cache file records that Docker Hub has no logo. If Docker Hub
/// does not answer, an old cache file is better than no logo.
pub async fn logo(client: &Client, repo: &str) -> Option<Vec<u8>> {
    let path = cache_path(repo);
    let cached = match &path {
        Some(p) => tokio::fs::read(p).await.ok().zip(age(p).await),
        None => None,
    };
    if let Some((bytes, age)) = &cached
        && *age < MAX_AGE
    {
        return (!bytes.is_empty()).then(|| bytes.clone());
    }
    match fetch(client, repo).await {
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
            tracing::debug!("cannot get the logo for {repo}: {e:#}");
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

async fn fetch(client: &Client, repo: &str) -> Result<Option<Vec<u8>>> {
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
        let resp = client.get(&url).send().await?;
        if resp.status() == StatusCode::NOT_FOUND {
            continue;
        }
        if !resp.status().is_success() {
            bail!("{url}: HTTP {}", resp.status());
        }
        let profile: serde_json::Value = resp.json().await?;
        return match profile["gravatar_url"].as_str().and_then(avatar_url) {
            Some(avatar) => get(client, &avatar).await,
            None => Ok(None),
        };
    }
    Ok(None)
}

/// Changes a Gravatar URL to return a 404 in place of the default
/// silhouette, and to use a larger size.
fn avatar_url(gravatar: &str) -> Option<String> {
    let base = gravatar.split('?').next().filter(|b| !b.is_empty())?;
    Some(format!("{base}?s=128&r=g&d=404"))
}

#[cfg(test)]
mod tests {
    use super::{avatar_url, hub_repo};

    #[test]
    fn finds_docker_hub_repos_from_image_names() {
        let cases = [
            ("nginx:alpine", Some("library/nginx")),
            ("Redis:7-alpine", Some("library/redis")),
            ("library/postgres:16", Some("library/postgres")),
            ("docker.io/library/mysql:8", Some("library/mysql")),
            ("mongodb/mongodb-atlas-local:8", Some("mongodb/mongodb-atlas-local")),
            ("confluentinc/cp-kafka:7.6.0", Some("confluentinc/cp-kafka")),
            ("bitnami/postgresql@sha256:abc", Some("bitnami/postgresql")),
            ("enaimco-api-server", Some("library/enaimco-api-server")),
            ("mcr.microsoft.com/azure-storage/azurite:3.37.0", None),
            ("ghcr.io/acme/billing", None),
            ("localhost:5000/team/node:20", None),
            ("a/b/c", None),
            ("", None),
        ];
        for (image, want) in cases {
            assert_eq!(hub_repo(image).as_deref(), want, "{image}");
        }
    }

    #[test]
    fn avatar_url_asks_for_a_404() {
        assert_eq!(
            avatar_url("https://www.gravatar.com/avatar/abc?s=80&r=g&d=mm").as_deref(),
            Some("https://www.gravatar.com/avatar/abc?s=128&r=g&d=404"),
        );
        assert_eq!(avatar_url(""), None);
    }
}
