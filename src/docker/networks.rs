//! Network list and actions.

use std::collections::HashMap;

use anyhow::Result;
use bollard::Docker;
use bollard::models::{Ipam, Network, NetworkCreateRequest};
use bollard::query_parameters::{ListNetworksOptions, PruneNetworksOptions};

use super::containers::COMPOSE_PROJECT_LABEL;

/// The name of the network in the Compose file, without the project prefix.
pub const COMPOSE_NETWORK_LABEL: &str = "com.docker.compose.network";

/// The networks that Docker makes itself. Docker refuses to remove them,
/// and a prune does not touch them.
pub const BUILT_IN: [&str; 3] = ["bridge", "host", "none"];

/// The part of a network that the list shows.
#[derive(Clone, Debug, PartialEq)]
pub struct NetworkRow {
    pub id: String,
    pub name: String,
    pub driver: String,
    /// The subnets of the network, IPv4 first, for example "172.18.0.0/16".
    pub subnets: Vec<String>,
    pub gateways: Vec<String>,
    /// A container on an internal network cannot reach outside networks.
    pub internal: bool,
    pub ipv6: bool,
    pub compose_project: Option<String>,
    pub compose_network: Option<String>,
}

impl NetworkRow {
    pub fn short_id(&self) -> &str {
        &self.id[..self.id.len().min(12)]
    }

    pub fn is_built_in(&self) -> bool {
        BUILT_IN.contains(&self.name.as_str())
    }

    /// The name that the list shows. A Compose network shows its name in
    /// the Compose file.
    pub fn display_name(&self, grouped: bool) -> &str {
        match &self.compose_network {
            Some(n) if grouped => n,
            _ => &self.name,
        }
    }
}

/// The subnets and the gateways of the IPAM config, IPv4 first.
fn addresses(ipam: Option<&Ipam>) -> (Vec<String>, Vec<String>) {
    let configs = ipam.and_then(|i| i.config.as_deref()).unwrap_or_default();
    let mut subnets: Vec<String> = configs.iter().filter_map(|c| c.subnet.clone()).collect();
    let mut gateways: Vec<String> = configs.iter().filter_map(|c| c.gateway.clone()).collect();
    // An IPv6 address has colons. The sort is stable, so IPv4 comes first
    // and each family keeps the order of the engine.
    subnets.sort_by_key(|s| s.contains(':'));
    gateways.sort_by_key(|g| g.contains(':'));
    (subnets, gateways)
}

impl From<Network> for NetworkRow {
    fn from(n: Network) -> Self {
        let labels = n.labels.unwrap_or_default();
        let (subnets, gateways) = addresses(n.ipam.as_ref());
        NetworkRow {
            id: n.id.unwrap_or_default(),
            name: n.name.unwrap_or_default(),
            driver: n.driver.unwrap_or_default(),
            subnets,
            gateways,
            internal: n.internal.unwrap_or(false),
            ipv6: n.enable_ipv6.unwrap_or(false),
            compose_project: labels.get(COMPOSE_PROJECT_LABEL).cloned(),
            compose_network: labels.get(COMPOSE_NETWORK_LABEL).cloned(),
        }
    }
}

pub async fn list_all(docker: &Docker) -> Result<Vec<NetworkRow>> {
    let list = docker.list_networks(None::<ListNetworksOptions>).await?;
    Ok(list.into_iter().map(NetworkRow::from).collect())
}

/// Makes a network with the bridge driver. The engine chooses the subnet.
pub async fn create(docker: &Docker, name: &str) -> Result<()> {
    let request = NetworkCreateRequest {
        name: name.to_string(),
        driver: Some("bridge".to_string()),
        ..Default::default()
    };
    docker.create_network(request).await?;
    Ok(())
}

/// A container on the network, from the inspect data. The engine lists
/// only the running containers.
pub struct Endpoint {
    pub container_id: String,
    /// The address with the prefix length, for example "172.18.0.2/16".
    pub ipv4: Option<String>,
}

/// The part of the inspect data that the list does not have.
pub struct NetworkInfo {
    /// The time in the format of the engine, for example
    /// "2026-09-30T17:28:05.123456789-02:30".
    pub created: Option<String>,
    pub endpoints: Vec<Endpoint>,
    pub labels: Vec<(String, String)>,
    pub options: Vec<(String, String)>,
    /// The full inspect data as pretty JSON.
    pub json: String,
}

pub async fn inspect(docker: &Docker, name: &str) -> Result<NetworkInfo> {
    let network = docker.inspect_network(name, None).await?;
    // Read the time from the JSON, because its Rust type changes with the
    // features of bollard.
    let value = serde_json::to_value(&network)?;
    let sorted = |map: Option<&HashMap<String, String>>| {
        let mut list: Vec<(String, String)> =
            map.into_iter().flatten().map(|(k, v)| (k.clone(), v.clone())).collect();
        list.sort();
        list
    };
    let mut endpoints: Vec<Endpoint> = network
        .containers
        .iter()
        .flatten()
        .map(|(id, e)| Endpoint {
            container_id: id.clone(),
            ipv4: e.ipv4_address.clone().filter(|ip| !ip.is_empty()),
        })
        .collect();
    endpoints.sort_by(|a, b| a.container_id.cmp(&b.container_id));
    Ok(NetworkInfo {
        created: value.get("Created").and_then(|v| v.as_str()).map(str::to_string),
        endpoints,
        labels: sorted(network.labels.as_ref()),
        options: sorted(network.options.as_ref()),
        json: serde_json::to_string_pretty(&value)?,
    })
}

pub async fn remove(docker: &Docker, name: &str) -> Result<()> {
    docker.remove_network(name).await?;
    Ok(())
}

/// Deletes all the networks that no running container uses. The engine
/// keeps the built-in networks. Returns the number of deleted networks.
pub async fn prune(docker: &Docker) -> Result<usize> {
    let result = docker.prune_networks(None::<PruneNetworksOptions>).await?;
    Ok(result.networks_deleted.map_or(0, |n| n.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::IpamConfig;

    #[test]
    fn rows_come_from_the_engine_data() {
        let network = Network {
            id: Some("4f1c0a7d9e2b8c3a".into()),
            name: Some("shop_default".into()),
            driver: Some("bridge".into()),
            enable_ipv6: Some(true),
            ipam: Some(Ipam {
                config: Some(vec![
                    IpamConfig {
                        subnet: Some("fd00:1::/64".into()),
                        gateway: Some("fd00:1::1".into()),
                        ..Default::default()
                    },
                    IpamConfig {
                        subnet: Some("172.18.0.0/16".into()),
                        gateway: Some("172.18.0.1".into()),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            }),
            labels: Some(HashMap::from([
                (COMPOSE_PROJECT_LABEL.to_string(), "shop".to_string()),
                (COMPOSE_NETWORK_LABEL.to_string(), "default".to_string()),
            ])),
            ..Default::default()
        };
        let row = NetworkRow::from(network);
        assert_eq!(row.subnets, ["172.18.0.0/16", "fd00:1::/64"]);
        assert_eq!(row.gateways, ["172.18.0.1", "fd00:1::1"]);
        assert!(row.ipv6);
        assert!(!row.internal);
        assert_eq!(row.short_id(), "4f1c0a7d9e2b");
        assert_eq!(row.display_name(true), "default");
        assert_eq!(row.display_name(false), "shop_default");
        assert!(!row.is_built_in());
        let host = NetworkRow::from(Network { name: Some("host".into()), ..Default::default() });
        assert!(host.is_built_in());
        assert!(host.subnets.is_empty());
    }
}
