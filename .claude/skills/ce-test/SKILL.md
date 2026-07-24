---
name: ce-test
description: How to use and work on ce-test — the CE testing framework/API + CLI. Read before writing a test suite for a ceapp, or editing this repo. Ships with the repo (self-contained).
---

# ce-test — the CE testing framework/API

`ce-test` spins ephemeral, chain-free (`--no-economy`) real `ce` nodes, drives module↔module traffic
over the real mesh, lets you assert, and tears down. It is JUST the framework/substrate — **the actual
tests live in the ceapp that owns them**, never here.

Deeper guide (in this repo): `GUIDE.md` (mental model + 5 things impossible in traditional systems).
API + CLI reference: `README.md`.

## Write a suite (in YOUR ceapp, not here)

```toml
# your-app/Cargo.toml
[dev-dependencies]
ce-test = { git = "https://github.com/ce-net/ce-test" }
tokio   = { version = "1", features = ["rt-multi-thread", "macros", "time", "sync"] }
```

```rust
// your-app/tests/comms.rs
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns a real ce node; run with --ignored"]
async fn answers_over_the_mesh() {
    let mut h = ce_test::Harness::new();
    let node  = h.node().await.unwrap();
    let _svc  = node.responder("my/op", my_app::handle);   // YOUR real handler (expose it as a lib fn)
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let out = node.request(&node.node_id, "my/op", b"in", 5_000).await.unwrap();
    assert_eq!(out, b"expected");
}
```

Run it: `CE_BIN=$(command -v ce) cargo test -p your-app --test comms -- --ignored`.

## API
`Harness::new()`; `h.node()` (isolated node), `h.peer_of(seed)` (2nd node dialing it over real libp2p —
cross-node), `h.arduino(alias)` (a board; emulated locally, real via `ce onboard`), `h.on(On::alias(
"relay"))` (a `RemoteNode` handle to a **real, already-running fleet node** driven over the mesh from your
local node as controller — ships no code; the capability must already run there; single-node only, since
fan-out is core ce-net distribution the harness *consumes*), `node.responder(topic, f)`, `node.request(
to, topic, payload, timeout_ms)`, `remote.install(app, registry, grant)` (install a ceapp on the remote
over the mesh — the CLI's own cap-gated `mesh_app_install`; pair with request for the install→drive loop),
`remote.request(topic, payload, timeout_ms)` / `remote.reachable()`,
`h.assert_eventually(cond, timeout)`. The harness tears down everything it *spawned* on drop (`h.on`
spawns nothing). Node-spawning + fleet tests are `#[ignore]` and skip cleanly with no fleet in reach.

## Multi-node & replicated-state tests (the recipe)

Two real nodes over real libp2p: `let a = h.node().await?; let b = h.peer_of(&a).await?;` — `b` dials
`a` on loopback, an isolated 2-node mesh. To test **replicated state** (`ce-coord` `Merged`/
`Replicated`, or any app on it) converging across them, bridge each node's public `CeClient`
(`TestNode.client`, Clone) into ce-coord:

```rust
let a = h.node().await?;
let b = h.peer_of(&a).await?;                                   // 2nd node dialing A
let coord_a = Coord::with_client(a.client.clone()).await?;      // ce-coord bound to node A
let coord_b = Coord::with_client(b.client.clone()).await?;
let app_a = MyClient::open(&coord_a, std::slice::from_ref(&b.node_id)).await?;  // each follows the other
let app_b = MyClient::open(&coord_b, std::slice::from_ref(&a.node_id)).await?;
app_a.write(/* … */).await?;
h.assert_eventually(|| { app_b.refresh(); app_b.read() == expected }, Duration::from_secs(20)).await?;
```

- **Drive the REAL app code.** If the state machine/client lives in a `[[bin]]`, extract it to the
  crate's `lib.rs` so the test imports it (ce-sticky did exactly this: bin → lib+bin). A test that
  re-declares the ops proves nothing about the app.
- **Prove non-vacuity (mandatory).** A fast green can lie. Run a negative control that MUST fail —
  open the follower with NO peers (`&[]`) and assert the same `assert_eventually` TIMES OUT.
  (ce-screen: with peering it converges in ~0.5s; with none it times out at 20s.)
- **`assert_eventually`, never a fixed sleep.** ce-coord folds peer ops on `read`/`pull`, so call
  `refresh()`/`pull()` inside the condition closure.
- **ce-coord multi-writer LWW needs a TRUE Lamport clock**, not a wall-clock seed:
  `next_key = max(local, max_observed_lamport) + 1` (track `max_lamport` in the MergeMachine's fold),
  so a write that observed another always outranks it — else convergence is skew-dependent.
- Exemplars: `ce-screen/tests/convergence.rs`, `ce-sticky/tests/convergence.rs`.

Note on **local single-node self-request** (`node.request(&node.node_id, …)` to a `node.responder`
on the same node): treat it as best-effort — it was observed to time out against a long-running local
node. For a LOCAL client↔daemon on one machine, prefer a **loopback socket** (unix domain), not a
mesh self-call; cross-DEVICE requests (the two-node recipe above) are the solid path. (See
`ce-screen/tests/self_delivery.rs`, which probes this directly.)

## The CLI + cetest.toml
A repo-root `cetest.toml` catalogs suites; `ce-test [run|list] [--suite|--tier|--on] [--json]` runs them.
`--json` emits machine-readable results (`{suites,summary}`) on a pure-JSON stdout for CI (gate on
`summary.failed == 0`); child output is captured, with a tail of any failure in its `note`.
`where = local | fleet | org:x | node:<id> | relay` per suite (or `--on <target>`) declares placement —
**no machine names**. `local` is wired; non-local runs over the mesh via the **core ce-net distributed-
run capability** — ce-test does NOT build its own distribution.

## Working on this repo
- ce-test must **not** depend on any ceapp (Cargo). Reach apps by shelling `ce`/`ce app`/`ce onboard`.
- No test cases in the crate; keep them in the ceapps that use it (exemplars: ce-conntest, ce-blueprint,
  ce-arduino-bridge).
- Self-contained: no `PLAN/` / `~/ce-net` / `../` refs. Commit as Leif, no co-author, no emojis.
