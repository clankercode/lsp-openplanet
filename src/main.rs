use openplanet_lsp::{cli, server};

const HELP: &str = "\
openplanet-lsp - Language Server Protocol for OpenPlanet AngelScript

USAGE:
    openplanet-lsp [FLAGS]
    openplanet-lsp check [OPTIONS] <PATH>

FLAGS:
    -h, --help       Print this help and exit
    -V, --version    Print version and exit

COMMANDS:
    check            Run workspace diagnostics for an OpenPlanet plugin
                     Run `openplanet-lsp check --help` for check-specific options

With no flags, runs as a stdio LSP server (JSON-RPC over stdin/stdout).
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
