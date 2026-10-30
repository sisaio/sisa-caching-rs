//! A TCP proxy in front of one server, which a test can cut off from it.

use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinHandle,
};

/// Forwards every connection to a server until [`cut`](Self::cut), as a network split would:
/// nothing passes and nothing is refused, so a client finds out only by timing out. Its
/// connections then close at [`restore`](Self::restore), with whatever was sent meanwhile, as
/// connections that outlived a long split would; new ones connect again. Stops when dropped.
pub struct Proxy {
    url: String,
    cut: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Proxy {
    /// A proxy on a free local port in front of the server at `url`, such as
    /// `redis://127.0.0.1:6379`.
    pub async fn start(url: &str) -> Self {
        let target = url
            .strip_prefix("redis://")
            .expect("a redis:// URL")
            .to_owned();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (cut, watching) = watch::channel(false);
        let task = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                tokio::spawn(forward(client, target.clone(), watching.clone()));
            }
        });

        Self {
            url: format!("redis://127.0.0.1:{port}"),
            cut,
            task,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn cut(&self) {
        self.cut.send_replace(true);
    }

    pub fn restore(&self) {
        self.cut.send_replace(false);
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Forwards one connection until the proxy is cut, then holds it until the cut ends and closes it.
async fn forward(mut client: TcpStream, target: String, mut cut: watch::Receiver<bool>) {
    if !*cut.borrow_and_update()
        && let Ok(mut server) = TcpStream::connect(&target).await
    {
        tokio::select! {
            _ = copy_bidirectional(&mut client, &mut server) => return,
            _ = cut.wait_for(|cut| *cut) => {}
        }
    }

    let _ = cut.wait_for(|cut| !*cut).await;
}
