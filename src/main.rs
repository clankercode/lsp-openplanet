use openplanet_lsp::server;

const HELP: &str = "\
openplanet-lsp - Language Server Protocol for OpenPlanet AngelScript

USAGE:
    openplanet-lsp [FLAGS]

FLAGS:
    -h, --help       Print this help and exit
    -V, --version    Print version and exit

With no flags, runs as a stdio LSP server (JSON-RPC over stdin/stdout).
";

fn handle_early_args() -> Option<i32> {
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("openplanet-lsp {}", env!("CARGO_PKG_VERSION"));
                return Some(0);
            }
            "--help" | "-h" => {
                print!("{}", HELP);
                return Some(0);
            }
            _ => {}
        }
    }
    None
}

#[tokio::main]
async fn main() {
    if let Some(code) = handle_early_args() {
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
