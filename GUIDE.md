# The CE test & capability system — guide, extension, and 5 things you can't do anywhere else

**What this documents:** the system built from `ce-test` (this repo — the testing framework/API/CLI),
[`ce-blueprint`](https://github.com/ce-net/ce-blueprint) (chips-are-data planning), and
[`ce-onboard`](https://github.com/ce-net/ce-onboard) (one-command node bring-up), all riding the CE
substrate (mesh transport, capabilities, spawn, the portability contract). How to use it, how to expand
it, how to build on it — and five things it makes possible that traditional systems fundamentally cannot.

For the API reference and the "write a suite" quickstart, see [`README.md`](./README.md); this is the
deeper guide.

---

## 1. The mental model (why this is different before you write a line)

Three ideas do all the work:

1. **The fleet is one computer.** You never target a machine; you call a **capability** and declare
   *where* (`local` / `fleet` / `org:x` / `node:id`) — the substrate places it, traverses NAT, attenuates
   caps, and returns results. No hostnames, no ssh, no transport code.
2. **A test runs on REAL nodes over the REAL mesh.** `ce-test` spins actual chain-free `ce` nodes and
   drives module↔module traffic over libp2p — the same identity, wire format, and capability checks as
   production. Not mocks. The *same test* runs unmodified on your laptop, the relay, or a $3
   microcontroller (the transparency invariant).
3. **Hardware is data, not code.** A new chip is a `TargetDescriptor` (JSON) that `ce-blueprint` turns
   into a build/deliver/run plan. The OS never grows per chip; your test coverage of a new board is a
   file, not a fork.

The consequence: **the framework disappears.** You write `request()` and `assert`, and it happens to be
true across the planet and across silicon.

---

## 2. How to use it (quickstart)

Actual tests live in **your** ceapp (never in ce-test). Add it as a dev-dependency:

```toml
# your-app/Cargo.toml
[dev-dependencies]
ce-test = { git = "https://github.com/ce-net/ce-test" }
tokio   = { version = "1", features = ["rt-multi-thread", "macros", "time", "sync"] }
```

```rust
// your-app/tests/comms.rs — module A drives module B over a real mesh, then you assert.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns a real ce node; run with --ignored"]
async fn my_capability_answers_over_the_mesh() {
    let mut h = ce_test::Harness::new();
    let node  = h.node().await.unwrap();                     // a real chain-free node
    let _svc  = node.responder("my/op", my_app::handle);     // YOUR real handler
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let out = node.request(&node.node_id, "my/op", b"in", 5_000).await.unwrap();
    assert_eq!(out, b"expected");
}
```

Register it in `cetest.toml` and run the catalog:

```toml
[[suite]]
name = "my-comms"
tier = "T3"
path = "your-app"
test = "comms"
where = "local"     # or fleet | org:mine | node:<id>
```

```bash
ce-test list                 # the catalog
ce-test run                  # all suites
ce-test run --suite my-comms # one
ce-test run --tier T3 --on fleet=mine   # (design) run across the fleet — no machine names
```

**API surface:** `h.node()` (isolated node), `h.peer_of(seed)` (a second node dialing it over real
libp2p), `h.arduino(alias)` (a board — emulated in CI, real via `ce onboard`), `h.on(On::alias("relay"))`
(a `RemoteNode` handle to a **real, already-running fleet node** driven over the mesh from your local
node as controller — ships no code; single-node only, since fan-out is core ce-net distribution the
harness consumes), `node.responder(topic,f)`, `node.request(to,topic,payload,timeout)`,
`remote.request(topic,payload,timeout)` / `remote.reachable()`, `h.assert_eventually(cond,timeout)`.
Everything the harness *spawned* tears down on drop; `h.on` spawns nothing, so it owns no lifetime.

---

## 3. How to expand it

- **Add a test ceapp:** dev-dep `ce-test`, write `tests/*.rs`, add a `[[suite]]`. Exemplars:
  [`ce-conntest`](https://github.com/ce-net/ce-conntest) (comms),
  [`ce-blueprint`](https://github.com/ce-net/ce-blueprint) (a mesh capability),
  [`ce-arduino-bridge`](https://github.com/ce-net/ce-arduino-bridge) (peripherals).
- **Add a hardware target:** drop a `descriptors/<chip>.json` in `ce-blueprint` — it plans by
  *capability* (has_os? crypto? wasm? buses?), never by name. No code. (Porting playbooks — new runtime
  backend, new peripheral driver, tier promotion — are in the `ce` repo's `docs/portability.md`.)
- **Add a capability under test:** expose your app's real handler as a `serve_*` fn in its lib (like
  `ce_conntest::serve_responder`, `ce_blueprint::serve_capability`) so the suite drives the *real* code,
  not a re-implementation.
- **Add the framework to a new language:** the contract is the mesh wire format + `ce-rs`/`ce-ts` SDK;
  a `@ce-net/test` mirror gives this same API to JS/browser modules — same nodes, same suites.

---

## 4. How to build on top of it

- **Your app + its suite ship together.** The suite is the deploy-smoke gate: `ce-test run --on <target>`
  proves the real deployed path before release.
- **Compose capabilities.** Every installed ceapp makes the mesh strictly more capable, so a suite can
  stand up a graph of providers (`h.node()` × N, each running a capability) and test the *composed*
  behavior — the higher-order capability that emerges from wiring simple ones.
- **The distribution is not yours to build.** "Run this workload on these targets, return results" is a
  **core ce-net capability**; `ce-test`'s runner is a thin caller of it. You never write a scheduler, a
  transport, or a result collector — you declare `where`. (See `src/runner.rs` and the roadmap below.)

---

## 5. Five things you can do here that you cannot do with traditional systems

These are not "nicer" versions of existing tools — they are categorically impossible when your network
is mocked, your hardware is simulated, and your machines are hostnames in a config.

### 1) One test spanning your laptop, a relay on another continent, and a $3 chip in your pocket
A single `#[test]` can hold a local node, the relay, and a real ESP32 on a powerbank — and drive them
with the *same* `request()`, over the *same* libp2p mesh, with real identity and capability checks,
running the exact `.wasm` that ships. **Traditional systems can't:** they mock the network (losing
NAT/relay/roaming truth) or the hardware (losing the real chip), so "integration test" means
"integration of fakes." Here the test *is* production, minus the release.

### 2) A suite you run **while walking around the building**
Real transport means real geography: roaming between APs, NAT rebinding, the relay hairpin engaging and
disengaging — all become *assertable*. `ce-conntest`'s rolling latency/jitter/loss/throughput report
already does this. **Traditional systems can't:** a faked network has no mobility, so "flaky under real
roaming" is invisible until production. Your test suite becomes a *field instrument*.

### 3) Self-provisioning tests: the suite **onboards the machines it needs, mid-run, from bare hardware**
`h.arduino("unoq")` calls `ce onboard`, which brings a fresh, unprovisioned device up as a chain-free
node over the mesh, installs the module, runs the assertions, and returns the device on teardown — all
cap-scoped so authority only shrinks downward. **Traditional systems can't:** CI needs pre-provisioned
runners and a separate deploy pipeline. Here **the test is also the deployment** — it grows its own fleet
on demand and gives it back.

### 4) One test, **every language and every architecture** — polyglot contract tests with zero glue
A capability is `provide`/`need` over one frozen host-ABI with content-addressed, byte-checked replies.
So you drive a **Python** provider on a Raspberry Pi and a **Rust** provider on x86 with the *identical*
`request()`, and assert *byte-identical* replies — proving both satisfy the same contract. The
N-languages × M-architectures matrix is free, no per-target harness. **Traditional systems can't:**
cross-language, cross-arch conformance normally means a bespoke harness per pair.

### 5) **Emergent-behavior + authority-attenuation** tests of a live, self-healing system
Spawn is recursive and attenuating; capabilities compound; `ce-lane` revocation kills live flows. So a
test can stand up a *tree* of apps spawning apps (orchestrator → workers → inference → a protocol-link
that teaches the mesh a new transport), inject one attenuated capability at the root, and assert two
things traditional tests can't even express: (a) the **emergent** composed capability works end-to-end,
and (b) **authority only ever shrinks downward** — no child exceeds its parent. Then fuzz it: kill nodes,
revoke a cap mid-flow, and assert the system *self-heals* and *fails closed*.

---

## 6. Why it's so powerful — and why you should use it

- **The test is the truth.** Same bytes, transport, crypto, capabilities, and hardware as production. A
  green suite is evidence about the real system, not about a pile of mocks.
- **Scale is a config value, not a project.** Chain-free nodes are free to spawn; `where` is a string;
  distribution is a substrate capability you *call*. 1 node to 100, or your desk to three continents, is
  an edit.
- **Hardware coverage is a JSON file.** New chip → a descriptor. Framework, suites, and app bytes don't
  change.
- **You never build test infrastructure.** No scheduler, runners, network harness, or result bus — the
  substrate's job. You write `request()` and `assert`.
- **It compounds.** Every capability-providing ceapp anyone installs makes every test's reach strictly
  greater.

Use it because it collapses the three things that make distributed/embedded testing miserable —
faithfulness, provisioning, and heterogeneity — into declarations, and gives back tests that are true
about the real world, not a diorama of it.

---

## 7. State & roadmap (this repo)

- **Working now:** the `Harness` API (`node`/`peer_of`/`arduino`/`responder`/`request`/
  `assert_eventually`), the `ce-test` CLI (`run`/`list`) over `cetest.toml`, and `where = local`
  execution. Exemplar suites are green (`ce-conntest`, `ce-blueprint`, `ce-arduino-bridge`).
- **Next:** `h.on(target)` — a `TestNode` bound to a *real fleet node* over the mesh (drive-remote-nodes
  mode, no new substrate); then wire `where != local` onto the **core ce-net distributed-run capability**
  (ce-test must NOT build its own distribution — "run a workload on targets → results" is substrate work
  it *calls*). Fold in `ce-ci` sharding. `#[ce_test::test]` sugar and the `@ce-net/test` TS mirror.
- **One dependency to watch:** shipping a compiled suite to heterogeneous machines rides p2p
  artifact-by-CID distribution (the same keystone `ce onboard` uses for offline delivery).
