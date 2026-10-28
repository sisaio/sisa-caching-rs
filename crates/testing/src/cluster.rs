//! Three primaries in cluster mode.

use std::net::IpAddr;

use redis::{
    Client, ProtocolVersion,
    cluster::{ClusterClient, ClusterClientBuilder},
    cluster_async::ClusterConnection,
};
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

use crate::server::{Server, image, query, start_server, wait_for};

/// A cluster of three primaries, without replicas, one container each. Tests reach the servers at
/// their containers' own addresses, which works where Docker runs on the same Linux host, as on CI.
///
/// Redis, Valkey and Garnet nodes join by gossip. Dragonfly's cluster mode has no gossip: each node
/// is told the whole layout with `DFLYCLUSTER CONFIG`, as Dragonfly's own orchestration does.
pub struct RedisCluster {
    _containers: Vec<ContainerAsync<GenericImage>>,
    ips: Vec<IpAddr>,
}

impl RedisCluster {
    pub async fn start() -> Self {
        let server = Server::current();
        let mut args = server.serve(6379);
        args.extend(
            match server {
                Server::Redis => ["--cluster-enabled", "yes"].as_slice(),
                Server::Dragonfly => &["--cluster_mode=yes", "--admin_port=6380"],
                Server::Garnet => &["--cluster", "--device-type", "RandomAccess"],
            }
            .iter()
            .map(|arg| arg.to_string()),
        );

        let mut containers = Vec::new();
        let mut ips = Vec::new();
        for _ in 0..3 {
            let (container, ip) = start_server(image().with_cmd(args.clone()), 6379).await;
            containers.push(container);
            ips.push(ip);
        }

        let cluster = Self {
            _containers: containers,
            ips,
        };
        match server {
            Server::Dragonfly => cluster.configure_dragonfly().await,
            _ => cluster.join().await,
        }
        cluster
    }

    /// The slots node `index` of three owns.
    fn slots(index: usize) -> (u16, u16) {
        [(0, 5460), (5461, 10922), (10923, 16383)][index]
    }

    async fn join(&self) {
        let nodes = self.nodes();
        for (index, node) in nodes.iter().enumerate() {
            let (first, last) = Self::slots(index);
            let _: () = query(
                node,
                redis::cmd("CLUSTER")
                    .arg("ADDSLOTSRANGE")
                    .arg(first)
                    .arg(last),
            )
            .await;
        }
        for ip in &self.ips[1..] {
            let _: () = query(
                &nodes[0],
                redis::cmd("CLUSTER")
                    .arg("MEET")
                    .arg(ip.to_string())
                    .arg(6379),
            )
            .await;
        }
        for node in &nodes {
            wait_for("every node sees all slots served", || async {
                let info: String = query(node, redis::cmd("CLUSTER").arg("INFO")).await;
                let slots: Vec<redis::Value> =
                    query(node, redis::cmd("CLUSTER").arg("SLOTS")).await;
                info.contains("cluster_state:ok") && slots.len() == 3
            })
            .await;
        }
    }

    async fn configure_dragonfly(&self) {
        let ranges = [0, 1, 2].map(|index| vec![Self::slots(index)]);

        self.configure_dragonfly_slots(&ranges).await;
    }

    /// Hands each node `ranges[index]`, through Dragonfly's admin port.
    async fn configure_dragonfly_slots(&self, ranges: &[Vec<(u16, u16)>; 3]) {
        let mut shards = Vec::new();
        for (index, node) in self.nodes().iter().enumerate() {
            let id: String = query(node, redis::cmd("CLUSTER").arg("MYID")).await;
            let slots: Vec<String> = ranges[index]
                .iter()
                .map(|(first, last)| format!(r#"{{"start":{first},"end":{last}}}"#))
                .collect();
            shards.push(format!(
                r#"{{"slot_ranges":[{}],"master":{{"id":"{id}","ip":"{}","port":6379}},"replicas":[]}}"#,
                slots.join(","),
                self.ips[index]
            ));
        }
        let config = format!("[{}]", shards.join(","));

        for ip in &self.ips {
            let admin = Client::open(format!("redis://{ip}:6380")).unwrap();
            let _: () = query(&admin, redis::cmd("DFLYCLUSTER").arg("CONFIG").arg(&config)).await;
        }
    }

    /// Moves the slot of `key`, which must hold no keys, to the next node, and waits until every
    /// node agrees. Returns the client of the node that owns it now.
    pub async fn move_slot(&self, key: &str) -> Client {
        let nodes = self.nodes();
        let slot: u16 = query(&nodes[0], redis::cmd("CLUSTER").arg("KEYSLOT").arg(key)).await;
        let owner = (0..3)
            .find(|&index| {
                let (first, last) = Self::slots(index);
                (first..=last).contains(&slot)
            })
            .unwrap();
        let target = (owner + 1) % 3;

        if Server::current() == Server::Dragonfly {
            let mut ranges = [0, 1, 2].map(|index| vec![Self::slots(index)]);
            let (first, last) = Self::slots(owner);
            ranges[owner] = [(first, slot.wrapping_sub(1)), (slot + 1, last)]
                .into_iter()
                .filter(|(first, last)| first <= last && *last != u16::MAX)
                .collect();
            ranges[target].push((slot, slot));
            self.configure_dragonfly_slots(&ranges).await;
        } else {
            let id: String = query(&nodes[target], redis::cmd("CLUSTER").arg("MYID")).await;
            for index in [target, owner, 3 - target - owner] {
                let _: () = query(
                    &nodes[index],
                    redis::cmd("CLUSTER")
                        .arg("SETSLOT")
                        .arg(slot)
                        .arg("NODE")
                        .arg(&id),
                )
                .await;
            }
        }

        let target_ip = self.ips[target].to_string();
        for node in &nodes {
            wait_for("every node sees the slot moved", || async {
                let ranges: Vec<(u16, u16, (String, u16))> =
                    query::<Vec<Vec<redis::Value>>>(node, redis::cmd("CLUSTER").arg("SLOTS"))
                        .await
                        .into_iter()
                        .map(|range: Vec<redis::Value>| {
                            let mut range = range.into_iter();
                            let first = redis::from_redis_value(range.next().unwrap()).unwrap();
                            let last = redis::from_redis_value(range.next().unwrap()).unwrap();
                            let primary: Vec<redis::Value> =
                                redis::from_redis_value(range.next().unwrap()).unwrap();
                            let mut primary = primary.into_iter();
                            let host = redis::from_redis_value(primary.next().unwrap()).unwrap();
                            let port = redis::from_redis_value(primary.next().unwrap()).unwrap();
                            (first, last, (host, port))
                        })
                        .collect();
                ranges.iter().any(|(first, last, (host, _))| {
                    (*first..=*last).contains(&slot) && *host == target_ip
                })
            })
            .await;
        }

        nodes[target].clone()
    }

    /// One plain client per primary, to look at what each node holds.
    pub fn nodes(&self) -> Vec<Client> {
        self.ips
            .iter()
            .map(|ip| Client::open(format!("redis://{ip}:6379")).unwrap())
            .collect()
    }

    /// A cluster client on RESP3, which sharded pub/sub needs.
    pub fn client(&self) -> ClusterClient {
        ClusterClientBuilder::new(vec![format!("redis://{}:6379", self.ips[0])])
            .use_protocol(ProtocolVersion::RESP3)
            .build()
            .unwrap()
    }

    pub async fn connection(&self) -> ClusterConnection {
        self.client().get_async_connection().await.unwrap()
    }
}
