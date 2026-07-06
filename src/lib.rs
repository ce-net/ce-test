//! ce-test — the CE testing **framework/API** (guide: `GUIDE.md`; API + CLI: `README.md`).
//!
//! This crate is JUST the substrate: an SDK any ceapp adds as a dev-dependency to write tests — like
//! `cargo test`, but the nodes are real and can be spread across many machines. It spins ephemeral,
//! **chain-free** (`--no-economy`) nodes, runs modules on them, drives **direct module↔module**
//! communication over the mesh, asserts, and tears everything down. **The actual tests live in the
//! ceapps that use this API, never here** (see `ce-conntest`'s `tests/comms.rs` for the pattern).
//!
//! Topologies: `h.node()` (an isolated node — the co-located self-request path), `h.peer_of(seed)`
//! (a second node dialing the seed directly over real libp2p — cross-node), `h.arduino(alias)`
//! (a board — emulated as a local node in CI; a real board attaches via the installed `ce onboard`
//! ceapp, env-gated), and `h.on(target)` (drive a REAL, already-running fleet node over the mesh from
//! the operator's local node as controller — the interim distributed-testing path, no code shipped).
//! Next: `ce app install` in the harness + the distributed `ce test` runner (fold in `ce-ci`) so one
//! command runs a ceapp's suites across the fleet — that fan-out is core ce-net distribution the
//! harness *consumes*, never a test-specific distributor it builds.
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

pub mod config;
pub mod runner;

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

    /// Bring a **board** into the topology and return a handle to it. Real hardware is env-gated
    /// (`CE_TEST_ARDUINO_<ALIAS>` naming a reachable node, stood up via the installed `ce onboard`
    /// ceapp — shelled over the mesh, NOT a Cargo dependency: the framework must not depend on apps).
    /// By default — and in CI — this is an **emulated board**: a local chain-free node standing in for
    /// the board, so board-shaped tests run hardware-free.
    pub async fn arduino(&mut self, _alias: &str) -> Result<TestNode> {
        // Emulated board == a local chain-free node. (A real board attaches via `ce onboard`, env-gated.)
        self.spawn(None).await
    }

    /// Bind a handle to a **real, already-running fleet node** reached over the mesh — the interim
    /// distributed-testing path. Unlike [`node`](Self::node)/[`peer_of`](Self::peer_of) (which spawn
    /// *ephemeral, isolated* nodes this harness owns), `on` does **not** spawn anything: it drives an
    /// existing node on the real network, using the operator's **local `ce` node as the controller**
    /// (`request` is routed local-controller → libp2p → the remote node). The harness never owns the
    /// controller's or the remote's lifetime — nothing is killed on drop.
    ///
    /// This is deliberately NOT a distribution system (that is core ce-net functionality every app
    /// consumes, not ce-test's to build — see the repo GUIDE). `on` ships **no code** to the remote;
    /// the capability under test must already be running there (a responder on the topic you drive).
    /// Moving a test artifact to a heterogeneous machine and running it *there* is a separate path
    /// gated on the p2p artifact keystone (`fetch-by-CID`).
    ///
    /// `target` is a [`On`]: an explicit 64-hex node id (`On::node("…")` / `On::parse("node:…")`) or a
    /// **wallet alias** (`On::alias("relay")`), resolved from the LOCAL wallet (`ce wallet ls`) so a
    /// suite names a friendly alias, never a pasted 64-hex id and never a machine address.
    ///
    /// Errors if the local controller node is not running/healthy (a suite that needs a live fleet
    /// should be `#[ignore]`d and gated by an env var, then skip cleanly on this error — the same
    /// honest "no fleet → skip" contract the runner uses for non-local placement).
    ///
    /// ```no_run
    /// # async fn demo() -> anyhow::Result<()> {
    /// use ce_test::{Harness, On};
    /// let mut h = Harness::new();
    /// let relay = h.on(On::alias("relay")).await?;          // a real fleet node, by friendly name
    /// if !relay.reachable().await { return Ok(()); }        // no fleet in reach → skip
    /// let reply = relay.request("test/echo", b"hi", 5_000).await?;  // drive it over the mesh
    /// assert_eq!(reply, b"hi");
    /// # Ok(()) }
    /// ```
    pub async fn on(&mut self, target: On) -> Result<RemoteNode> {
        let node_id = match target {
            On::Node(id) => normalize_node_id(&id)?,
            On::Alias(alias) => resolve_alias(&alias)?,
        };
        let controller = CeClient::local();
        if !controller.health().await.unwrap_or(false) {
            return Err(anyhow!(
                "h.on(...) needs a running local `ce` node as the mesh controller, but its API is not \
                 healthy — start it with `ce start` (a suite that needs a live fleet should #[ignore] \
                 and skip when this fails)"
            ));
        }
        Ok(RemoteNode::via(controller, node_id))
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

/// A target for [`Harness::on`] — a single, already-running fleet node.
///
/// Multi-node selectors (`fleet=mine`, `org:x`) are deliberately absent: fanning a workload across
/// many machines is the core ce-net *distribution* capability every app consumes, not something a
/// test handle should reimplement. `On` names exactly one node — by id or by local wallet alias.
#[derive(Debug, Clone)]
pub enum On {
    /// An explicit 64-hex CE node id.
    Node(String),
    /// A wallet alias (resolved from the local `ce` wallet to a full node id), e.g. `"relay"`.
    Alias(String),
}

impl On {
    /// A target from an explicit 64-hex node id.
    pub fn node(id: impl Into<String>) -> On {
        On::Node(id.into())
    }

    /// A target from a wallet alias (resolved locally via `ce wallet`).
    pub fn alias(name: impl Into<String>) -> On {
        On::Alias(name.into())
    }

    /// Parse a placement string (the runner's `where` / CLI `--on` vocabulary) into a single-node
    /// target: `node:<hex>` / `node=<hex>` or a bare 64-hex id → [`On::Node`]; anything else → an
    /// [`On::Alias`] (so `relay`, `desktop`, `unoq` resolve through the wallet). Multi-node forms
    /// (`fleet`, `org:x`) are not single-node targets and are rejected — they belong to the
    /// distribution capability, not `on`.
    pub fn parse(s: &str) -> Result<On> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix("node:").or_else(|| s.strip_prefix("node=")) {
            return Ok(On::Node(normalize_node_id(rest)?));
        }
        if s == "fleet" || s.starts_with("fleet=") || s.starts_with("org:") || s.starts_with("org=") {
            return Err(anyhow!(
                "`{s}` selects many nodes; `On`/`h.on` targets exactly one — use the distribution \
                 capability for fan-out, or name a single `node:<id>` / wallet alias"
            ));
        }
        if is_node_id(s) {
            return Ok(On::Node(s.to_string()));
        }
        Ok(On::Alias(s.to_string()))
    }
}

/// A handle to a **real, already-running remote node**, driven over the mesh from the local
/// controller. Cheap to clone; owns no process lifetime (see [`Harness::on`]).
#[derive(Clone)]
pub struct RemoteNode {
    /// The local, mesh-connected node used to route requests to the remote (the operator's `ce`).
    controller: CeClient,
    /// The remote node's 64-hex CE id.
    pub node_id: String,
}

impl RemoteNode {
    /// Build a handle that drives node `node_id` through an explicit `controller` client, instead of
    /// the operator's local node ([`Harness::on`] uses the local node). Use this to drive a remote
    /// from a non-local controller (e.g. the relay as controller), or in tests.
    pub fn via(controller: CeClient, node_id: impl Into<String>) -> RemoteNode {
        RemoteNode { controller, node_id: node_id.into() }
    }

    /// Drive a directed mesh request at this remote node on `topic`; returns the reply bytes.
    /// Routed local-controller → libp2p → remote. The remote must already run a responder on `topic`
    /// (this ships no code); errors on timeout / unreachable / no responder.
    pub async fn request(&self, topic: &str, payload: &[u8], timeout_ms: u64) -> Result<Vec<u8>> {
        self.controller.request(&self.node_id, topic, payload, timeout_ms).await
    }

    /// Best-effort: is the remote currently visible to the controller's capacity atlas? Useful to
    /// **skip** a fleet-only suite cleanly when the node is not in reach. A `false` here (or a
    /// `request` timeout) is the honest "no fleet → skip" signal; it never falls back to local.
    pub async fn reachable(&self) -> bool {
        match self.controller.atlas().await {
            Ok(atlas) => atlas.iter().any(|e| e.node_id == self.node_id),
            Err(_) => false,
        }
    }

    /// The local controller client, for advanced use (e.g. driving several remotes from one).
    pub fn controller(&self) -> &CeClient {
        &self.controller
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

/// A node this harness spawned, killed + wiped on drop.
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

/// True iff `s` is a well-formed 64-hex CE node id.
fn is_node_id(s: &str) -> bool {
    let s = s.trim();
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Validate + lowercase a 64-hex node id (the form `/mesh/request` routes on).
fn normalize_node_id(id: &str) -> Result<String> {
    let id = id.trim();
    if !is_node_id(id) {
        return Err(anyhow!("not a 64-hex CE node id: `{id}`"));
    }
    Ok(id.to_ascii_lowercase())
}

/// Resolve a wallet alias to a full node id by reading the LOCAL node's `wallet.toml` (self-contained:
/// it reads this machine's own wallet, hardcoding no ids). The node data dir is the same one ce-rs
/// uses (`ProjectDirs("", "", "ce")`). NOTE: this parses `wallet.toml` directly because `ce wallet`
/// has no full-id resolve command yet (logged as tooling pain in FINDINGS); swap to `ce wallet
/// resolve <alias>` once it exists.
fn resolve_alias(alias: &str) -> Result<String> {
    let dir = directories::ProjectDirs::from("", "", "ce")
        .ok_or_else(|| anyhow!("cannot locate the ce data dir to resolve wallet alias `{alias}`"))?
        .data_dir()
        .to_path_buf();
    resolve_alias_in(&dir, alias)
}

/// The pure, testable core of [`resolve_alias`]: read `<data_dir>/wallet.toml` and return the full
/// node id stored under `[entries.<alias>]`.
fn resolve_alias_in(data_dir: &std::path::Path, alias: &str) -> Result<String> {
    let path = data_dir.join("wallet.toml");
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("read wallet {} (is the local node set up?)", path.display()))?;
    let doc: toml::Value = text.parse().with_context(|| format!("parse {}", path.display()))?;
    let id = doc
        .get("entries")
        .and_then(|e| e.get(alias))
        .and_then(|e| e.get("node_id"))
        .and_then(|n| n.as_str())
        .ok_or_else(|| anyhow!("no wallet alias `{alias}` (see `ce wallet ls`) in {}", path.display()))?;
    normalize_node_id(id)
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

    #[test]
    fn node_id_recognition() {
        let good = "21f5c206ffbf88d7bebdf9078d687e30be5b9a3c6e7ac752e018a559faf171d4";
        assert!(is_node_id(good));
        assert!(!is_node_id("relay")); // an alias
        assert!(!is_node_id(&good[..63])); // too short
        assert!(!is_node_id(&format!("{good}0"))); // too long
        assert!(!is_node_id("zz5f206ffbf88d7bebdf9078d687e30be5b9a3c6e7ac752e018a559faf171d4x")); // non-hex
    }

    #[test]
    fn normalize_lowercases_and_validates() {
        let up = "21F5C206FFBF88D7BEBDF9078D687E30BE5B9A3C6E7AC752E018A559FAF171D4";
        assert_eq!(normalize_node_id(up).unwrap(), up.to_ascii_lowercase());
        assert!(normalize_node_id("nope").is_err());
    }

    #[test]
    fn parse_targets() {
        let id = "21f5c206ffbf88d7bebdf9078d687e30be5b9a3c6e7ac752e018a559faf171d4";
        // Explicit id, and the node:/node= prefixes, all → Node.
        for s in [id.to_string(), format!("node:{id}"), format!("node={id}")] {
            assert!(matches!(On::parse(&s).unwrap(), On::Node(got) if got == id));
        }
        // A friendly name → Alias.
        assert!(matches!(On::parse("relay").unwrap(), On::Alias(a) if a == "relay"));
        // Multi-node selectors are rejected — they are not single-node targets.
        for s in ["fleet", "fleet=mine", "org:drones", "org=work"] {
            assert!(On::parse(s).is_err(), "{s} must be rejected as multi-node");
        }
    }

    #[test]
    fn alias_resolves_from_wallet_toml() {
        let dir = std::env::temp_dir().join(format!("ce-test-wallet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let full = "25df8f15853855c4cd2c5769cbc9789bf156534356ffead3b67c2c395f6d8ac1";
        std::fs::write(
            dir.join("wallet.toml"),
            format!("[entries.desktop]\nnode_id = \"{full}\"\norgs = []\n"),
        )
        .unwrap();

        assert_eq!(resolve_alias_in(&dir, "desktop").unwrap(), full);
        assert!(resolve_alias_in(&dir, "nonesuch").is_err()); // unknown alias
        let _ = std::fs::remove_dir_all(&dir);
        assert!(resolve_alias_in(&dir, "desktop").is_err()); // missing wallet
    }
}
