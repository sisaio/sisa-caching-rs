//! A primary and a replica behind Sentinel.

use std::{
    net::IpAddr,
    time::{Duration, Instant},
};

use redis::{
    Client,
    sentinel::{SentinelClient, SentinelServerType},
};
use testcontainers::{ContainerAsync, GenericImage, ImageExt, runners::AsyncRunner};

use crate::{
    eventually,
    server::{Server, beside, image, query, start_server, wait_for, wait_for_ping},
};

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
        let replica = beside(&primary, image().with_cmd(server.serve(6380)))
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
        let command = format!(
            "printf '{config}' > /tmp/sentinel.conf && exec redis-sentinel /tmp/sentinel.conf"
        );
        let sentinel = beside(
            &primary,
            sentinel_image.with_cmd(["sh".to_owned(), "-c".to_owned(), command]),
        )
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
    /// [`Redis::drop_subscriptions`](crate::Redis::drop_subscriptions) does, since Dragonfly
    /// refuses `CLIENT KILL TYPE`. Dragonfly lists a replica's link to its primary too and refuses
    /// to kill it, so that one stays.
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
            // Dragonfly answers writes with LOADING for a moment after the link comes up, so wait
            // until it refuses them as a replica does.
            let read_only = redis::cmd("SET")
                .arg("read-only-check")
                .arg(1)
                .query_async::<()>(&mut old)
                .await
                .is_err_and(|error| {
                    error.kind() == redis::ErrorKind::Server(redis::ServerErrorKind::ReadOnly)
                });
            if info.contains("master_port:6380")
                && info.contains("master_link_status:up")
                && read_only
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
