//! `ce-test` — the CLI/runner for the CE testing framework.
//!
//! Reads a `cetest.toml` and runs the configured suites (each an actual test written in a ceapp that
//! uses the `ce_test` API). Placement is declared with `where` per suite / `--on <target>` globally —
//! you never name a machine; the platform runs it (local now, distributed over the mesh next).
//!
//! - `ce-test list [--config cetest.toml] [--json]`                        — the configured suites.
//! - `ce-test run  [--config …] [--suite NAME] [--tier T3] [--on fleet=mine] [--json]` — run + report.
//!
//! `--json` emits a machine-readable result (for CI / other tools) instead of the human table; the
//! process still exits non-zero if any suite failed.

use std::path::PathBuf;

use anyhow::Result;
use ce_test::config::Config;
use ce_test::runner::{run, RunOpts};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("run");
    let json = has_flag(&args, "--json");
    let cfg_path = flag(&args, "--config").map(PathBuf::from).unwrap_or_else(Config::default_path);
    // Suite paths resolve against the config's directory.
    let root = cfg_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    match cmd {
        "list" => {
            let cfg = Config::load(&cfg_path)?;
            if json {
                let items: Vec<_> = cfg
                    .suites
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "name": s.name,
                            "tier": s.tier,
                            "where": s.location(),
                            "path": s.path,
                            "test": s.test,
                        })
                    })
                    .collect();
                println!("{}", serde_json::json!({ "suites": items }));
                return Ok(());
            }
            println!("suites in {} ({}):", cfg_path.display(), cfg.suites.len());
            for s in &cfg.suites {
                let target = s.test.as_ref().map(|t| format!("--test {t}")).unwrap_or_else(|| "unit".into());
                println!(
                    "  {:<26} tier={:<3} where={:<10} {} [{}]",
                    s.name,
                    s.tier.as_deref().unwrap_or("-"),
                    s.location(),
                    s.path,
                    target
                );
            }
            Ok(())
        }
        "run" => {
            let cfg = Config::load(&cfg_path)?;
            let opts = RunOpts {
                only: flag(&args, "--suite"),
                tier: flag(&args, "--tier"),
                on: flag(&args, "--on"),
                root,
                capture: json, // --json: capture child output so stdout stays pure JSON
            };
            let report = run(&cfg, &opts);

            if json {
                println!("{}", report.to_json());
                if report.failed() > 0 {
                    std::process::exit(1);
                }
                return Ok(());
            }

            println!("\n── ce-test results ──");
            for r in &report.results {
                let tag = if r.skipped {
                    "SKIP"
                } else if r.passed {
                    "PASS"
                } else {
                    "FAIL"
                };
                let note = if r.note.is_empty() { String::new() } else { format!("  ({})", r.note) };
                println!("  [{tag}] {:<26} {:>5.1}s{note}", r.name, r.secs);
            }
            println!(
                "\n{} suites: {} passed, {} failed, {} skipped",
                report.results.len(),
                report.passed(),
                report.failed(),
                report.skipped()
            );
            if report.failed() > 0 {
                std::process::exit(1);
            }
            Ok(())
        }
        _ => {
            eprintln!("usage: ce-test [run|list] [--config cetest.toml] [--suite NAME] [--tier T1|T2|T3] [--on <target>] [--json]");
            Ok(())
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(|s| s.to_string())
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}
