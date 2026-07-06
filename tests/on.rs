//! Self-tests for `h.on(target)` / [`RemoteNode`] — the harness driving an already-running node over
//! the mesh. These test the *framework's own API* (not a ceapp): the ceapp-facing invariant "no ceapp
//! tests live in ce-test" is about product suites, not the harness verifying itself.
//!
//! - `remote_request_routes` (hermetic, `--ignored`): spins one real chain-free node, runs a responder
//!   on it, then drives it through a [`RemoteNode`] built on that node's own client — proving the
//!   `RemoteNode::request` path routes to a live responder end-to-end, with no fleet required.
//! - `on_live_fleet` (env-gated): the real thing — the local node as controller driving a real remote
//!   named by `$CE_TEST_ON_TARGET`, skipped cleanly when no fleet is in reach.

use ce_test::{Harness, On, RemoteNode};

/// The `RemoteNode` request path works against a real, running node + responder. Uses the co-located
/// self-request (controller == target node) so it needs no second machine, but exercises the exact
/// `RemoteNode::request` → node AppBus → responder → reply path `h.on` uses over the mesh.
#[tokio::test]
#[ignore = "spawns a real `ce` node; run with --ignored (needs $CE_BIN)"]
async fn remote_request_routes() {
    let mut h = Harness::new();
    let node = h.node().await.expect("spawn chain-free node");
    let _echo = node.responder("test/echo", |p| p); // module under test: echoes

    // Let the serve loop subscribe on the node before we drive it (same settle the comms test uses).
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    // Drive that node as if it were remote: a RemoteNode over its own client, targeting its own id.
    let remote = RemoteNode::via(node.client.clone(), node.node_id.clone());
    let reply = remote
        .request("test/echo", b"ping", 5_000)
        .await
        .expect("request routes to the responder");
    assert_eq!(reply, b"ping");
}

/// End-to-end over the real mesh: the operator's local node resolves + reaches a real remote node.
/// Gated on `$CE_TEST_ON_TARGET` (a node id or wallet alias) so CI without a fleet skips; also skips
/// if the target is not currently reachable. This proves everything `h.on` owns (resolution →
/// controller → reachability). Set `$CE_TEST_ON_TOPIC` to a topic the remote *actually serves* to opt
/// into asserting a full mesh round-trip reply (omitted by default: `h.on` ships no responder, so
/// there is no topic every remote is guaranteed to answer).
#[tokio::test]
#[ignore = "needs a live local node + a reachable remote; set $CE_TEST_ON_TARGET"]
async fn on_live_fleet() {
    let Ok(target) = std::env::var("CE_TEST_ON_TARGET") else {
        eprintln!("skip: set $CE_TEST_ON_TARGET (node id or wallet alias) to run");
        return;
    };
    let mut h = Harness::new();
    let remote = match h.on(On::parse(&target).expect("parse target")).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("skip: no local controller node ({e})");
            return;
        }
    };
    if !remote.reachable().await {
        eprintln!("skip: target `{target}` ({}) not in reach", remote.node_id);
        return;
    }
    eprintln!("ok: `{target}` resolved to {} and is reachable via the controller", remote.node_id);

    // Full round-trip only when the caller names a topic the remote serves.
    if let Ok(topic) = std::env::var("CE_TEST_ON_TOPIC") {
        let reply = remote
            .request(&topic, b"ping", 5_000)
            .await
            .unwrap_or_else(|e| panic!("request to {} on {topic} failed: {e}", remote.node_id));
        eprintln!("ok: {} replied {} bytes on {topic}", remote.node_id, reply.len());
    }
}

/// The install→drive loop over the real mesh: install a published ceapp on a real target through the
/// SAME cap-gated verb the CLI uses (`RemoteNode::install` → `mesh_app_install`), then drive it.
/// Gated on `$CE_TEST_ON_TARGET` + `$CE_TEST_INSTALL_APP` (the published app slug) + optional
/// `$CE_TEST_INSTALL_REGISTRY`; skips cleanly with no fleet/app. Proves apps install apps like the CLI.
#[tokio::test]
#[ignore = "needs a live controller + a reachable target + a published app; set the CE_TEST_INSTALL_* envs"]
async fn on_live_install() {
    let (Ok(target), Ok(app)) =
        (std::env::var("CE_TEST_ON_TARGET"), std::env::var("CE_TEST_INSTALL_APP"))
    else {
        eprintln!("skip: set $CE_TEST_ON_TARGET + $CE_TEST_INSTALL_APP to run");
        return;
    };
    let registry = std::env::var("CE_TEST_INSTALL_REGISTRY").unwrap_or_else(|_| "https://ce-net.com".into());
    let mut h = Harness::new();
    let remote = match h.on(On::parse(&target).expect("parse target")).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("skip: no local controller node ({e})");
            return;
        }
    };
    if !remote.reachable().await {
        eprintln!("skip: target `{target}` ({}) not in reach", remote.node_id);
        return;
    }
    let installed = remote
        .install(&app, &registry, None)
        .await
        .unwrap_or_else(|e| panic!("install {app} on {} failed: {e}", remote.node_id));
    eprintln!("ok: installed {} v{} on {}", installed.app, installed.version, remote.node_id);
}
