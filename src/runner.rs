//! The `ce-test` runner: given a [`Config`], run the selected suites and report.
//!
//! ## Local vs distributed (the deploy-transparency goal)
//! A suite's `where` decides placement, not the test. `local` runs `cargo test` here. Non-local
//! (`fleet`/`org:x`/`node:id`/`relay`) means "run it across the mesh" — the intended path builds each
//! suite as a runnable artifact and places+runs it with the SAME substrate primitives every app uses
//! (`ce app install --on …` / `ce-fn` spawn, cap-gated, NAT-traversed, atlas-placed), collecting
//! results over a mesh topic. That is deliberately ONE API with no machine names. **v1 wires `local`
//! fully**; a non-local suite is reported `SKIP` with the reason (the distributed executor rides the
//! p2p artifact-distribution keystone, `fetch-by-CID`, still landing) rather than silently running local.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use crate::config::{Config, Suite};

/// Outcome of one suite.
#[derive(Serialize)]
pub struct SuiteResult {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    pub passed: bool,
    pub skipped: bool,
    pub secs: f64,
    pub note: String,
}

/// The whole run.
pub struct Report {
    pub results: Vec<SuiteResult>,
}

impl Report {
    pub fn failed(&self) -> usize {
        self.results.iter().filter(|r| !r.passed && !r.skipped).count()
    }
    pub fn passed(&self) -> usize {
        self.results.iter().filter(|r| r.passed).count()
    }
    pub fn skipped(&self) -> usize {
        self.results.iter().filter(|r| r.skipped).count()
    }

    /// A machine-readable view for CI (`ce-test run --json`): `{ "suites": [...], "summary": {...} }`.
    /// Stable field names so a downstream tool can gate on `summary.failed == 0`.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "suites": self.results,
            "summary": {
                "total": self.results.len(),
                "passed": self.passed(),
                "failed": self.failed(),
                "skipped": self.skipped(),
            }
        })
    }
}

/// Filters + placement override for a run.
#[derive(Default)]
pub struct RunOpts {
    /// Only this suite (by name).
    pub only: Option<String>,
    /// Only suites of this tier.
    pub tier: Option<String>,
    /// Override every suite's `where` (the CLI `--on <target>`).
    pub on: Option<String>,
    /// Workspace root (the config's directory) — suite paths resolve against it.
    pub root: PathBuf,
    /// Capture each suite's child output instead of streaming it live (for `--json`: keeps our stdout
    /// pure JSON, and puts a tail of the failing output into the result note).
    pub capture: bool,
}

/// Run the configured suites and collect a [`Report`]. Runs `tools/ce-dev-link` first if
/// `defaults.dev_link` is set (best-effort).
pub fn run(cfg: &Config, opts: &RunOpts) -> Report {
    if cfg.defaults.dev_link.unwrap_or(false) {
        let mut cmd = std::process::Command::new("tools/ce-dev-link");
        cmd.current_dir(&opts.root);
        if opts.capture {
            // Keep stdout pure JSON: send dev-link's chatter to stderr, not our stdout.
            cmd.stdout(std::process::Stdio::null());
        }
        let _ = cmd.status();
    }

    let mut results = Vec::new();
    for s in &cfg.suites {
        if let Some(only) = &opts.only {
            if &s.name != only {
                continue;
            }
        }
        if let Some(tier) = &opts.tier {
            if s.tier.as_deref() != Some(tier.as_str()) {
                continue;
            }
        }

        let location = opts.on.as_deref().unwrap_or_else(|| s.location());
        let started = Instant::now();

        if location != "local" {
            // Distributed placement is the designed path (see module docs); not yet wired.
            results.push(SuiteResult {
                name: s.name.clone(),
                tier: s.tier.clone(),
                passed: false,
                skipped: true,
                secs: 0.0,
                note: format!(
                    "where={location}: distributed execution over the mesh is pending the p2p artifact path (fetch-by-CID); not run local silently"
                ),
            });
            continue;
        }

        let (passed, note) = run_local(cfg, s, &opts.root, opts.capture);
        results.push(SuiteResult {
            name: s.name.clone(),
            tier: s.tier.clone(),
            passed,
            skipped: false,
            secs: started.elapsed().as_secs_f64(),
            note,
        });
    }
    Report { results }
}

/// Run one suite locally via `cargo test`. When `capture`, the child's output is captured (not
/// streamed) so `--json` keeps stdout pure JSON; a tail of the output is folded into the note on
/// failure so CI still sees why it broke.
fn run_local(cfg: &Config, s: &Suite, root: &Path, capture: bool) -> (bool, String) {
    let dir = root.join(&s.path);
    if !dir.exists() {
        return (false, format!("path not found: {}", dir.display()));
    }

    let mut cmd = std::process::Command::new("cargo");
    cmd.current_dir(&dir).arg("test");
    if let Some(p) = &s.package {
        cmd.arg("-p").arg(p);
    }
    if let Some(t) = &s.test {
        cmd.arg("--test").arg(t);
    }
    if let Some(f) = &s.features {
        cmd.arg("--features").arg(f);
    }
    if let Some(bin) = &cfg.defaults.ce_bin {
        cmd.env("CE_BIN", bin);
    }
    // `--ignored` for node-spawning suites (suite override wins over the default).
    let ignored = s.ignored.or(cfg.defaults.ignored).unwrap_or(false);
    cmd.arg("--");
    if ignored {
        cmd.arg("--ignored");
    }

    if !capture {
        return match cmd.status() {
            Ok(st) if st.success() => (true, String::new()),
            Ok(st) => (false, format!("cargo test exited {}", st.code().unwrap_or(-1))),
            Err(e) => (false, format!("spawn cargo failed: {e}")),
        };
    }

    match cmd.output() {
        Ok(out) if out.status.success() => (true, String::new()),
        Ok(out) => {
            let mut buf = String::from_utf8_lossy(&out.stdout).into_owned();
            buf.push_str(&String::from_utf8_lossy(&out.stderr));
            let tail: String = buf.lines().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" | ");
            (false, format!("cargo test exited {}: {tail}", out.status.code().unwrap_or(-1)))
        }
        Err(e) => (false, format!("spawn cargo failed: {e}")),
    }
}
