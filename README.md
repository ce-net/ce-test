# ce-test — the CE testing harness

Make it trivial to test **the substrate, one module, and direct module↔module communication** — the
three tiers that matter (not e2e for every line). `ce-test` spins ephemeral, **chain-free**
(`--no-economy`) `ce` nodes, runs your modules on them, drives real mesh request/reply between them,
lets you assert, and tears everything down automatically.

`ce-test` is the framework/API + CLI **only** — the actual tests live in the ceapps that use it. See
**[GUIDE.md](./GUIDE.md)** for the full guide: the mental model, how to expand and build on it, and five
things this makes possible that traditional systems can't.

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
| `h.on(On::alias("relay"))` | A handle (`RemoteNode`) to a **real, already-running fleet node**, driven over the mesh from your **local node as controller**. Ships no code; the capability under test must already run there. The interim distributed-testing path (no `fetch-by-CID` needed). Target by id (`On::node`/`On::parse("node:…")`) or wallet alias. |
| `node.responder(topic, f)` | Run a background module that answers every request on `topic` with `f(payload)` (via `ce_rs::serve`). Drop the returned `Responder` to stop it. |
| `node.request(to, topic, payload, timeout_ms)` | Drive a directed mesh request; returns the reply bytes. |
| `remote.request(topic, payload, timeout_ms)` | Drive a request at a `RemoteNode` (the `to` is fixed to that node), routed controller → libp2p → remote. |
| `remote.reachable()` | Best-effort: is the remote in the controller's atlas right now? Use it to **skip** a fleet-only suite cleanly when there is no fleet in reach. |
| `node.dial_addr()` | The `/ip4/…/tcp/…/p2p/…` multiaddr another node can `--bootstrap` to. |
| `h.assert_eventually(cond, timeout)` | Poll a condition until true or timeout (the mesh is async). |

`TestNode` carries `client` (a `ce_rs::CeClient`), `node_id` (the Ed25519 CE identity), `peer_id` (the
libp2p PeerId), `api`, and `p2p_port`. `RemoteNode` carries the remote `node_id` + the controller client
(`RemoteNode::via(controller, id)` builds one over any controller — e.g. the relay driving another node).

**`h.on` vs fan-out.** `h.on` targets exactly **one** node. Running a workload across *many* machines
(`fleet=mine`, `org:x`) is core ce-net **distribution** — a capability every app *consumes*, not something
a test harness reimplements — so `On`/`h.on` deliberately reject multi-node selectors. When that core
capability lands, the runner's non-local placement rides it; `h.on` stays the single-node primitive.

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

A suite in your ceapp is a normal `cargo test`:

```bash
CE_BIN=$(command -v ce) cargo test -p your-app --test comms -- --ignored
```

Tests that spawn a real node are `#[ignore]` by default so `cargo test` stays hermetic — run them
explicitly with `--ignored`. Nodes come up in ~1s; a full 2-node comms round-trip runs in ~2s.

## The CLI + `cetest.toml`

`ce-test` also ships a runner. A `cetest.toml` catalogs the suites (each an actual test in a ceapp using
this API); `ce-test` runs them and reports.

```toml
[defaults]
ignored = true       # node-spawning suites are #[ignore]
dev_link = true      # run tools/ce-dev-link first (resolve cross-repo WIP deps)

[[suite]]
name = "my-comms"
tier = "T3"
path = "your-app"    # repo dir, relative to this file
test = "comms"       # cargo test --test comms
features = "serve"   # optional
where = "local"      # local now; fleet | org:x | node:<id> | relay (placement — no machine names)
```

```bash
ce-test list                          # the catalog
ce-test list --json                   # the catalog, machine-readable
ce-test run                           # all suites, report PASS/FAIL/SKIP
ce-test run --suite my-comms          # one suite
ce-test run --tier T3 --on fleet=mine # a tier, across the fleet (distributed exec: see GUIDE §7)
ce-test run --json                    # machine-readable results for CI (see below)
```

`--json` emits `{ "suites": [ { name, tier, passed, skipped, secs, note } ], "summary": { total,
passed, failed, skipped } }` on stdout (child test output is captured, so stdout stays pure JSON — a
tail of a failing suite's output lands in its `note`); the process still exits non-zero on any failure.
Gate CI on `summary.failed == 0`.

Placement is **declared** (`where` / `--on`), never coded — you never name a machine. `where != local`
runs over the mesh via the core ce-net distributed-run capability; `ce-test` does not build its own
distribution. Today `local` is wired; non-local suites report `SKIP` with the reason.

### Installed through the one app system

`ce-test` ships a `ceapp.toml`, so it is installed like every other capability — through the one
cap-gated app system, not as a bespoke standalone binary (`ce app install ce-test`; `--on <target>` to
place it on a fleet node). It runs to completion (not a daemon) and provides the `test` capability, so a
future `ce test` routes to it identically local or across the mesh. This is the anti-sprawl rule applied
to dev tooling: the test runner is a ceapp like anything else. (Artifacts are filled by `ce-publish` at
release time; a local install wants a `cargo build --release` first.)

## Notes / gotchas

- `--data-dir` is a **global** `ce` flag (before `start`); `--api-port`/`--port` are `start` flags. The
  harness places them correctly — this is only relevant if you drive `ce` by hand.
- Isolated nodes never join the real mesh (`CE_NO_AUTOBOOTSTRAP=1`, `--no-mdns`), so tests can't leak
  onto `ce-net.com` and are deterministic.

## Roadmap

- ~~`h.on(target)` — a handle bound to a **real fleet node** over the mesh (drive-remote-nodes mode).~~ **Done** (`h.on` / `RemoteNode`).
- `h.install(app, On::…)` — deploy a ceapp onto a harness node over the mesh (the real install path).
- `h.arduino(alias)` — attach a **real board** (env-gated via `ce onboard`); emulated locally today.
- Wire `where != local` onto the core ce-net distributed-run capability; fold in `ce-ci` sharding.
- `#[ce_test::test]` proc-macro + the `@ce-net/test` TS mirror.

See **[GUIDE.md](./GUIDE.md)** §7 for the full state & roadmap and why distribution is substrate work
`ce-test` *calls*, not something it builds.
