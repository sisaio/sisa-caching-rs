//! A server that accepts only TLS.

use std::time::Duration;

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};

use crate::server::{Server, image};

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
    pub fn client(&self) -> redis::Client {
        redis::Client::build_with_tls(
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
