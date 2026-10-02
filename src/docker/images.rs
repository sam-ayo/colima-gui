//! Image list, pulls, and actions.

use std::collections::HashMap;

use anyhow::Result;
use bollard::Docker;
use bollard::models::{CreateImageInfo, ImageSummary};
use bollard::query_parameters::{CreateImageOptions, ListImagesOptions, PruneImagesOptions, RemoveImageOptions};
use futures::{Stream, StreamExt};
use indexmap::IndexMap;

use super::containers::ICON_LABEL;

/// The tag that Docker gives to an image with no tag in the list data.
const NONE_TAG: &str = "<none>:<none>";

/// The part of an image that the list shows.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageRow {
    /// The full ID, for example "sha256:858f009f…".
    pub id: String,
    /// The references of the image, for example "redis:7-alpine". An
    /// untagged image has none.
    pub tags: Vec<String>,
    pub digests: Vec<String>,
    /// The time in seconds since the Unix epoch.
    pub created: i64,
    /// The disk use, with the layers that other images share.
    pub size: u64,
    pub icon: Option<String>,
}

impl ImageRow {
    /// The first 12 characters of the ID, without "sha256:", as
    /// `docker images` shows.
    pub fn short_id(&self) -> &str {
        short_id(&self.id)
    }
}

pub fn short_id(id: &str) -> &str {
    let hex = id.strip_prefix("sha256:").unwrap_or(id);
    &hex[..hex.len().min(12)]
}

impl From<ImageSummary> for ImageRow {
    fn from(i: ImageSummary) -> Self {
        let mut tags: Vec<String> = i.repo_tags.into_iter().filter(|t| t != NONE_TAG).collect();
        tags.sort();
        ImageRow {
            id: i.id,
            tags,
            digests: i.repo_digests,
            created: i.created,
            size: u64::try_from(i.size).unwrap_or(0),
            icon: i.labels.get(ICON_LABEL).cloned(),
        }
    }
}

/// Splits a reference such as "ghcr.io/owner/app:v2" into the repository
/// and the tag. The colon of a registry port is not a tag separator.
pub fn split_reference(reference: &str) -> (&str, Option<&str>) {
    let name_start = reference.rfind('/').map_or(0, |i| i + 1);
    match reference[name_start..].rfind(':') {
        Some(i) => (&reference[..name_start + i], Some(&reference[name_start + i + 1..])),
        None => (reference, None),
    }
}

/// Writes a reference in the form of the image list: no "docker.io/" or
/// "library/" part, and the "latest" tag when there is no tag or digest.
pub fn normalize_reference(reference: &str) -> String {
    let mut r = reference.trim();
    for prefix in ["docker.io/", "index.docker.io/", "registry-1.docker.io/"] {
        if let Some(rest) = r.strip_prefix(prefix) {
            r = rest;
            break;
        }
    }
    if let Some(rest) = r.strip_prefix("library/") {
        r = rest;
    }
    if r.contains('@') || split_reference(r).1.is_some() {
        r.to_string()
    } else {
        format!("{r}:latest")
    }
}

/// Checks a reference before a pull. The engine gives the final answer,
/// but its message for a bad name is hard to read.
pub fn check_reference(reference: &str) -> std::result::Result<(), &'static str> {
    let r = reference.trim();
    if r.is_empty() {
        return Err("Type the name of an image.");
    }
    if r.chars().any(char::is_whitespace) {
        return Err("The name must not have spaces.");
    }
    let name = r.split('@').next().unwrap_or(r);
    let (repo, tag) = split_reference(name);
    // The registry host can have capitals. The path cannot.
    let path = match repo.split_once('/') {
        Some((host, rest)) if host.contains(['.', ':']) || host == "localhost" => rest,
        _ => repo,
    };
    if path.is_empty() || path.split('/').any(str::is_empty) {
        return Err("The name has an empty part.");
    }
    if path.chars().any(|c| c.is_ascii_uppercase()) {
        return Err("The repository name must be in lowercase.");
    }
    if !path.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-')) {
        return Err("Use only letters, digits, “/”, “.”, “_”, and “-” in the repository name.");
    }
    if tag.is_some_and(|t| t.is_empty()) {
        return Err("Type a tag after the “:”, or remove the “:”.");
    }
    Ok(())
}

pub async fn list_all(docker: &Docker) -> Result<Vec<ImageRow>> {
    let list = docker.list_images(None::<ListImagesOptions>).await?;
    Ok(list.into_iter().map(ImageRow::from).collect())
}

/// The part of the inspect data that the list does not have.
pub struct ImageInfo {
    /// For example "linux/arm64/v8".
    pub platform: Option<String>,
    pub author: Option<String>,
    pub entrypoint: Option<String>,
    pub command: Option<String>,
    pub working_dir: Option<String>,
    pub user: Option<String>,
    pub exposed_ports: Vec<String>,
    pub env: Vec<(String, String)>,
    pub labels: Vec<(String, String)>,
    /// The full inspect data as pretty JSON.
    pub json: String,
}

/// Writes a command list as a shell would show it. A part with a space
/// gets quotes.
fn join_command(parts: &[String]) -> String {
    parts
        .iter()
        .map(|p| if p.is_empty() || p.contains(char::is_whitespace) { format!("\"{p}\"") } else { p.clone() })
        .collect::<Vec<_>>()
        .join(" ")
}

pub async fn inspect(docker: &Docker, id: &str) -> Result<ImageInfo> {
    let image = docker.inspect_image(id).await?;
    let value = serde_json::to_value(&image)?;
    let config = image.config.unwrap_or_default();
    let platform = [image.os, image.architecture, image.variant]
        .into_iter()
        .flatten()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>();
    let mut labels: Vec<(String, String)> = config.labels.unwrap_or_default().into_iter().collect();
    labels.sort();
    let mut exposed_ports = config.exposed_ports.unwrap_or_default();
    exposed_ports.sort();
    let some = |s: Option<String>| s.filter(|s| !s.is_empty());
    Ok(ImageInfo {
        platform: (!platform.is_empty()).then(|| platform.join("/")),
        author: some(image.author),
        entrypoint: config.entrypoint.filter(|e| !e.is_empty()).map(|e| join_command(&e)),
        command: config.cmd.filter(|c| !c.is_empty()).map(|c| join_command(&c)),
        working_dir: some(config.working_dir),
        user: some(config.user),
        exposed_ports,
        env: config
            .env
            .unwrap_or_default()
            .into_iter()
            .map(|e| match e.split_once('=') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (e, String::new()),
            })
            .collect(),
        labels,
        json: serde_json::to_string_pretty(&value)?,
    })
}

/// One step of the build history of an image.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    /// The time in seconds since the Unix epoch.
    pub created: i64,
    /// The Dockerfile instruction, without the shell prefix.
    pub command: String,
    pub size: u64,
}

/// Removes the shell prefix that the classic builder writes before each
/// instruction, so "/bin/sh -c #(nop)  CMD [\"sh\"]" gives "CMD [\"sh\"]".
/// BuildKit writes "RUN /bin/sh -c apk add … # buildkit". Each run of
/// spaces, tabs, and line breaks becomes one space, because a Dockerfile
/// step that continues on many lines keeps its indents in the history.
fn clean_command(command: &str) -> String {
    let c = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let c = c.as_str();
    let c = c.strip_suffix("# buildkit").unwrap_or(c).trim_end();
    if let Some(rest) = c.strip_prefix("/bin/sh -c #(nop)") {
        return rest.trim().to_string();
    }
    if let Some(rest) = c.strip_prefix("/bin/sh -c ") {
        return format!("RUN {}", rest.trim());
    }
    if let Some(rest) = c.strip_prefix("RUN /bin/sh -c ") {
        return format!("RUN {}", rest.trim());
    }
    c.to_string()
}

/// The build steps, the newest first, as `docker history` shows them.
pub async fn history(docker: &Docker, id: &str) -> Result<Vec<Layer>> {
    let items = docker.image_history(id).await?;
    Ok(items
        .into_iter()
        .map(|i| Layer {
            created: i.created,
            command: clean_command(&i.created_by),
            size: u64::try_from(i.size).unwrap_or(0),
        })
        .collect())
}

/// Removes a reference. If the image has other tags, Docker removes only
/// this tag. Otherwise Docker deletes the image. `reference` can also be
/// an image ID.
pub async fn remove(docker: &Docker, reference: &str) -> Result<()> {
    docker.remove_image(reference, None::<RemoveImageOptions>, None).await?;
    Ok(())
}

/// The result of a prune: the number of deleted images and the bytes that
/// came free.
pub struct Pruned {
    pub count: usize,
    pub bytes: u64,
}

/// Deletes all the images that no container uses, also the tagged images.
/// Without `dangling=false`, the engine deletes only the untagged images.
pub async fn prune(docker: &Docker) -> Result<Pruned> {
    let opts = PruneImagesOptions {
        filters: Some(HashMap::from([("dangling".to_string(), vec!["false".to_string()])])),
    };
    let result = docker.prune_images(Some(opts)).await?;
    Ok(Pruned {
        count: result.images_deleted.iter().flatten().filter(|i| i.deleted.is_some()).count(),
        bytes: result.space_reclaimed.and_then(|b| u64::try_from(b).ok()).unwrap_or(0),
    })
}

/// Pulls an image. The stream gives the progress messages of the engine,
/// and ends after the last one. An error ends the pull.
pub fn pull(docker: &Docker, reference: &str) -> impl Stream<Item = Result<CreateImageInfo, String>> + Send + 'static {
    let opts = CreateImageOptions {
        from_image: Some(reference.to_string()),
        ..Default::default()
    };
    docker
        .create_image(Some(opts), None, None)
        .map(|r| {
            r.map_err(|e| match e {
                // The message of the engine, without the "Docker stream
                // error" prefix.
                bollard::errors::Error::DockerStreamError { error } => error,
                e => e.to_string(),
            })
        })
}

/// The state of one layer in a pull.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct LayerProgress {
    current: u64,
    total: u64,
    downloaded: bool,
}

/// The progress of a pull, from the messages of the engine.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PullProgress {
    layers: IndexMap<String, LayerProgress>,
    /// The last message that is not about one layer, for example
    /// "Pulling from library/nginx".
    pub status: Option<String>,
}

impl PullProgress {
    pub fn apply(&mut self, info: &CreateImageInfo) {
        let Some(status) = info.status.as_deref() else { return };
        let layer_status = matches!(
            status,
            "Pulling fs layer"
                | "Waiting"
                | "Downloading"
                | "Verifying Checksum"
                | "Download complete"
                | "Extracting"
                | "Pull complete"
                | "Already exists"
        );
        let Some(id) = info.id.as_ref().filter(|_| layer_status) else {
            self.status = Some(status.to_string());
            return;
        };
        let layer = self.layers.entry(id.clone()).or_default();
        match status {
            "Downloading" => {
                if let Some(d) = &info.progress_detail {
                    layer.current = d.current.and_then(|c| u64::try_from(c).ok()).unwrap_or(layer.current);
                    layer.total = d.total.and_then(|t| u64::try_from(t).ok()).unwrap_or(layer.total);
                }
            }
            "Verifying Checksum" | "Download complete" | "Extracting" | "Pull complete" | "Already exists" => {
                layer.downloaded = true;
                layer.current = layer.total;
            }
            _ => {}
        }
    }

    /// The bytes that came, and the size of the layers with a known size.
    pub fn bytes(&self) -> (u64, u64) {
        self.layers
            .values()
            .filter(|l| l.total > 0)
            .fold((0, 0), |(c, t), l| (c + l.current.min(l.total), t + l.total))
    }

    /// The part of the pull that is done, from 0 to 1. Each layer has the
    /// same weight, because a waiting layer has no size yet. `None` until
    /// the engine names the layers.
    pub fn fraction(&self) -> Option<f32> {
        if self.layers.is_empty() {
            return None;
        }
        let done: f32 = self
            .layers
            .values()
            .map(|l| match l {
                l if l.downloaded => 1.,
                l if l.total > 0 => l.current as f32 / l.total as f32,
                _ => 0.,
            })
            .sum();
        Some((done / self.layers.len() as f32).clamp(0., 1.))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::ProgressDetail;

    #[test]
    fn references_split_at_the_tag() {
        assert_eq!(split_reference("nginx:alpine"), ("nginx", Some("alpine")));
        assert_eq!(split_reference("nginx"), ("nginx", None));
        assert_eq!(split_reference("localhost:5000/app"), ("localhost:5000/app", None));
        assert_eq!(split_reference("localhost:5000/app:v1"), ("localhost:5000/app", Some("v1")));
        assert_eq!(
            split_reference("ghcr.io/google/osv-scanner:v2.6.0"),
            ("ghcr.io/google/osv-scanner", Some("v2.6.0"))
        );
    }

    #[test]
    fn references_normalize_as_the_list_shows_them() {
        assert_eq!(normalize_reference("nginx"), "nginx:latest");
        assert_eq!(normalize_reference(" docker.io/library/redis:7 "), "redis:7");
        assert_eq!(normalize_reference("library/alpine"), "alpine:latest");
        assert_eq!(normalize_reference("ghcr.io/owner/app"), "ghcr.io/owner/app:latest");
        assert_eq!(normalize_reference("alpine@sha256:abc"), "alpine@sha256:abc");
    }

    #[test]
    fn references_are_checked() {
        assert!(check_reference("nginx").is_ok());
        assert!(check_reference("ghcr.io/owner/app:v1.2").is_ok());
        assert!(check_reference("Registry.Example.com:5000/app").is_ok());
        assert!(check_reference("").is_err());
        assert!(check_reference("my image").is_err());
        assert!(check_reference("Nginx").is_err());
        assert!(check_reference("nginx:").is_err());
        assert!(check_reference("owner//app").is_err());
    }

    #[test]
    fn short_ids_drop_the_algorithm() {
        assert_eq!(short_id("sha256:858f009f9709ce576febc734"), "858f009f9709");
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn history_commands_lose_the_shell_prefix() {
        assert_eq!(clean_command("/bin/sh -c #(nop)  CMD [\"sh\"]"), "CMD [\"sh\"]");
        assert_eq!(clean_command("/bin/sh -c apk add curl"), "RUN apk add curl");
        assert_eq!(clean_command("RUN /bin/sh -c npm ci # buildkit"), "RUN npm ci");
        assert_eq!(clean_command("COPY . /app # buildkit"), "COPY . /app");
        assert_eq!(
            clean_command("RUN /bin/sh -c apk add \\\n\t    curl  && echo ok # buildkit"),
            "RUN apk add \\ curl && echo ok"
        );
    }

    fn info(id: Option<&str>, status: &str, progress: Option<(i64, i64)>) -> CreateImageInfo {
        CreateImageInfo {
            id: id.map(Into::into),
            status: Some(status.into()),
            progress_detail: progress.map(|(current, total)| ProgressDetail {
                current: Some(current),
                total: Some(total),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn pull_progress_counts_each_layer() {
        let mut p = PullProgress::default();
        p.apply(&info(Some("alpine"), "Pulling from library/nginx", None));
        assert_eq!(p.status.as_deref(), Some("Pulling from library/nginx"));
        assert_eq!(p.fraction(), None);
        p.apply(&info(Some("a"), "Pulling fs layer", None));
        p.apply(&info(Some("b"), "Already exists", None));
        p.apply(&info(Some("a"), "Downloading", Some((50, 100))));
        assert_eq!(p.fraction(), Some(0.75));
        assert_eq!(p.bytes(), (50, 100));
        p.apply(&info(Some("a"), "Extracting", Some((10, 400))));
        p.apply(&info(Some("a"), "Pull complete", None));
        assert_eq!(p.fraction(), Some(1.));
        assert_eq!(p.bytes(), (100, 100));
        p.apply(&info(None, "Status: Downloaded newer image for nginx:alpine", None));
        assert_eq!(p.status.as_deref(), Some("Status: Downloaded newer image for nginx:alpine"));
    }
}
