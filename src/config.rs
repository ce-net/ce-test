//! `cetest.toml` — declarative test configuration the `ce-test` CLI reads.
//!
//! A suite says WHAT to run (a repo + an optional cargo test target) and WHERE (`where`), never HOW to
//! deploy it. `where = "local"` runs it here; `where = "fleet"` / `"org:x"` / `"node:<id>"` / `"relay"`
//! means "run it across the mesh" — the platform places it, you don't name machines. That deploy-
//! transparency is the whole point (see the module docs on the distributed runner).
//!
//! ```toml
//! [defaults]
//! ce_bin = "ce"       # the binary the harness spawns (default: `ce` on PATH)
//! ignored = true      # suites that spawn real nodes are #[ignore] — pass --ignored
//!
//! [[suite]]
//! name = "conntest-comms"
//! tier = "T3"
//! path = "ce-conntest"
//! test = "comms"      # cargo test -p <pkg> --test comms
//! where = "local"     # or: fleet | org:mine | node:<id> | relay
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// The whole `cetest.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub defaults: Defaults,
    /// Each `[[suite]]` block.
    #[serde(default, rename = "suite")]
    pub suites: Vec<Suite>,
}

/// Values applied to every suite unless the suite overrides them.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Defaults {
    /// Binary the harness spawns (exported as `$CE_BIN`). `None` → `ce` on `PATH`.
    pub ce_bin: Option<String>,
    /// Pass `--ignored` (node-spawning suites are `#[ignore]` to keep `cargo test` hermetic).
    pub ignored: Option<bool>,
    /// Run `tools/ce-dev-link` at the workspace root before suites (resolve cross-repo WIP deps).
    pub dev_link: Option<bool>,
}

/// One test ceapp's suite.
#[derive(Debug, Clone, Deserialize)]
pub struct Suite {
    /// Display name.
    pub name: String,
    /// Repo directory (relative to the config's dir), e.g. `ce-conntest`.
    pub path: String,
    /// Which testing tier this is (`T1`/`T2`/`T3`) — informational, filterable with `--tier`.
    pub tier: Option<String>,
    /// `-p <package>` (defaults to the repo's own package).
    pub package: Option<String>,
    /// `--test <name>` for an integration test; omit to run the crate's unit tests.
    pub test: Option<String>,
    /// `--features <list>` (e.g. `serve`).
    pub features: Option<String>,
    /// Override `defaults.ignored` for this suite.
    pub ignored: Option<bool>,
    /// WHERE to run: `local` (default) | `relay` | `fleet` | `org:<x>` | `node:<id>`. Non-local means
    /// "the platform places it over the mesh" — you never name a machine.
    #[serde(rename = "where")]
    pub location: Option<String>,
}

impl Config {
    /// Parse a `cetest.toml`.
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
    }

    /// Default config path: `$CE_TEST_CONFIG`, else `cetest.toml` in the cwd.
    pub fn default_path() -> PathBuf {
        std::env::var_os("CE_TEST_CONFIG").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("cetest.toml"))
    }
}

impl Suite {
    /// Effective location, defaulting to `local`.
    pub fn location(&self) -> &str {
        self.location.as_deref().unwrap_or("local")
    }
}
