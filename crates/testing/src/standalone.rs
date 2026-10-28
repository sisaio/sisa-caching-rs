//! One server, which a test can stop, restart, freeze and cut off.

use redis::{Client, aio::ConnectionManager};
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt, core::IntoContainerPort, runners::AsyncRunner,
};

use crate::server::{Server, image, wait_for_ping};

/// A Redis server of its own for one test, in Docker. It is removed when this value drops.
///
/// `REDIS_IMAGE` picks the image, such as `valkey/valkey:8-alpine`; the default is
/// `redis:8-alpine`.
pub struct Redis {
    container: ContainerAsync<GenericImage>,
    url: String,
    restartable: bool,
}

impl Redis {
    /// A server on a host port Docker picks.
    pub async fn start() -> Self {
        let container = image()
            .with_exposed_port(6379.tcp())
            .with_cmd(Server::current().serve(6379))
            .start()
            .await
            .expect("Docker must be running to start Redis for the tests");
        let port = container.get_host_port_ipv4(6379).await.unwrap();

        Self::ready(container, port, false).await
    }

    /// A server that keeps its address across [`stop`](Self::stop) and [`restart`](Self::restart).
    /// Docker gives a restarted container a new host port unless it was told one, so this picks a
    /// free port first. Another process can take that port before Docker binds it, so only tests
    /// that restart the server use this.
    pub async fn start_restartable() -> Self {
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

        Self::ready(container, port, true).await
    }

    async fn ready(container: ContainerAsync<GenericImage>, port: u16, restartable: bool) -> Self {
        let redis = Self {
            container,
            url: format!("redis://127.0.0.1:{port}"),
            restartable,
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
        assert!(
            self.restartable,
            "only a server from `Redis::start_restartable` keeps its address"
        );
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
