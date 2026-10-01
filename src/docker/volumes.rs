//! Volume list, sizes, and actions.

use std::collections::HashMap;

use anyhow::Result;
use bollard::Docker;
use bollard::models::{Volume, VolumeCreateRequest};
use bollard::query_parameters::{DataUsageOptions, PruneVolumesOptions, RemoveVolumeOptions};

use super::containers::COMPOSE_PROJECT_LABEL;

/// The name of the volume in the Compose file, without the project prefix.
pub const COMPOSE_VOLUME_LABEL: &str = "com.docker.compose.volume";
/// Docker sets this label on a volume that has no name in the run command
/// or in the image. The name of such a volume is a random hex string.
pub const ANONYMOUS_LABEL: &str = "com.docker.volume.anonymous";

/// The part of a volume that the list shows.
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeRow {
    pub name: String,
    pub driver: String,
    /// The path in the Colima VM, not on the Mac.
    pub mountpoint: String,
    pub compose_project: Option<String>,
    pub compose_volume: Option<String>,
    pub anonymous: bool,
}

impl VolumeRow {
    /// The name that the list shows. A Compose volume shows its name in
    /// the Compose file. An anonymous volume shows the first 12 characters,
    /// as a container ID does.
    pub fn display_name(&self, grouped: bool) -> &str {
        if grouped && let Some(v) = &self.compose_volume {
            return v;
        }
        if self.anonymous {
            return &self.name[..self.name.len().min(12)];
        }
        &self.name
    }
}

impl From<Volume> for VolumeRow {
    fn from(v: Volume) -> Self {
        VolumeRow {
            compose_project: v.labels.get(COMPOSE_PROJECT_LABEL).cloned(),
            compose_volume: v.labels.get(COMPOSE_VOLUME_LABEL).cloned(),
            anonymous: v.labels.contains_key(ANONYMOUS_LABEL),
            name: v.name,
            driver: v.driver,
            mountpoint: v.mountpoint,
        }
    }
}

pub async fn list_all(docker: &Docker) -> Result<Vec<VolumeRow>> {
    let list = docker.list_volumes(None::<bollard::query_parameters::ListVolumesOptions>).await?;
    Ok(list.volumes.unwrap_or_default().into_iter().map(VolumeRow::from).collect())
}

/// Reads the disk use of each volume, in bytes. The engine measures each
/// volume, so this call can take some seconds. A volume that the engine
/// cannot measure is not in the map.
pub async fn sizes(docker: &Docker) -> Result<HashMap<String, u64>> {
    // `verbose` gives the volume items in the format of API 1.52 and later.
    // Bollard cannot encode the `type` list in the query, so the engine
    // also measures the images and the containers.
    let opts = DataUsageOptions {
        _type: None,
        verbose: true,
    };
    let usage = docker.df(Some(opts)).await?;
    let items = usage.volume_usage.and_then(|u| u.items).unwrap_or_default();
    Ok(parse_sizes(items))
}

fn parse_sizes(items: Vec<serde_json::Value>) -> HashMap<String, u64> {
    items
        .into_iter()
        .filter_map(|item| {
            let name = item.get("Name")?.as_str()?.to_string();
            // The engine writes -1 for a size that it does not know.
            let size = item.get("UsageData")?.get("Size")?.as_i64()?;
            Some((name, u64::try_from(size).ok()?))
        })
        .collect()
}

/// Checks a volume name with the rule of the engine: a letter or a digit
/// first, then letters, digits, `_`, `.`, or `-`. The engine needs at least
/// two characters.
pub fn check_name(name: &str) -> std::result::Result<(), &'static str> {
    let mut chars = name.chars();
    match chars.next() {
        None => return Err("Type a name."),
        Some(c) if !c.is_ascii_alphanumeric() => return Err("The name must start with a letter or a digit."),
        _ => {}
    }
    if name.len() < 2 {
        return Err("The name must have at least two characters.");
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')) {
        return Err("Use only letters, digits, “_”, “.”, and “-”.");
    }
    Ok(())
}

/// Makes a volume with the local driver.
pub async fn create(docker: &Docker, name: &str) -> Result<VolumeRow> {
    let request = VolumeCreateRequest {
        name: Some(name.to_string()),
        ..Default::default()
    };
    Ok(docker.create_volume(request).await?.into())
}

/// The part of the inspect data that the list does not have.
pub struct VolumeInfo {
    /// The time in the format of the engine, for example
    /// "2026-09-30T17:28:05-02:30".
    pub created: Option<String>,
    pub labels: Vec<(String, String)>,
    pub options: Vec<(String, String)>,
    /// The full inspect data as pretty JSON.
    pub json: String,
}

pub async fn inspect(docker: &Docker, name: &str) -> Result<VolumeInfo> {
    let volume = docker.inspect_volume(name).await?;
    // Read the time from the JSON, because its Rust type changes with the
    // features of bollard.
    let value = serde_json::to_value(&volume)?;
    let sorted = |map: &HashMap<String, String>| {
        let mut list: Vec<(String, String)> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        list.sort();
        list
    };
    Ok(VolumeInfo {
        created: value.get("CreatedAt").and_then(|v| v.as_str()).map(str::to_string),
        labels: sorted(&volume.labels),
        options: sorted(&volume.options),
        json: serde_json::to_string_pretty(&value)?,
    })
}

pub async fn remove(docker: &Docker, name: &str) -> Result<()> {
    docker.remove_volume(name, None::<RemoveVolumeOptions>).await?;
    Ok(())
}

/// The result of a prune: the number of deleted volumes and the bytes that
/// came free.
pub struct Pruned {
    pub count: usize,
    pub bytes: u64,
}

/// Deletes all the volumes that no container uses, also the named volumes.
/// Without `all`, API 1.42 and later delete only the anonymous volumes.
pub async fn prune(docker: &Docker) -> Result<Pruned> {
    let opts = PruneVolumesOptions {
        filters: Some(HashMap::from([("all".to_string(), vec!["true".to_string()])])),
    };
    let result = docker.prune_volumes(Some(opts)).await?;
    Ok(Pruned {
        count: result.volumes_deleted.map_or(0, |v| v.len()),
        bytes: result.space_reclaimed.and_then(|b| u64::try_from(b).ok()).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_skip_unknown_values() {
        let items = serde_json::json!([
            {"Name": "db", "UsageData": {"RefCount": 1, "Size": 4021}},
            {"Name": "cache", "UsageData": {"RefCount": 0, "Size": -1}},
            {"Name": "tmp"},
        ]);
        let serde_json::Value::Array(items) = items else { unreachable!() };
        assert_eq!(parse_sizes(items), HashMap::from([("db".to_string(), 4021)]));
    }

    #[test]
    fn names_follow_the_engine_rule() {
        assert!(check_name("db_data").is_ok());
        assert!(check_name("a1.b-c").is_ok());
        assert!(check_name("").is_err());
        assert!(check_name("a").is_err());
        assert!(check_name("_db").is_err());
        assert!(check_name("my data").is_err());
        assert!(check_name("dät").is_err());
    }

    #[test]
    fn display_names() {
        let mut row = VolumeRow {
            name: "shop_db_data".into(),
            driver: "local".into(),
            mountpoint: String::new(),
            compose_project: Some("shop".into()),
            compose_volume: Some("db_data".into()),
            anonymous: false,
        };
        assert_eq!(row.display_name(true), "db_data");
        assert_eq!(row.display_name(false), "shop_db_data");
        row = VolumeRow {
            name: "0ad985de1aa2db606c3f26bf280e756b".into(),
            compose_project: None,
            compose_volume: None,
            anonymous: true,
            ..row
        };
        assert_eq!(row.display_name(false), "0ad985de1aa2");
    }
}
