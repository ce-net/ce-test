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

use crate::config::{Config, Suite};

/// Outcome of one suite.
pub struct SuiteResult {
    pub name: String,
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
}

/// Run the configured suites and collect a [`Report`]. Runs `tools/ce-dev-link` first if
/// `defaults.dev_link` is set (best-effort).
pub fn run(cfg: &Config, opts: &RunOpts) -> Report {
    if cfg.defaults.dev_link.unwrap_or(false) {
        let _ = std::process::Command::new("tools/ce-dev-link").current_dir(&opts.root).status();
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
                passed: false,
                skipped: true,
                secs: 0.0,
                note: format!(
                    "where={location}: distributed execution over the mesh is pending the p2p artifact path (fetch-by-CID); not run local silently"
                ),
            });
            continue;
        }

        let (passed, note) = run_local(cfg, s, &opts.root);
        results.push(SuiteResult {
            name: s.name.clone(),
            passed,
            skipped: false,
            secs: started.elapsed().as_secs_f64(),
            note,
        });
    }
    Report { results }
}

/// Run one suite locally via `cargo test`.
fn run_local(cfg: &Config, s: &Suite, root: &Path) -> (bool, String) {
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

    match cmd.status() {
        Ok(st) if st.success() => (true, String::new()),
        Ok(st) => (false, format!("cargo test exited {}", st.code().unwrap_or(-1))),
        Err(e) => (false, format!("spawn cargo failed: {e}")),
    }
}
