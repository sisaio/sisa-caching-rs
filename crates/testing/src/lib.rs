//! Test helpers for the Sisa Caching crates. Not published.
//!
//! - [`ManualClock`]: a clock that moves only when the test moves it.
//! - [`Redis`]: a fresh Redis server in Docker for one test, removed when the test ends.
//! - [`RedisCluster`] and [`RedisSentinel`]: the same for a three-primary cluster and for a
//!   primary and replica behind Sentinel.
//!
//! `REDIS_IMAGE` picks the server for all of them: Redis, Valkey, Dragonfly or Garnet.
//! - [`RedisTls`]: a server that only speaks TLS, with or without asking for a client certificate.
//! - [`eventually`]: waits for something another task does.

use std::{
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
use redis::{
    Client, ProtocolVersion,
    aio::ConnectionManager,
    cluster::{ClusterClient, ClusterClientBuilder},
    cluster_async::ClusterConnection,
    sentinel::{SentinelClient, SentinelServerType},
};
use sisa_caching_core::clock::Clock;
use testcontainers::{
    ContainerAsync, ContainerRequest, GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};

/// A clock that only moves on [`advance`](Self::advance). Clones share the time, so a test keeps
/// one and gives another to a store.
#[derive(Debug, Clone)]
pub struct ManualClock {
    start: Instant,
    elapsed_ms: Arc<AtomicU64>,
    wall_jump_ms: Arc<AtomicU64>,
}

impl ManualClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            elapsed_ms: Arc::default(),
            wall_jump_ms: Arc::default(),
        }
    }

    /// Moves both clocks forward, as time passing does.
    pub fn advance(&self, by: Duration) {
        self.elapsed_ms.fetch_add(millis(by), Ordering::SeqCst);
    }

    /// Moves only the wall clock forward, as someone changing the machine's time does.
    pub fn jump_wall_clock(&self, by: Duration) {
        self.wall_jump_ms.fetch_add(millis(by), Ordering::SeqCst);
    }

    fn elapsed(&self) -> Duration {
        Duration::from_millis(self.elapsed_ms.load(Ordering::SeqCst))
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for ManualClock {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
            + Duration::from_secs(1_000_000)
            + self.elapsed()
            + Duration::from_millis(self.wall_jump_ms.load(Ordering::SeqCst))
    }

    fn instant(&self) -> Instant {
        self.start + self.elapsed()
    }
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis() as u64
}

/// A Redis server of its own for one test, in Docker. It is removed when this value drops.
///
/// `REDIS_IMAGE` picks the image, such as `valkey/valkey:8-alpine`; the default is
/// `redis:8-alpine`.
pub struct Redis {
    container: ContainerAsync<GenericImage>,
    url: String,
}

impl Redis {
    pub async fn start() -> Self {
        // A host port of its own, so the server keeps its address after `restart`.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let container = image()
            .with_mapped_port(port, 6379.tcp())
            .with_cmd(Server::current().serve(6379))
            .start()
            .await
            .expect("Docker must be running to start Redis for the tests");
        let redis = Self {
            container,
            url: format!("redis://127.0.0.1:{port}"),
        };

        redis.wait_until_ready().await;
        redis
    }

    async fn wait_until_ready(&self) {
        wait_for_ping(&self.client()).await;
    }

    /// Kills the server, as a crash would: connections close and new ones are refused.
    pub async fn stop(&self) {
        self.container.stop_with_timeout(Some(0)).await.unwrap();
    }

    /// Starts a stopped server again at the same address, and waits until it answers.
    pub async fn restart(&self) {
        self.container.start().await.unwrap();
        self.wait_until_ready().await;
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn client(&self) -> Client {
        Client::open(self.url.as_str()).unwrap()
    }

    pub async fn connection(&self) -> ConnectionManager {
        ConnectionManager::new(self.client()).await.unwrap()
    }

    /// Freezes the server: connections stay open but nothing answers, as on a network that drops
    /// traffic silently.
    pub async fn pause(&self) {
        self.container.pause().await.unwrap();
    }

    pub async fn unpause(&self) {
        self.container.unpause().await.unwrap();
    }

    /// Closes every Pub/Sub connection, as a server restart or a proxy timeout would, and returns
    /// how many it closed. Lists the clients and kills each by ID, which Redis, Valkey and
    /// Dragonfly all accept, unlike `CLIENT KILL TYPE pubsub`.
    pub async fn drop_subscriptions(&self) -> usize {
        let mut connection = self.connection().await;
        let clients: String = redis::cmd("CLIENT")
            .arg("LIST")
            .query_async(&mut connection)
            .await
            .unwrap();
        let mut closed = 0;
        for client in clients.lines().filter(|client| is_subscriber(client)) {
            let id = client
                .split(' ')
                .find_map(|field| field.strip_prefix("id="));
            let killed: usize = redis::cmd("CLIENT")
                .arg("KILL")
                .arg("ID")
                .arg(id.unwrap())
                .query_async(&mut connection)
                .await
                .unwrap();
            closed += killed;
        }
        closed
    }
}

/// Whether a `CLIENT LIST` line is a Pub/Sub connection: its flags include `P`.
fn is_subscriber(client: &str) -> bool {
    client.split(' ').any(|field| {
        field
            .strip_prefix("flags=")
            .is_some_and(|flags| flags.contains('P'))
    })
}

/// The server family `REDIS_IMAGE` runs. Each takes its own flags, and Cluster, Sentinel and TLS
/// are set up differently on each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Server {
    /// Redis or Valkey.
    Redis,
    Dragonfly,
    Garnet,
}

impl Server {
    /// The server `REDIS_IMAGE` names.
    pub fn current() -> Self {
        let image = image_name();

        if image.contains("dragonfly") {
            Self::Dragonfly
        } else if image.contains("garnet") {
            Self::Garnet
        } else {
            Self::Redis
        }
    }

    /// Flags that serve plain clients on `port`, reachable from other containers.
    fn serve(self, port: u16) -> Vec<String> {
        let port = port.to_string();
        let args: &[&str] = match self {
            Self::Redis => &["--port", &port, "--save", "", "--protected-mode", "no"],
            Self::Dragonfly => &["--port", &port],
            // Garnet listens on 127.0.0.1 unless told otherwise.
            Self::Garnet => &["--bind", "0.0.0.0", "--port", &port],
        };

        args.iter().map(|arg| arg.to_string()).collect()
    }
}

fn image_name() -> String {
    std::env::var("REDIS_IMAGE").unwrap_or_else(|_| "redis:8-alpine".into())
}

fn image() -> GenericImage {
    let image = image_name();
    let (name, tag) = image.rsplit_once(':').unwrap_or((&image, "latest"));

    GenericImage::new(name, tag)
}

/// Starts one server and waits until it answers on `port`.
async fn start_server(
    request: ContainerRequest<GenericImage>,
    port: u16,
) -> (ContainerAsync<GenericImage>, IpAddr) {
    let container = request
        .start()
        .await
        .expect("Docker must be running to start Redis for the tests");
    let ip = container.get_bridge_ip_address().await.unwrap();

    wait_for_ping(&Client::open(format!("redis://{ip}:{port}")).unwrap()).await;
    (container, ip)
}

// Servers log readiness differently, so wait until one answers PING.
async fn wait_for_ping(client: &Client) {
    wait_for("the server answers PING", || async {
        ping(client).await.is_ok()
    })
    .await;
}

async fn ping(client: &Client) -> redis::RedisResult<()> {
    let mut connection = client.get_multiplexed_async_connection().await?;
    redis::cmd("PING").query_async(&mut connection).await
}

async fn query<T: redis::FromRedisValue>(client: &Client, cmd: &mut redis::Cmd) -> T {
    let mut connection = client.get_multiplexed_async_connection().await.unwrap();
    cmd.query_async(&mut connection).await.unwrap()
}

/// Waits up to 60 s for `check`, which topology changes such as a cluster agreeing on its slots
/// can need.
async fn wait_for<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(60);
    while !check().await {
        assert!(Instant::now() < deadline, "timed out after 60 s: {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

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

/// A primary on port 6379 and its replica on 6380, watched by one Sentinel as `primary`. The three
/// share one network namespace, so they announce one address. Redis Sentinel watches Dragonfly
/// too; Garnet replicates only in cluster mode, so it has no Sentinel setup.
pub struct RedisSentinel {
    _containers: Vec<ContainerAsync<GenericImage>>,
    ip: IpAddr,
}

impl RedisSentinel {
    /// `None` on Garnet, which Sentinel can't watch.
    pub async fn start() -> Option<Self> {
        let server = Server::current();
        let sentinel_image = match server {
            Server::Redis => image(),
            Server::Dragonfly => GenericImage::new("redis", "8-alpine"),
            Server::Garnet => {
                eprintln!("skipped: Garnet has no Sentinel setup");
                return None;
            }
        };

        let (primary, ip) = start_server(image().with_cmd(server.serve(6379)), 6379).await;
        let beside = |request: ContainerRequest<GenericImage>| {
            let id = primary.id().to_owned();
            request.with_host_config_modifier(move |config| {
                config.network_mode = Some(format!("container:{id}"));
                // Docker refuses published ports in another container's network.
                config.publish_all_ports = Some(false);
                config.port_bindings = None;
            })
        };
        let replica = beside(image().with_cmd(server.serve(6380)))
            .start()
            .await
            .unwrap();
        let replica_client = Client::open(format!("redis://{ip}:6380")).unwrap();
        wait_for_ping(&replica_client).await;
        let _: () = query(
            &replica_client,
            redis::cmd("REPLICAOF").arg(ip.to_string()).arg(6379),
        )
        .await;
        wait_for("the replica syncs", || async {
            let info: String = query(&replica_client, redis::cmd("INFO").arg("replication")).await;
            info.contains("master_link_status:up")
        })
        .await;

        let config = format!(
            "port 26379\nprotected-mode no\nsentinel monitor primary {ip} 6379 1\n\
             sentinel down-after-milliseconds primary 1000\nsentinel failover-timeout primary 5000\n"
        );
        let sentinel = beside(sentinel_image.with_cmd([
            "sh".to_owned(),
            "-c".to_owned(),
            format!(
                "printf '{config}' > /tmp/sentinel.conf && exec redis-sentinel /tmp/sentinel.conf"
            ),
        ]))
        .start()
        .await
        .unwrap();

        let redis = Self {
            _containers: vec![primary, replica, sentinel],
            ip,
        };
        let sentinel_client = redis.server(26379);
        wait_for("Sentinel finds the replica", || async {
            let replicas: redis::RedisResult<Vec<redis::Value>> = async {
                let mut connection = sentinel_client.get_multiplexed_async_connection().await?;
                redis::cmd("SENTINEL")
                    .arg("REPLICAS")
                    .arg("primary")
                    .query_async(&mut connection)
                    .await
            }
            .await;
            replicas.is_ok_and(|replicas| !replicas.is_empty())
        })
        .await;
        Some(redis)
    }

    pub fn sentinel_client(&self) -> SentinelClient {
        SentinelClient::build(
            vec![format!("redis://{}:26379", self.ip)],
            String::from("primary"),
            None,
            SentinelServerType::Master,
        )
        .unwrap()
    }

    /// A plain client for the server on `port`, 6379 or 6380.
    pub fn server(&self, port: u16) -> Client {
        Client::open(format!("redis://{}:{port}", self.ip)).unwrap()
    }

    /// Closes every client connection to the server on `port` but this call's own, as a server
    /// dropping its clients would, and returns how many it closed. Kills each by ID, as
    /// [`Redis::drop_subscriptions`] does, since Dragonfly refuses `CLIENT KILL TYPE`. Dragonfly
    /// lists a replica's link to its primary too and refuses to kill it, so that one stays.
    pub async fn drop_clients(&self, port: u16) -> usize {
        let mut connection = self
            .server(port)
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        let own: u64 = redis::cmd("CLIENT")
            .arg("ID")
            .query_async(&mut connection)
            .await
            .unwrap();
        let clients: String = redis::cmd("CLIENT")
            .arg("LIST")
            .query_async(&mut connection)
            .await
            .unwrap();
        let mut closed = 0;
        for id in clients
            .lines()
            .filter_map(|client| {
                client
                    .split(' ')
                    .find_map(|field| field.strip_prefix("id="))
            })
            .filter(|id| *id != own.to_string())
        {
            let killed: redis::RedisResult<usize> = redis::cmd("CLIENT")
                .arg("KILL")
                .arg("ID")
                .arg(id)
                .query_async(&mut connection)
                .await;
            closed += killed.unwrap_or(0);
        }
        closed
    }

    /// Promotes the server on 6380 and waits until Sentinel names it. The old primary keeps
    /// accepting writes until Sentinel demotes it, about ten seconds later.
    pub async fn failover(&self) {
        let sentinel = self
            .server(26379)
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        // Sentinel refuses with NOGOODSLAVE until it has fresh news of the replica.
        wait_for("Sentinel starts the failover", || async {
            redis::cmd("SENTINEL")
                .arg("FAILOVER")
                .arg("primary")
                .query_async::<()>(&mut sentinel.clone())
                .await
                .is_ok()
        })
        .await;

        eventually("Sentinel names 6380 as the primary", || async {
            let primary = self.sentinel_client().async_get_client().await;

            primary.is_ok_and(|client| {
                client
                    .get_connection_info()
                    .addr()
                    .to_string()
                    .ends_with(":6380")
            })
        })
        .await;
    }

    /// Makes 6379 a replica of 6380 now, as Sentinel would after a failover.
    pub async fn demote_old_primary(&self) {
        let mut old = self
            .server(6379)
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        let _: () = redis::cmd("REPLICAOF")
            .arg(self.ip.to_string())
            .arg(6380)
            .query_async(&mut old)
            .await
            .unwrap();

        // The first sync takes several seconds, longer than `eventually` waits.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let info: String = redis::cmd("INFO")
                .arg("replication")
                .query_async(&mut old)
                .await
                .unwrap();
            // Dragonfly answers LOADING for a moment after the link comes up.
            let loading = redis::cmd("EXISTS")
                .arg("loading-check")
                .query_async::<bool>(&mut old)
                .await
                .is_err_and(|error| {
                    error.kind() == redis::ErrorKind::Server(redis::ServerErrorKind::BusyLoading)
                });
            if info.contains("master_port:6380")
                && info.contains("master_link_status:up")
                && !loading
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "6379 did not replicate from 6380 within 30 s"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// The password a Dragonfly TLS server without client certificates requires.
const PASSWORD: &str = "secret";

/// A server of the `REDIS_IMAGE` that accepts only TLS, on certificates made for this test: its
/// own CA, a server certificate for `localhost`, and a client certificate the server requires.
pub struct RedisTls {
    _container: ContainerAsync<GenericImage>,
    url: String,
    // Read only by `client`, which needs rustls.
    #[cfg_attr(not(feature = "rustls"), allow(dead_code))]
    ca: String,
    #[cfg_attr(not(feature = "rustls"), allow(dead_code))]
    client_cert: String,
    #[cfg_attr(not(feature = "rustls"), allow(dead_code))]
    client_key: String,
}

impl RedisTls {
    pub async fn start() -> Self {
        Self::start_with("yes").await
    }

    /// A server that doesn't ask for a client certificate, for clients that can't show one, as
    /// redis-rs's native-tls can't.
    pub async fn start_without_client_certificates() -> Self {
        Self::start_with("no").await
    }

    async fn start_with(auth_clients: &str) -> Self {
        let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        // Each certificate needs its own subject, or a leaf looks self-signed.
        ca.distinguished_name.push(DnType::CommonName, "test CA");
        let ca_key = KeyPair::generate().unwrap();
        let ca_cert = ca.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca, ca_key);
        let leaf = |name: &str| {
            let key = KeyPair::generate().unwrap();
            let mut params = CertificateParams::new(vec![name.to_owned()]).unwrap();
            params.distinguished_name.push(DnType::CommonName, name);
            let cert = params.signed_by(&key, &issuer).unwrap();

            (cert.pem(), key.serialize_pem())
        };
        let (server_cert, server_key) = leaf("localhost");
        let (client_cert, client_key) = leaf("client");

        let auth = auth_clients == "yes";
        let (args, ready): (Vec<&str>, _) = match Server::current() {
            Server::Redis => (
                vec![
                    "--port",
                    "0",
                    "--tls-port",
                    "6379",
                    "--tls-cert-file",
                    "/tls/server.crt",
                    "--tls-key-file",
                    "/tls/server.key",
                    "--tls-ca-cert-file",
                    "/tls/ca.crt",
                    "--tls-auth-clients",
                    auth_clients,
                ],
                WaitFor::message_on_stdout("Ready to accept connections"),
            ),
            // Dragonfly won't serve TLS without a way to authenticate clients: a client
            // certificate, or else a password.
            Server::Dragonfly => (
                [
                    "--port",
                    "6379",
                    "--tls",
                    "--tls_cert_file",
                    "/tls/server.crt",
                    "--tls_key_file",
                    "/tls/server.key",
                ]
                .into_iter()
                .chain(if auth {
                    ["--tls_ca_cert_file", "/tls/ca.crt"]
                } else {
                    ["--requirepass", PASSWORD]
                })
                .collect(),
                WaitFor::message_on_stderr("listening on 0.0.0.0:6379"),
            ),
            Server::Garnet => (
                vec![
                    "--bind",
                    "0.0.0.0",
                    "--tls",
                    "--cert-file-name",
                    "/tls/server.crt",
                    "--cert-password",
                    "/tls/server.key",
                    "--issuer-certificate-path",
                    "/tls/ca.crt",
                    "--client-certificate-required",
                    if auth { "true" } else { "false" },
                ],
                WaitFor::message_on_stdout("Ready to accept connections"),
            ),
        };

        let container = image()
            .with_exposed_port(6379.tcp())
            .with_wait_for(ready)
            .with_copy_to("/tls/ca.crt", ca_cert.pem().into_bytes())
            .with_copy_to("/tls/server.crt", server_cert.into_bytes())
            .with_copy_to("/tls/server.key", server_key.into_bytes())
            .with_cmd(args)
            .with_startup_timeout(Duration::from_secs(60))
            .start()
            .await
            .expect("Docker must be running to start Redis for the tests");
        let port = container.get_host_port_ipv4(6379).await.unwrap();

        Self {
            _container: container,
            url: match Server::current() {
                Server::Dragonfly if !auth => format!("rediss://:{PASSWORD}@localhost:{port}"),
                _ => format!("rediss://localhost:{port}"),
            },
            ca: ca_cert.pem(),
            client_cert,
            client_key,
        }
    }

    /// A `rediss://` URL, which the system's root certificates don't trust.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// A client that trusts this server's CA and shows the client certificate.
    #[cfg(feature = "rustls")]
    pub fn client(&self) -> Client {
        Client::build_with_tls(
            self.url.as_str(),
            redis::TlsCertificates {
                client_tls: Some(redis::ClientTlsConfig {
                    client_cert: self.client_cert.clone().into_bytes(),
                    client_key: self.client_key.clone().into_bytes(),
                }),
                root_cert: Some(self.ca.clone().into_bytes()),
            },
        )
        .unwrap()
    }
}

/// Polls `check` until it holds, and fails the test with `what` after five seconds.
pub async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

    while !check().await {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");

        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
