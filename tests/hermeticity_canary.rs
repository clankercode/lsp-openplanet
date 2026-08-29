//! B045 — canaries for the hermetic test gate (`just test-hermetic`).
//!
//! # Why this file exists
//!
//! Since B019 the `DEPENDENCY_<ID>` define is gated on a dependency actually
//! RESOLVING, so a test that reaches the machine's real Openplanet install
//! passes on the author's box and fails for everyone else. `just test-hermetic`
//! re-runs every test binary with `HOME` / `USERPROFILE` pointed at an empty
//! directory so that class of test fails where it is written.
//!
//! A gate that cannot fail manufactures confidence. These tests are the gate's
//! own red/green evidence: each one is a deliberately non-hermetic (or
//! deliberately un-catchable) test whose outcome under the isolated
//! environment is fixed and machine-independent.
//!
//! # Naming contract — `scripts/test-hermetic.py --selftest` enforces it
//!
//! * `caught_*`   — MUST FAIL under the isolated env. Each one depends on a
//!   real `HOME`; if it passes, `HOME` was not isolated and the gate is dead.
//! * `uncaught_*` — MUST PASS under the isolated env. These are the gate's
//!   documented blind spots, kept executable so the limits cannot rot into
//!   prose that stopped being true.
//!
//! A canary with any other prefix is a hard error in the self-test. So is a
//! canary that is not in the script's `REQUIRED_CANARIES` map, and so is a
//! pinned name that is no longer declared here — the set is compared for
//! EQUALITY, because a gate whose coverage can shrink unnoticed is the same
//! failure as a gate that cannot fail. Adding a canary means adding it there.
//!
//! # Every test here is `#[ignore]`d, on purpose
//!
//! `just test` and `just test-hermetic` both run without `--ignored`, so the
//! canaries never run as part of either gate — only
//! `just test-hermetic-selftest` runs them, one at a time, by exact name.
//! Keeping them (rather than deleting them after the red step) is what makes
//! the gate's failure mode a checked property on every CI run instead of a
//! one-off manual observation, which is the exact failure B045 was filed
//! about.
//!
//! Note that `caught_*` are expected to PASS under a normal `cargo test
//! --ignored` on a developer box with Openplanet installed, and
//! `uncaught_dependency_on_a_plugin_being_absent` is expected to FAIL there.
//! That asymmetry is the point; it is also why they are never part of a gate.
//!
//! # What HOME isolation does NOT cover
//!
//! See the `uncaught_*` tests below, and the header of
//! `scripts/test-hermetic.py` — which also lists the env vars the gate scrubs
//! alongside `HOME`, because `XDG_CONFIG_HOME` / `APPDATA` and the suite's
//! `OPENPLANET*` opt-ins (notably `OPENPLANET_REAL_PLUGINS_DIR`, passed
//! straight through as `--plugins-dir`) would otherwise hand a real install
//! back to a run whose `HOME` is a sandbox.

use std::path::PathBuf;

use openplanet_lsp::config::LspConfig;

/// `$HOME` as the process sees it. Under the gate this is an empty temp dir.
fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME is always set, real or isolated"))
}

// ---------------------------------------------------------------------------
// caught_* — the gate MUST turn these red.
// ---------------------------------------------------------------------------

/// The whole auto-detect ladder (`USERPROFILE` then `HOME`, per
/// `LspConfig::auto_detect_for_target`) in one assertion: a test that lets
/// config load itself and then assumes an install was found.
///
/// This is the shape of the bug B045 guards — the real
/// `check_command_applies_manifest_defines_and_dependency_defines` regression
/// asserted a `#if DEPENDENCY_EDITOR` branch was live because `Editor` happens
/// to be installed on one dev box.
#[test]
#[ignore = "B045 canary: depends on this machine's Openplanet install (see module docs)"]
fn caught_auto_detect_finds_a_local_openplanet_install() {
    let config = LspConfig::load(None, None);
    assert!(
        config.openplanet_dir.is_some(),
        "auto-detect found no Openplanet install under HOME={}",
        home().display()
    );
}

/// A test reading `~/OpenplanetNext/Plugins` directly — the plugins dir that
/// dependency resolution walks.
#[test]
#[ignore = "B045 canary: depends on this machine's Openplanet install (see module docs)"]
fn caught_home_plugins_dir_lists_installed_plugins() {
    let plugins = home().join("OpenplanetNext/Plugins");
    let count = std::fs::read_dir(&plugins)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", plugins.display()))
        .count();
    assert!(count > 0, "{} is empty", plugins.display());
}

/// The user global config surface, resolved through PRODUCTION code rather
/// than a hand-written path: `update::config_dir()` is the same ladder the
/// binary uses (`OPENPLANET_LSP_CONFIG_DIR` → `XDG_CONFIG_HOME` → `%APPDATA%`
/// → `~/.config` via `HOME` / `USERPROFILE`), and `LspConfig` reads
/// `~/.config/openplanet-lsp/config.toml` from the same place. Going through
/// it means this canary also fails if the gate stops scrubbing
/// `XDG_CONFIG_HOME` or `APPDATA`, which would route around the sandbox
/// without touching `HOME` at all.
///
/// The assertion is on the resolved dir's PARENT, not on `config.toml`: a
/// developer who has never written a user config still has `~/.config`, so
/// this stays a real red/green signal on a machine that has never run the
/// tool. Under the gate the parent is inside an empty temp dir and is absent.
#[test]
#[ignore = "B045 canary: depends on the real HOME's config dir (see module docs)"]
fn caught_user_config_dir_resolves_under_a_real_home() {
    let dir = openplanet_lsp::update::config_dir().expect("config dir resolves from HOME");
    let parent = dir.parent().expect("config dir always has a parent");
    assert!(
        parent.is_dir(),
        "{} does not exist — the user config layer cannot be reached",
        parent.display()
    );
}

/// The Windows-style branch of `auto_detect_for_target`, which probes
/// `%USERPROFILE%\OpenplanetNext` BEFORE `$HOME`.
///
/// On a POSIX dev box `USERPROFILE` is normally unset, so this passes
/// vacuously there. The gate SETS it (to the empty sandbox), which is exactly
/// what makes the Windows branch observable — and failing — off Windows.
#[test]
#[ignore = "B045 canary: depends on USERPROFILE resolving to a real install (see module docs)"]
fn caught_userprofile_points_at_an_openplanet_install() {
    let Ok(profile) = std::env::var("USERPROFILE") else {
        return; // unset: nothing to probe, which is the POSIX dev-box case
    };
    let dir = PathBuf::from(profile).join("OpenplanetNext");
    assert!(
        dir.is_dir(),
        "USERPROFILE is set but {} does not exist",
        dir.display()
    );
}

// ---------------------------------------------------------------------------
// uncaught_* — documented blind spots. The gate MUST leave these green.
// ---------------------------------------------------------------------------

/// **Limit: absolute paths outside HOME are invisible to the gate.**
///
/// HOME isolation is a runtime environment change; it cannot reach a path that
/// was never derived from `HOME`. A test with a hardcoded
/// `--plugins-dir /opt/openplanet/Plugins`, or one baked in at compile time
/// like `CARGO_MANIFEST_DIR` below, keeps resolving exactly as before.
///
/// What makes the gate sufficient TODAY is a separate property, not enforced
/// here: every `--plugins-dir` in the suite points inside a temp fixture dir.
#[test]
#[ignore = "B045 canary: documents a gate blind spot (see module docs)"]
fn uncaught_absolute_path_outside_home() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    assert!(
        repo.join("Cargo.toml").is_file(),
        "{} is not the repo root",
        repo.display()
    );
}

/// **Limit: the inverse case — depending on a plugin being ABSENT.**
///
/// This passes under the gate and in CI, and fails on any box that has
/// `Editor` installed. HOME isolation makes "no install" the tested
/// environment, so it can only ever catch tests that need something to be
/// there; a test that needs something to be missing is green here and red for
/// the developer, which is the failure mode inverted rather than removed.
#[test]
#[ignore = "B045 canary: documents a gate blind spot; FAILS on a box with Editor installed"]
fn uncaught_dependency_on_a_plugin_being_absent() {
    let editor = home().join("OpenplanetNext/Plugins/Editor");
    assert!(
        !editor.exists(),
        "{} exists — the gate cannot see this, but your machine can",
        editor.display()
    );
}
