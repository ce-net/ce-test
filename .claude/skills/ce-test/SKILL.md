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
