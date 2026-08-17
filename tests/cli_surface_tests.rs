//! CLI surface tests: `--version`, `--help`, unknown-command errors, and the
//! bare-launch help must advertise the repository URL and ask users to file
//! issues for Openplanet-behavior mismatches (agent-filed GH #45).

use std::process::Command;

/// Trailer every help/version surface must print.
const ISSUE_ASK: &str = "https://github.com/clankercode/lsp-openplanet/issues";

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
}

#[test]
fn tui_subcommand_launches_watch_tui_not_unknown_command() {
    // `openplanet-lsp tui` must route to the watch TUI (same as `check --watch`),
    // not die as an unknown command. The test cwd (repo root) has no info.toml
    // and stdin is piped, so the TUI must NOT start: either outcome below
    // proves the `tui` subcommand was recognized.
    let out = bin().arg("tui").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("unknown command"),
        "`tui` must be a recognized subcommand: got {stderr:?}"
    );
    assert!(
        stderr.contains("No OpenPlanet plugin found") || stderr.contains("watch TUI failed"),
        "expected no-plugin help or a TUI terminal error: status {:?}, stderr {stderr:?}",
        out.status.code()
    );
}

#[test]
fn version_output_includes_repo_url_and_issue_ask() {
    let out = bin().arg("--version").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("openplanet-lsp "),
        "--version must keep the `openplanet-lsp <semver>` first line (the VS Code\n\
         extension's parseVersion takes whitespace-token 2): got {stdout:?}"
    );
    assert!(
        stdout.contains(ISSUE_ASK),
        "--version must point at the issue tracker for behavior mismatches: got {stdout:?}"
    );
}

#[test]
fn version_short_flag_matches_long_flag() {
    let long = bin().arg("--version").output().unwrap();
    let short = bin().arg("-V").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&long.stdout),
        String::from_utf8_lossy(&short.stdout),
        "-V and --version must print identical output"
    );
}

#[test]
fn help_output_includes_repo_url_and_issue_ask() {
    let out = bin().arg("--help").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("USAGE"),
        "--help must print the usage block"
    );
    assert!(
        stdout.contains(ISSUE_ASK),
        "--help must point at the issue tracker: got {stdout:?}"
    );
    let short = bin().arg("-h").output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&short.stdout),
        stdout,
        "-h and --help must print identical output"
    );
}

#[test]
fn help_subcommand_matches_long_flag() {
    // `openplanet-lsp help` is an alias for `openplanet-lsp --help`.
    let long = bin().arg("--help").output().unwrap();
    let sub = bin().arg("help").output().unwrap();
    assert!(sub.status.success(), "`help` must exit 0: {sub:?}");
    assert_eq!(
        String::from_utf8_lossy(&sub.stdout),
        String::from_utf8_lossy(&long.stdout),
        "`help` and `--help` must print identical output"
    );
}

#[test]
fn unknown_command_error_points_at_help_and_repo() {
    let out = bin().arg("definitely-not-a-command").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown command"),
        "error must name the problem: got {stderr:?}"
    );
    assert!(
        stderr.contains(ISSUE_ASK),
        "unknown-command error should point at the issue tracker: got {stderr:?}"
    );
}

#[test]
fn bare_tty_help_includes_repo_url_and_issue_ask() {
    // no_plugin_help_message() backs the TTY + no-plugin bare launch path.
    // Unit-level check (no TTY needed).
    let msg = openplanet_lsp::entrypoint::no_plugin_help_message();
    assert!(
        msg.contains(ISSUE_ASK),
        "bare-launch help must point at the issue tracker: got {msg:?}"
    );
}
