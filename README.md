# ce-test — the CE testing harness

Make it trivial to test **the substrate, one module, and direct module↔module communication** — the
three tiers that matter (not e2e for every line). `ce-test` spins ephemeral, **chain-free**
(`--no-economy`) `ce` nodes, runs your modules on them, drives real mesh request/reply between them,
lets you assert, and tears everything down automatically.

This is the foundation of `ce test` (design: `PLAN/ce-testing-framework.md`).

## What it gives you

```rust
use ce_test::Harness;

let mut h = Harness::new();

// One node, a module answering on a topic, another driving it over the node's Bus:
let node  = h.node().await?;
let _echo = node.responder("test/echo", |p| p);       // module B
let reply = node.request(&node.node_id, "test/echo", b"hi", 5_000).await?;  // module A
assert_eq!(reply, b"hi");

// Two nodes over REAL libp2p (the peer dials the seed directly — no relay, no mDNS):
let dev = h.node().await?;
let hub = h.peer_of(&dev).await?;
let _r  = dev.responder("svc/op", handler);
let out = hub.request(&dev.node_id, "svc/op", &payload, 10_000).await?;
```

All nodes drop → killed, data-dirs wiped. No ports to pick, no cleanup to write.

## API

| Call | What it does |
|---|---|
| `Harness::new()` | A fresh topology. Uses `$CE_BIN` (default `ce` on `PATH`). |
| `h.node()` | An **isolated** ephemeral node (`--no-economy`, own temp data-dir + free ports, `CE_NO_AUTOBOOTSTRAP=1`). Waits for the API to be live. |
| `h.peer_of(&seed)` | A second node that **dials `seed` directly** and joins its isolated mesh (no relay, no mDNS). For cross-node T3 comms. |
| `node.responder(topic, f)` | Run a background module that answers every request on `topic` with `f(payload)` (via `ce_rs::serve`). Drop the returned `Responder` to stop it. |
| `node.request(to, topic, payload, timeout_ms)` | Drive a directed mesh request; returns the reply bytes. |
| `node.dial_addr()` | The `/ip4/…/tcp/…/p2p/…` multiaddr another node can `--bootstrap` to. |
| `h.assert_eventually(cond, timeout)` | Poll a condition until true or timeout (the mesh is async). |

`TestNode` carries `client` (a `ce_rs::CeClient`), `node_id` (the Ed25519 CE identity), `peer_id` (the
libp2p PeerId), `api`, and `p2p_port`.

## Writing a comms test in your module's repo

Add `ce-test` as a **dev-dependency** and write a `tests/comms.rs` — the same shape every T3 suite takes.
Expose your app's real handler as a library function (a `[lib]` next to the `[[bin]]`) so the test
drives the **real module code**, not a re-implementation:

```toml
[dev-dependencies]
ce-test = { git = "https://github.com/ce-net/ce-test" }
tokio   = { version = "1", features = ["rt-multi-thread", "macros", "time"] }
```

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns real `ce` nodes; run with --ignored"]
async fn my_module_answers_over_the_mesh() {
    let mut h = ce_test::Harness::new();
    let dev = h.node().await.unwrap();
    let hub = h.peer_of(&dev).await.unwrap();
    let _svc = dev.responder("my/op", my_crate::handle);   // your real handler
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let out = hub.request(&dev.node_id, "my/op", b"in", 10_000).await.unwrap();
    assert_eq!(out, b"expected");
}
```

## Running

```bash
cargo test -p ce-test -- --ignored --nocapture   # the harness's own demo suites
CE_BIN=/path/to/ce cargo test -- --ignored        # pin a specific `ce` binary
```

Tests that spawn a real node are `#[ignore]` by default so `cargo test` stays hermetic — run them
explicitly with `--ignored`. Nodes come up in ~1s; a full 2-node comms round-trip runs in ~2s.

## Notes / gotchas

- `--data-dir` is a **global** `ce` flag (before `start`); `--api-port`/`--port` are `start` flags. The
  harness places them correctly — this is only relevant if you drive `ce` by hand.
- Isolated nodes never join the real mesh (`CE_NO_AUTOBOOTSTRAP=1`, `--no-mdns`), so tests can't leak
  onto `ce-net.com` and are deterministic.

## Roadmap

- `h.install(app, On::…)` — deploy a ceapp onto a harness node over the mesh (the real install path).
- `h.arduino(alias)` — bring a **real board** into a test topology, with an emulated-board fallback for CI.
- `#[ce_test::test]` proc-macro + a `ce test` CLI ceapp (folding in `e2e/` + `integration/` + `ce-ci/`).
