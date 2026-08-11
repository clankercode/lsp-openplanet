use openplanet_lsp::{cli, server, update};

const HELP: &str = "\
openplanet-lsp - Language Server Protocol for OpenPlanet AngelScript

USAGE:
    openplanet-lsp [FLAGS]
    openplanet-lsp check [OPTIONS] <PATH>
    openplanet-lsp update [OPTIONS]

FLAGS:
    -h, --help       Print this help and exit
    -V, --version    Print version and exit

COMMANDS:
    check            Run workspace diagnostics for an OpenPlanet plugin
                     Run `openplanet-lsp check --help` for check-specific options
    update           Check for / apply self-updates via the detected install method
                     Run `openplanet-lsp update --help` for update-specific options

With no flags, runs as a stdio LSP server (JSON-RPC over stdin/stdout).
";

const UPDATE_HELP: &str = "\
openplanet-lsp update - Check for and apply self-updates

USAGE:
    openplanet-lsp update [OPTIONS]

OPTIONS:
    -h, --help       Show this help message
    --check          Query npm for the latest version and write the status file
                     (do not install)
    --status         Print the last saved status without contacting the network
    --force          Re-run the install command even if already on the latest
                     reported version

BEHAVIOR:
    Latest version is read from the npm registry (registry.npmjs.org), not the
    GitHub API. The running binary path is classified as npm-global, npm-local,
    cargo, development, or standalone, and the matching upgrade command is used.

    Status is written to:
      $OPENPLANET_LSP_CONFIG_DIR/update-status.json
      (default: ~/.config/openplanet-lsp/update-status.json)

DEV / CI OVERRIDES (optional):
    OPENPLANET_LSP_VERSION          Pretend current version (update compare only;
                                    --version still prints the real binary version)
    OPENPLANET_LSP_LATEST_VERSION   Skip network; treat this as registry latest
    OPENPLANET_LSP_UPDATE_PACKAGE    npm install target(s) instead of
                                    openplanet-lsp@latest (whitespace-separated;
                                    local .tgz paths ok)
    OPENPLANET_LSP_EXE              Pretend this is the running binary path
                                    (install-method detection)

EXAMPLES:
    openplanet-lsp update --check
    openplanet-lsp update --status
    openplanet-lsp update
";

fn handle_early_args(args: &[String]) -> Option<i32> {
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("openplanet-lsp {}", env!("CARGO_PKG_VERSION"));
            Some(0)
        }
        Some("--help" | "-h") => {
            print!("{}", HELP);
            Some(0)
        }
        Some("check") => Some(run_check_command(&args[1..])),
        Some("update") => Some(run_update_command(&args[1..])),
        Some(arg) if arg.starts_with('-') => {
            eprintln!("unknown option: {arg}");
            eprintln!("Run `openplanet-lsp --help` for usage.");
            Some(2)
        }
        Some(arg) => {
            eprintln!("unknown command: {arg}");
            eprintln!("Run `openplanet-lsp --help` for usage.");
            Some(2)
        }
        None => None,
    }
}

fn run_check_command(args: &[String]) -> i32 {
    let options = match cli::parse_check_args(args) {
        Ok(options) => options,
        Err(err) => {
            eprintln!("{err}");
            eprintln!("Run `openplanet-lsp --help` for usage.");
            return 2;
        }
    };

    match cli::run_check(&options) {
        Ok(report) => {
            if !report.type_database_loaded && !options.no_typedb {
                eprintln!("warning: type database not loaded; pass --typedb-dir or --no-typedb");
            }
            print!("{}", cli::format_check_report(&report));
            // Warnings (e.g. B004 bare-string params) are reported but do not
            // fail the check command; only errors produce a non-zero exit.
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

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = handle_early_args(&args) {
        std::process::exit(code);
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    server::run_stdio().await;
}
