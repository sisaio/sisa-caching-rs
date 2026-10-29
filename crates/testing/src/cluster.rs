//! Three primaries in cluster mode.

use std::net::IpAddr;

use redis::{
    Client, ProtocolVersion,
    cluster::{ClusterClient, ClusterClientBuilder},
    cluster_async::ClusterConnection,
};
use testcontainers::{ContainerAsync, GenericImage, ImageExt, runners::AsyncRunner};

use crate::server::{Server, beside, image, query, start_server, wait_for, wait_for_ping};

/// A cluster of three primaries, without replicas, one container each. Tests reach the servers at
/// their containers' own addresses, which works where Docker runs on the same Linux host, as on CI.
///
/// Redis, Valkey and Garnet nodes join by gossip. Dragonfly's cluster mode has no gossip: each node
/// is told the whole layout with `DFLYCLUSTER CONFIG`, as Dragonfly's own orchestration does.
pub struct RedisCluster {
    _containers: Vec<ContainerAsync<GenericImage>>,
    /// Each node's address and port.
    addresses: Vec<(IpAddr, u16)>,
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
        let mut addresses = Vec::new();
        for _ in 0..3 {
            let (container, ip) = start_server(image().with_cmd(args.clone()), 6379).await;
            containers.push(container);
            addresses.push((ip, 6379));
        }

        let cluster = Self {
            _containers: containers,
            addresses,
        };
        match server {
            Server::Dragonfly => cluster.configure_dragonfly().await,
            _ => cluster.join().await,
        }
        cluster
    }

    /// Like [`start`](Self::start), but the three nodes share one address, on ports 6379 to 6381,
    /// and report no endpoint of their own (`cluster-preferred-endpoint-type unknown-endpoint`),
    /// as nodes behind one load balancer do: a client reaches each at the address it asked, on the
    /// port the node reports. `None` on Dragonfly and Garnet, which have no such setting.
    pub async fn start_without_endpoints() -> Option<Self> {
        if Server::current() != Server::Redis {
            eprintln!("skipped: only Redis and Valkey report nodes without an endpoint");
            return None;
        }
        let args = |port| {
            let mut args = Server::Redis.serve(port);
            args.extend(
                [
                    "--cluster-enabled",
                    "yes",
                    "--cluster-preferred-endpoint-type",
                    "unknown-endpoint",
                ]
                .map(str::to_owned),
            );
            image().with_cmd(args)
        };

        let (first, ip) = start_server(args(6379), 6379).await;
        let mut containers = vec![first];
        for port in [6380, 6381] {
            let node = beside(&containers[0], args(port)).start().await.unwrap();
            containers.push(node);
            wait_for_ping(&Client::open(format!("redis://{ip}:{port}")).unwrap()).await;
        }

        let cluster = Self {
            _containers: containers,
            addresses: [6379, 6380, 6381].map(|port| (ip, port)).into(),
        };
        cluster.join().await;
        Some(cluster)
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
        for (ip, port) in &self.addresses[1..] {
            let _: () = query(
                &nodes[0],
                redis::cmd("CLUSTER")
                    .arg("MEET")
                    .arg(ip.to_string())
                    .arg(port),
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
                self.addresses[index].0
            ));
        }
        let config = format!("[{}]", shards.join(","));

        for (ip, _) in &self.addresses {
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

        let target_ip = self.addresses[target].0.to_string();
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
        self.addresses
            .iter()
            .map(|(ip, port)| Client::open(format!("redis://{ip}:{port}")).unwrap())
            .collect()
    }

    /// A cluster client on RESP3, which sharded pub/sub needs.
    pub fn client(&self) -> ClusterClient {
        let (ip, port) = self.addresses[0];

        ClusterClientBuilder::new(vec![format!("redis://{ip}:{port}")])
            .use_protocol(ProtocolVersion::RESP3)
            .build()
            .unwrap()
    }

    pub async fn connection(&self) -> ClusterConnection {
        self.client().get_async_connection().await.unwrap()
    }
}
