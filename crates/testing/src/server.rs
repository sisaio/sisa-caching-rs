//! What every layout shares: the server family `REDIS_IMAGE` names, its image, and waiting for
//! a server to answer.

use std::{
    net::IpAddr,
    time::{Duration, Instant},
};

use redis::Client;
use testcontainers::{
    ContainerAsync, ContainerRequest, GenericImage, ImageExt, runners::AsyncRunner,
};

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
    pub(crate) fn serve(self, port: u16) -> Vec<String> {
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

pub(crate) fn image() -> GenericImage {
    let image = image_name();
    let (name, tag) = image.rsplit_once(':').unwrap_or((&image, "latest"));

    GenericImage::new(name, tag)
}

/// Starts one server and waits until it answers on `port`.
pub(crate) async fn start_server(
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

/// `request` to run in `container`'s network namespace, so its server shares that address.
pub(crate) fn beside(
    container: &ContainerAsync<GenericImage>,
    request: ContainerRequest<GenericImage>,
) -> ContainerRequest<GenericImage> {
    let id = container.id().to_owned();

    request.with_host_config_modifier(move |config| {
        config.network_mode = Some(format!("container:{id}"));
        // Docker refuses published ports in another container's network.
        config.publish_all_ports = Some(false);
        config.port_bindings = None;
    })
}

// Servers log readiness differently, so wait until one answers PING.
pub(crate) async fn wait_for_ping(client: &Client) {
    wait_for("the server answers PING", || async {
        ping(client).await.is_ok()
    })
    .await;
}

async fn ping(client: &Client) -> redis::RedisResult<()> {
    let mut connection = client.get_multiplexed_async_connection().await?;
    redis::cmd("PING").query_async(&mut connection).await
}

pub(crate) async fn query<T: redis::FromRedisValue>(client: &Client, cmd: &mut redis::Cmd) -> T {
    let mut connection = client.get_multiplexed_async_connection().await.unwrap();
    cmd.query_async(&mut connection).await.unwrap()
}

/// Waits up to 60 s for `check`, which topology changes such as a cluster agreeing on its slots
/// can need.
pub(crate) async fn wait_for<F, Fut>(what: &str, mut check: F)
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
