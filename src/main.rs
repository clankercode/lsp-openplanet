use std::path::PathBuf;

use openplanet_lsp::{cli, entrypoint, server, update};

/// Issue tracker. Every help/version surface prints this so users report
/// Openplanet-behavior mismatches as issues instead of guessing
/// (agent-filed GH #45; see tests/cli_surface_tests.rs for the contract).
const ISSUE_URL: &str = "https://github.com/clankercode/lsp-openplanet/issues";

/// Trailer appended to `--version` output (after the machine-parsed first
/// line `openplanet-lsp <semver>` — the VS Code extension's parseVersion
/// reads whitespace-token 2, so trailing lines must not break it).
const VERSION_TRAILER: &str = "\
Repo:  https://github.com/clankercode/lsp-openplanet
Found a mismatch with how Openplanet behaves in-game? Please open an issue:
       https://github.com/clankercode/lsp-openplanet/issues
";

const HELP: &str = "\
openplanet-lsp - Language Server Protocol for OpenPlanet AngelScript

USAGE:
    openplanet-lsp [FLAGS]
    openplanet-lsp check [OPTIONS] [PATH]
    openplanet-lsp update [OPTIONS]
    openplanet-lsp lsp
    openplanet-lsp --lsp

FLAGS:
    -h, --help       Print this help and exit
    -V, --version    Print version and exit
    --lsp            Force language server on stdio (even on a TTY)

COMMANDS:
    check            Run workspace diagnostics (or --watch for live TUI)
                     Run `openplanet-lsp check --help` for options
    update           Check for / apply self-updates
                     Run `openplanet-lsp update --help` for options
    lsp              Same as --lsp (language server on stdio)

DEFAULT (no command):
    • non-TTY (editors)  → language server
    • TTY + plugin root  → watch TUI (config: default_mode = \"tui\"|\"lsp\")
    • TTY + no plugin    → short help (exit 2)

REPO & ISSUES:
    Repo:  https://github.com/clankercode/lsp-openplanet
    Found a mismatch with how Openplanet behaves in-game (a diagnostic the
    game would not raise, or missing one it would)? Please open an issue:
    https://github.com/clankercode/lsp-openplanet/issues

";

const UPDATE_HELP: &str = "\
openplanet-lsp update - Check for and apply self-updates

USAGE:
    openplanet-lsp update [OPTIONS]

OPTIONS:
    -h, --help       Show this help message
    --check          Query the version source for the latest version and write
                     the status file (do not install)
    --status         Print the last saved status without contacting the network
    --force          Re-run the install command even if already on the latest
                     reported version
    --source <SRC>   Where to check for the latest version:
                       npm     (default) registry.npmjs.org
                       crate   crates.io
                       github  latest GitHub Release tag
                     Aliases: crates, cargo, gh, git

BEHAVIOR:
    Latest version is read from the chosen source (default: npm). The running
    binary path is classified as npm/pnpm/yarn/bun (global or local), cargo,
    development, or standalone, and the matching upgrade path is used.

    Status lines look like:
      current:  0.2.9 (install type: standalone)
      latest:   0.2.9 (source checked: npm)

    Status is written to:
      $OPENPLANET_LSP_CONFIG_DIR/update-status.json
      (default: ~/.config/openplanet-lsp/update-status.json)

DEV / CI OVERRIDES (optional):
    OPENPLANET_LSP_VERSION          Pretend current version (update compare only;
                                    --version still prints the real binary version)
    OPENPLANET_LSP_LATEST_VERSION   Skip network; treat this as registry latest
    OPENPLANET_LSP_UPDATE_PACKAGE    install target(s) instead of
                                    openplanet-lsp@latest (whitespace-separated;
                                    local .tgz paths ok)
    OPENPLANET_LSP_PACKAGE_MANAGER  Force js pm: npm | pnpm | yarn | bun
    OPENPLANET_LSP_EXE              Pretend this is the running binary path
                                    (install-method detection)
    OPENPLANET_LSP_RELEASE_ARCHIVE  Local .tar.gz/.zip for standalone apply tests

EXAMPLES:
    openplanet-lsp update --check
    openplanet-lsp update --check --source github
    openplanet-lsp update --check --source crate
    openplanet-lsp update --status
    openplanet-lsp update

";

fn handle_early_args(args: &[String]) -> Option<i32> {
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("openplanet-lsp {}", env!("CARGO_PKG_VERSION"));
            print!("{VERSION_TRAILER}");
            Some(0)
        }
        Some("--help" | "-h") => {
            print!("{}", HELP);
            Some(0)
        }
        Some("--lsp") | Some("lsp") => {
            // Fall through to LSP after this function returns None... but we need
            // to skip other args handling. Signal with a dedicated path.
            Some(run_lsp_marker())
        }
        Some("check") => Some(run_check_command(&args[1..])),
        Some("update") => Some(run_update_command(&args[1..])),
        Some(arg) if arg.starts_with('-') => {
            eprintln!("unknown option: {arg}");
            eprintln!("Run `openplanet-lsp --help` for usage.");
            eprintln!("Behavior mismatch with Openplanet? {ISSUE_URL}");
            Some(2)
        }
        Some(arg) => {
            eprintln!("unknown command: {arg}");
            eprintln!("Run `openplanet-lsp --help` for usage.");
            eprintln!("Behavior mismatch with Openplanet? {ISSUE_URL}");
            Some(2)
        }
        None => None,
    }
}

/// Sentinel: handle_early_args cannot start async LSP; main treats 0x4C53_50 as "run LSP now".
const RUN_LSP_CODE: i32 = 0x4c_53_50; // 'LSP' bytes-ish

fn run_lsp_marker() -> i32 {
    RUN_LSP_CODE
}

fn run_check_command(args: &[String]) -> i32 {
    let options = match cli::parse_check_args(args) {
        Ok(options) => options,
        Err(err) => {
            eprintln!("{err}");
            eprintln!("Run `openplanet-lsp check --help` for usage.");
            return 2;
        }
    };

    if options.watch {
        return match cli::watch::run_watch(options) {
            Ok(()) => 0,
            Err(err) => {
                eprintln!("{err}");
                2
            }
        };
    }

    match cli::run_check(&options) {
        Ok(report) => {
            if !report.type_database_loaded && !options.no_typedb {
                eprintln!("warning: type database not loaded; pass --typedb-dir or --no-typedb");
            }
            print!("{}", cli::format_check_report_for(&report, options.format));
            let has_errors = report.diagnostics.iter().any(|item| {
                !matches!(
                    item.diagnostic.severity,
                    Some(tower_lsp::lsp_types::DiagnosticSeverity::WARNING)
                        | Some(tower_lsp::lsp_types::DiagnosticSeverity::INFORMATION)
                        | Some(tower_lsp::lsp_types::DiagnosticSeverity::HINT)
                )
            });
            if has_errors {
                1
            } else {
                0
            }
        }
        Err(err) => {
            eprintln!("{err}");
            2
        }
    }
}

fn parse_update_args(args: &[String]) -> Result<update::UpdateOptions, String> {
    let mut options = update::UpdateOptions::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print!("{}", UPDATE_HELP);
                std::process::exit(0);
            }
            "--check" => {
                options.check_only = true;
                i += 1;
            }
            "--status" => {
                options.status_only = true;
                i += 1;
            }
            "--force" => {
                options.force_install = true;
                i += 1;
            }
            "--source" => {
                let val = args.get(i + 1).ok_or_else(|| {
                    "--source requires a value (npm, crate, or github)".to_string()
                })?;
                options.version_source = update::VersionSource::parse(val)?;
                i += 2;
            }
            other if let Some(rest) = other.strip_prefix("--source=") => {
                options.version_source = update::VersionSource::parse(rest)?;
                i += 1;
            }
            other if other.starts_with('-') => {
                return Err(format!(
                    "unknown update option: {other}\nRun `openplanet-lsp update --help` for usage."
                ));
            }
            other => {
                return Err(format!(
                    "unexpected argument: {other}\nRun `openplanet-lsp update --help` for usage."
                ));
            }
        }
    }
    if options.check_only && options.status_only {
        return Err("--check and --status cannot be combined".into());
    }
    if options.force_install && options.status_only {
        return Err("--force and --status cannot be combined".into());
    }
    Ok(options)
}

fn run_update_command(args: &[String]) -> i32 {
    let options = match parse_update_args(args) {
        Ok(options) => options,
        Err(err) => {
            eprintln!("{err}");
            return 2;
        }
    };

    match update::run_update(&options) {
        Ok(report) => {
            print!("{}", report.text);
            report.exit_code
        }
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

fn bare_launch_exit() -> Option<i32> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let prefs = openplanet_lsp::config::UserPrefs::load(Some(&cwd));
    match entrypoint::decide_bare_launch(entrypoint::stdin_is_tty(), &cwd, &prefs) {
        entrypoint::LaunchAction::Lsp => None,
        entrypoint::LaunchAction::WatchTui { path } => Some(entrypoint::run_watch_path(path)),
        entrypoint::LaunchAction::HelpNoPlugin => {
            eprint!("{}", entrypoint::no_plugin_help_message());
            Some(2)
        }
    }
}

#[tokio::main]
async fn main() {
    // Initialize tracing FIRST so every subcommand (check, watch, update, LSP)
    // emits warnings. Previously this sat just before run_stdio, so the CLI
    // `check` path (which std::process::exit's from handle_early_args) never
    // initialized a subscriber and tracing::warn! from dep loading was dropped.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = handle_early_args(&args) {
        if code == RUN_LSP_CODE {
            // fall through to LSP
        } else {
            std::process::exit(code);
        }
    } else if args.is_empty() {
        if let Some(code) = bare_launch_exit() {
            std::process::exit(code);
        }
    }

    server::run_stdio().await;
}
