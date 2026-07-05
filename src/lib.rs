//! ce-test — the CE testing harness (`PLAN/ce-testing-framework.md`).
//!
//! Spin ephemeral, **chain-free** (`--no-economy`) nodes, run modules on them, drive **direct
//! module↔module** communication over the mesh, assert, and tear down automatically. This is the
//! foundation of `ce test`. Two topologies today: co-located modules over one node's Bus
//! (`h.node()`, the self-request path — deterministic, no peering), and **cross-node over real
//! libp2p** (`h.peer_of(seed)` dials the seed directly; no relay, no mDNS). Next: `ce app install`
//! in the harness and `h.arduino(alias)` (a real board, with an emulated-board fallback for CI).
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! use ce_test::Harness;
//! let mut h = Harness::new();
//! let node = h.node().await?;                 // an isolated ephemeral node
//! let _echo = node.responder("test/echo", |p| p);   // module B: echoes on a topic
//! let reply = node.request(&node.node_id, "test/echo", b"hi", 5_000).await?;  // module A drives it
//! assert_eq!(reply, b"hi");
//! # Ok(()) }
//! ```

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use ce_rs::serve::{serve, Handler, Request};
use ce_rs::CeClient;
use tokio::sync::oneshot;

/// A test topology: spawns nodes and owns their lifetimes (killed + wiped on drop).
pub struct Harness {
    guards: Vec<NodeGuard>,
    ce_bin: String,
}

impl Default for Harness {
    fn default() -> Self {
        Harness::new()
    }
}

impl Harness {
    /// A fresh harness. The `ce` binary is `$CE_BIN` (default `ce` from `PATH`).
    pub fn new() -> Harness {
        Harness { guards: Vec::new(), ce_bin: std::env::var("CE_BIN").unwrap_or_else(|_| "ce".into()) }
    }

    /// Spin an **isolated, chain-free** ephemeral node (`ce start --no-economy` with
    /// `CE_NO_AUTOBOOTSTRAP=1`, its own temp data-dir + free ports) and wait until its API is live.
    /// Killed + wiped when the harness drops.
    pub async fn node(&mut self) -> Result<TestNode> {
        self.spawn(None).await
    }

    /// Spin a second node that dials `seed` **directly** and joins its isolated mesh — no relay, no
    /// mDNS, no bootstrap of the real network. Use this for T3 comms tests where module A on one node
    /// drives module B on another over real libp2p (the transparency invariant, cross-node).
    pub async fn peer_of(&mut self, seed: &TestNode) -> Result<TestNode> {
        let addr = seed.dial_addr();
        self.spawn(Some(&addr)).await
    }

    /// Spawn a `--no-economy` node, optionally `--bootstrap`ped at `dial` (a `/ip4/…/p2p/…` addr).
    async fn spawn(&mut self, dial: Option<&str>) -> Result<TestNode> {
        let idx = self.guards.len();
        let data_dir = std::env::temp_dir().join(format!("ce-test-{}-{idx}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data_dir);
        std::fs::create_dir_all(&data_dir)?;
        let api_port = free_port()?;
        let p2p_port = free_port()?;

        // `--data-dir` is a GLOBAL flag (before the subcommand); `--api-port`/`--port` are `start` flags.
        let mut args: Vec<String> = vec![
            "--data-dir".into(),
            data_dir.to_str().unwrap().into(),
            "start".into(),
            "--no-economy".into(),
            "--foreground".into(),
            "--no-mdns".into(),
            "--api-port".into(),
            api_port.to_string(),
            "--port".into(),
            p2p_port.to_string(),
        ];
        if let Some(d) = dial {
            args.push("--bootstrap".into());
            args.push(d.into());
        }

        let child = Command::new(&self.ce_bin)
            .args(&args)
            .env("CE_NO_AUTOBOOTSTRAP", "1") // isolated: do not join the real mesh
            .env("TMPDIR", "/tmp")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("spawn `{}` (set $CE_BIN?)", self.ce_bin))?;

        self.guards.push(NodeGuard { child, data_dir: data_dir.clone() });

        let api = format!("http://127.0.0.1:{api_port}");
        let token = wait_for_token(&data_dir).await.context("node never wrote api.token")?;
        let client = CeClient::with_token(api.clone(), Some(token.clone()));
        wait_for_health(&client).await.context("node API never became healthy")?;
        let node_id = client.status().await.context("node /status failed")?.node_id;
        let peer_id = fetch_peer_id(&api, &token).await.context("read peer id from /bootstrap")?;
        Ok(TestNode { client, node_id, api, peer_id, p2p_port })
    }

    /// Poll `cond` every 100ms until it is true or `timeout` elapses.
    pub async fn assert_eventually<F: Fn() -> bool>(&self, cond: F, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        loop {
            if cond() {
                return Ok(());
            }
            if start.elapsed() >= timeout {
                return Err(anyhow!("assert_eventually timed out after {timeout:?}"));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// A running node under test. Cheap to clone (holds a `CeClient` + its ids); the process lifetime is
/// owned by the [`Harness`].
#[derive(Clone)]
pub struct TestNode {
    pub client: CeClient,
    pub node_id: String,
    pub api: String,
    /// The node's libp2p PeerId (read off `GET /bootstrap`) — distinct from `node_id` (the Ed25519
    /// CE identity). Used to build a peer's dial address.
    pub peer_id: String,
    /// The libp2p listen port this node was assigned.
    pub p2p_port: u16,
}

impl TestNode {
    /// The multiaddr another node can `--bootstrap` to dial this one directly on loopback.
    pub fn dial_addr(&self) -> String {
        format!("/ip4/127.0.0.1/tcp/{}/p2p/{}", self.p2p_port, self.peer_id)
    }

    /// Drive a directed mesh request to `to` on `topic`; returns the reply bytes.
    pub async fn request(&self, to: &str, topic: &str, payload: &[u8], timeout_ms: u64) -> Result<Vec<u8>> {
        self.client.request(to, topic, payload, timeout_ms).await
    }

    /// Run a background module that answers every request on `topic` with `f(payload)` (via the
    /// standard `ce_rs::serve` loop — subscribe + reply). Drop the returned [`Responder`] to stop it.
    pub fn responder<F>(&self, topic: &'static str, f: F) -> Responder
    where
        F: Fn(Vec<u8>) -> Vec<u8> + Send + Sync + 'static,
    {
        let ce = self.client.clone();
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let handler = FnHandler { f: Arc::new(f), topic };
        let task = tokio::spawn(async move {
            let _ = serve(&ce, &[topic], &handler, async {
                let _ = stop_rx.await;
            })
            .await;
        });
        Responder { stop: Some(stop_tx), task }
    }
}

/// A running responder; stops when dropped.
pub struct Responder {
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Responder {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        self.task.abort();
    }
}

struct FnHandler {
    f: Arc<dyn Fn(Vec<u8>) -> Vec<u8> + Send + Sync>,
    #[allow(dead_code)]
    topic: &'static str,
}

impl Handler for FnHandler {
    async fn handle(&self, req: Request) -> Vec<u8> {
        (self.f)(req.payload)
    }
}

struct NodeGuard {
    child: Child,
    data_dir: PathBuf,
}

impl Drop for NodeGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.data_dir);
    }
}

/// Reserve a free TCP port by binding `:0` and reading it back (the socket is then dropped; a brief
/// race window, acceptable for tests).
fn free_port() -> Result<u16> {
    let l = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}

/// Wait (up to ~30s) for the node to write its `api.token` into the data dir, then return it.
async fn wait_for_token(data_dir: &std::path::Path) -> Result<String> {
    let path = data_dir.join("api.token");
    for _ in 0..300 {
        if let Ok(s) = std::fs::read_to_string(&path) {
            let t = s.trim().to_string();
            if !t.is_empty() {
                return Ok(t);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow!("no api.token at {}", path.display()))
}

/// Read the node's libp2p PeerId off `GET /bootstrap` (shape `{"peers":["/p2p/<id>"]}`). The
/// listen addr is added by the harness (it knows the port); only the identity comes from here.
async fn fetch_peer_id(api: &str, token: &str) -> Result<String> {
    let v: serde_json::Value = reqwest::Client::new()
        .get(format!("{api}/bootstrap"))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let addr = v["peers"][0]
        .as_str()
        .ok_or_else(|| anyhow!("no peers[0] in /bootstrap: {v}"))?;
    Ok(addr.rsplit("/p2p/").next().unwrap_or(addr).to_string())
}

/// Wait (up to ~30s) for `GET /health` to succeed.
async fn wait_for_health(client: &CeClient) -> Result<()> {
    for _ in 0..300 {
        if client.health().await.unwrap_or(false) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow!("node API not healthy"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // T3 (module↔module comms): a module answers on a topic, another drives it over the node's Bus,
    // the reply comes back. The co-located self-request path — deterministic, no peering. Needs `ce`
    // on PATH (or $CE_BIN); ignored by default so `cargo test` stays hermetic. Run with:
    //   cargo test -p ce-test -- --ignored
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "spawns a real `ce` node; run explicitly with --ignored"]
    async fn module_to_module_echo_over_the_bus() {
        let mut h = Harness::new();
        let node = h.node().await.expect("ephemeral node");
        let _echo = node.responder("test/echo", |p| p); // module B: echo
        // give the serve loop a moment to subscribe
        tokio::time::sleep(Duration::from_millis(800)).await;
        // module A drives B over the Bus and gets the reply back:
        let reply = node
            .request(&node.node_id, "test/echo", b"round-trip", 5_000)
            .await
            .expect("request");
        assert_eq!(reply, b"round-trip");
    }

    // T3 (cross-node module↔module): module B runs on node `dev`, module A on node `hub` drives it
    // over REAL libp2p (hub dials dev directly, no relay, no mDNS). This is the transparency invariant
    // across the wire — the same `request`/`responder` code as the co-located case, now over the mesh.
    //   cargo test -p ce-test -- --ignored
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "spawns two real `ce` nodes; run explicitly with --ignored"]
    async fn cross_node_request_over_the_mesh() {
        let mut h = Harness::new();
        let dev = h.node().await.expect("seed node");
        let hub = h.peer_of(&dev).await.expect("peer node dialing the seed");
        let _echo = dev.responder("test/echo", |mut p| {
            p.extend_from_slice(b"-pong");
            p
        });
        // let the direct connection settle + the serve loop subscribe on `dev`.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        // module A (on hub) drives module B (on dev) across the wire, addressed by dev's node_id:
        let reply = hub
            .request(&dev.node_id, "test/echo", b"ping", 10_000)
            .await
            .expect("cross-node request");
        assert_eq!(reply, b"ping-pong");
    }
}
