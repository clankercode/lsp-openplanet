//! B039: end-to-end proof that every command the server ADVERTISES can
//! actually be executed.
//!
//! The failure this file exists to catch is not "the handler is wrong" — it
//! is "the handler never sees the command". `ExecuteCommandOptions.commands`
//! is a PROMISE to every client: VS Code puts those names in front of users,
//! and a name in that list that falls through to a default arm is
//! indistinguishable, from the client's side, from one that works and does
//! nothing.
//!
//! So nothing here calls a handler directly. Every test boots the real
//! service through [`openplanet_lsp::server::build_service`] — the one
//! constructor `run_stdio` uses — reads the advertised list out of the
//! server's OWN `initialize` response, and dispatches raw JSON-RPC requests
//! exactly as a client would. A command implemented on `Backend` but not
//! reachable through the router fails every test here.
//!
//! The gate in [`every_advertised_command_is_dispatched`] is deliberately
//! generic over that list: adding a name to the capability without a handler
//! goes red, whatever the name is. Its discriminator — "unknown command is a
//! JSON-RPC error" — is itself pinned by
//! [`an_unknown_command_is_a_json_rpc_error_not_a_silent_null`], so the gate
//! cannot quietly become vacuous by the server learning to answer everything
//! with `null`.
//!
//! Invariant this places on future commands: an advertised command must be
//! SAFE to invoke against a freshly initialized server with no type database
//! configured, because that is what the gate does to each of them. A command
//! that cannot meet that should not be advertised unconditionally.

use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use serde_json::{json, Value};
use tower::{Service, ServiceExt};
use tower_lsp::jsonrpc::{Error as RpcError, Request};
use tower_lsp::lsp_types::{InitializeParams, InitializeResult, Url, WorkspaceFolder};
use tower_lsp::LspService;

use openplanet_lsp::server::{build_service, Backend};

const METHOD: &str = "workspace/executeCommand";

/// The fixture directory for one test, removed when the test ends.
///
/// The `Drop` is the point. Keying the directory on the pid and clearing it
/// only at START — which is what this did first — leaves a full set behind on
/// every single run: 135 stray `/tmp/oplsp-b039-*` directories had piled up on
/// one machine before anyone looked. On a shared /tmp that a whole batch of
/// lanes builds into, a suite that litters per invocation is not merely untidy.
///
/// Dropping also runs while a failing test unwinds, so a red run cleans up
/// too. Nothing here needs post-mortem inspection of the tree: every assertion
/// carries the server's own payload in its message.
struct Fixture {
    base: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// A plugin root, plus the `OpenplanetNext` dir its config points at.
///
/// Every fixture pins BOTH `openplanet_dir` — an EMPTY one when `with_typedb`
/// is false — and `game_target`. Left unset, `LspConfig` auto-detects
/// `$HOME/OpenplanetNext`, so on a developer machine with the game installed
/// these tests would silently load that person's real type database: slow, and
/// a different answer per machine.
///
/// `game_target` has to be pinned too, and the reason is B066 rather than
/// tidiness. The directory is named `OpenplanetNext` so the variant re-probe
/// (GH #45) sees a matching folder and returns before touching `$HOME` — but
/// that match is against the RESOLVED target, and the user-global
/// `~/.config/openplanet-lsp/config.toml` gets a say in that. A developer
/// whose global config sets `game_target = "MP4"` makes the re-probe look for
/// `OpenplanetMP4`, miss, and reach for `$HOME/OpenplanetMP4` — and B066 is
/// that the guard meant to protect an explicit `openplanet_dir` there is
/// always true, so the pin above is silently replaced. Observed, not
/// theorised: with a synthetic HOME carrying that config, the EMPTY-fixture
/// test loaded a full 4276-symbol database. Pinning the target keeps the
/// folder name matching so the re-probe returns early. When B066 is fixed
/// this pin should stop being load-bearing; do not remove it before then.
///
/// Worth knowing before trusting any assertion here to prove WHICH database
/// loaded: it cannot. The fixtures are refreshed from a real install
/// (`just typedb-refresh`), so the two agree on every stamp AND on the symbol
/// count — measured, 4276 either way. Nothing in the payload distinguishes
/// them. The pin above is what keeps the real install out of these tests, and
/// `just test-hermetic` is what proves the pin is working; no in-test
/// assertion can substitute for it.
fn fixture(name: &str, with_typedb: bool) -> Fixture {
    let base = std::env::temp_dir().join(format!("oplsp-b039-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("plugin");
    let op_dir = base.join("OpenplanetNext");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(&op_dir).unwrap();
    if with_typedb {
        // `Openplanet.h` is NOT optional decoration. `TypeIndex::load` looks
        // for a header beside the game database and folds it in, so a copy of
        // just the two .json files is a DIFFERENT database — 4274 symbols
        // against 4276 — and silently so. Copy the fixture whole.
        for file in ["OpenplanetCore.json", "OpenplanetNext.json", "Openplanet.h"] {
            std::fs::copy(typedb_fixture_dir().join(file), op_dir.join(file)).unwrap();
        }
    }
    std::fs::write(
        root.join("info.toml"),
        format!("[meta]\nname = \"B039 {name}\"\nversion = \"0.1.0\"\n"),
    )
    .unwrap();
    std::fs::write(root.join("src/Main.as"), "void Main() {}\n").unwrap();
    std::fs::write(
        root.join(".openplanet-lsp.toml"),
        format!(
            "openplanet_dir = {:?}\ngame_target = \"TMNEXT\"\n",
            op_dir.to_str().unwrap()
        ),
    )
    .unwrap();
    Fixture { base, root }
}

fn typedb_fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb")
}

/// How many symbols the SHIPPED fixture database holds, computed by loading
/// it here rather than hardcoded.
///
/// This does NOT prove the server avoided a real install — the fixture is a
/// refresh of one and matches it symbol for symbol. What it does prove is
/// that the server loaded the fixture WHOLE, which is not free: the first
/// version of this fixture copied the two .json files and not the
/// `Openplanet.h` beside them, and the server then loaded a quietly smaller
/// database that a `> 0` assertion was perfectly happy with. Derived rather
/// than pinned to a literal so `just typedb-refresh-apply` cannot red the
/// suite for doing its job.
fn fixture_symbol_count() -> usize {
    let dir = typedb_fixture_dir();
    let index = openplanet_lsp::typedb::TypeIndex::load(
        &dir.join("OpenplanetCore.json"),
        &dir.join("OpenplanetNext.json"),
    )
    .expect("the shipped fixture type db loads");
    index.type_count() + index.function_count() + index.enum_count()
}

fn init_params(root: &Path) -> InitializeParams {
    let uri = Url::from_directory_path(root).unwrap();
    #[allow(deprecated)]
    InitializeParams {
        root_uri: Some(uri.clone()),
        workspace_folders: Some(vec![WorkspaceFolder {
            uri,
            name: "b039".to_string(),
        }]),
        ..Default::default()
    }
}

async fn call(service: &mut LspService<Backend>, request: Request) -> Result<Value, RpcError> {
    let response = service
        .ready()
        .await
        .expect("service ready")
        .call(request)
        .await
        .expect("no transport error")
        .expect("a request gets a response");
    let (_id, result) = response.into_parts();
    result
}

/// Fire a notification (no response expected).
async fn notify(service: &mut LspService<Backend>, request: Request) {
    let response = service
        .ready()
        .await
        .expect("service ready")
        .call(request)
        .await
        .expect("no transport error");
    assert!(response.is_none(), "a notification gets no response");
}

/// Boot a real service on `root` and return it together with the command
/// names its `initialize` response advertised.
async fn boot(root: &Path) -> (LspService<Backend>, Vec<String>) {
    let (mut service, socket) = build_service();
    // Drain server->client traffic (`publishDiagnostics`, `showMessage`) so
    // nothing here blocks on a client that never reads.
    tokio::spawn(async move {
        let mut socket = socket;
        while socket.next().await.is_some() {}
    });
    let initialized = call(
        &mut service,
        Request::build("initialize")
            .params(serde_json::to_value(init_params(root)).unwrap())
            .id(1)
            .finish(),
    )
    .await
    .expect("initialize succeeds");
    let result: InitializeResult =
        serde_json::from_value(initialized).expect("a well-formed InitializeResult");
    let commands = result
        .capabilities
        .execute_command_provider
        .expect("the server advertises an execute-command provider")
        .commands;
    (service, commands)
}

/// Send one `workspace/executeCommand`, exactly as a client would.
async fn execute(service: &mut LspService<Backend>, command: &str) -> Result<Value, RpcError> {
    call(
        service,
        Request::build(METHOD)
            .params(json!({ "command": command, "arguments": [] }))
            .id(99)
            .finish(),
    )
    .await
}

/// THE gate. Generic over the advertised list on purpose: this must fail for
/// any future advertised-but-undispatched command, not just today's.
#[tokio::test]
async fn every_advertised_command_is_dispatched() {
    let fixture = fixture("gate", false);
    let root = fixture.root();
    let (mut service, commands) = boot(root).await;

    assert!(
        !commands.is_empty(),
        "the server advertises no commands at all — this gate would be vacuous"
    );

    for command in &commands {
        let outcome = execute(&mut service, command).await;
        assert!(
            outcome.is_ok(),
            "advertised command `{command}` is not dispatched by the executeCommand \
             handler: it answered {outcome:?}. Either implement it or stop advertising it."
        );
        assert!(
            !outcome.unwrap().is_null(),
            "advertised command `{command}` answered null — a client cannot tell that \
             apart from a command that did nothing"
        );
    }
}

/// The discriminator the gate above relies on. Without this the gate would
/// pass on a server that answered `Ok(null)` to literally every name.
#[tokio::test]
async fn an_unknown_command_is_a_json_rpc_error_not_a_silent_null() {
    let fixture = fixture("unknown", false);
    let root = fixture.root();
    let (mut service, _commands) = boot(root).await;

    let outcome = execute(&mut service, "openplanet.thisCommandDoesNotExist").await;
    let error = outcome.expect_err(
        "an unadvertised command must be a JSON-RPC error; answering it successfully \
         makes the advertised-command gate meaningless",
    );
    // The CODE, not just "some error". A client distinguishes a bad request
    // from a dead method by the code, and the gate above discriminates
    // dispatched-from-not on exactly this answer. Asserted through the router
    // rather than only on `commands::unknown_command_error` in isolation,
    // because a handler could construct the right error and the router still
    // rewrite it.
    assert_eq!(
        error.code,
        tower_lsp::jsonrpc::ErrorCode::InvalidParams,
        "unknown command should be InvalidParams — the method exists, the \
         `command` parameter names nothing: {error:?}"
    );
    assert!(
        error.message.contains("openplanet.thisCommandDoesNotExist"),
        "the error should name the command the client asked for: {error:?}"
    );
}

/// A bad command name must not take the server down with it.
#[tokio::test]
async fn an_unknown_command_does_not_kill_the_server() {
    let fixture = fixture("survives", false);
    let root = fixture.root();
    let (mut service, commands) = boot(root).await;

    let _ = execute(&mut service, "openplanet.nonsense").await;

    // The service still answers, and still answers its real command.
    let command = commands.first().expect("at least one advertised command");
    assert!(
        execute(&mut service, command).await.is_ok(),
        "the server stopped serving after an unknown command"
    );
}

/// Pins the advertised surface. A command added here without a client route
/// is the B039 defect happening again.
#[tokio::test]
async fn the_advertised_surface_is_exactly_the_documented_one() {
    let fixture = fixture("surface", false);
    let root = fixture.root();
    let (_service, commands) = boot(root).await;
    assert_eq!(commands, vec!["openplanet.regenerateTypeDb".to_string()]);
}

/// The command's own contract with the extension: it reports WHAT CHANGED,
/// not merely that it ran. `before` is the DB the server had loaded; `after`
/// is what is on disk now — which is the whole question a user asks after
/// updating Openplanet.
#[tokio::test]
async fn regenerate_type_db_reports_the_versions_before_and_after() {
    let fixture = fixture("versions", true);
    let root = fixture.root();
    let (mut service, _commands) = boot(root).await;

    let payload = execute(&mut service, "openplanet.regenerateTypeDb")
        .await
        .expect("the command is dispatched");

    assert_eq!(
        payload["ok"],
        json!(true),
        "regeneration failed against the fixture type db: {payload}"
    );
    // Pins that the WHOLE fixture loaded, header included. Not which
    // database: the fixture and a real install agree on the count as well as
    // the stamps. See `fixture_symbol_count`.
    assert_eq!(
        payload["regenerated"].as_u64().unwrap_or(0),
        fixture_symbol_count() as u64,
        "the server loaded a DIFFERENT database than tests/fixtures/typedb: {payload}"
    );

    // The fixture stamps, straight out of tests/fixtures/typedb.
    for side in ["before", "after"] {
        assert_eq!(
            payload[side]["op"],
            json!("1.29.5 (next, Public, 1234ad9e)"),
            "{side}.op is wrong: {payload}"
        );
        assert_eq!(
            payload[side]["mp"],
            json!("2026-02-03 03:51:19"),
            "{side}.mp is wrong: {payload}"
        );
        assert_eq!(
            payload[side]["game_op"],
            json!("1.29.5"),
            "{side}.game_op is wrong: {payload}"
        );
    }
    assert_eq!(
        payload["changed"],
        json!(false),
        "nothing on disk moved, so the versions did not change: {payload}"
    );
}

/// The unconfigured case is an ANSWER, not a crash and not a silent success:
/// a user who never pointed the server at Openplanet must be told that, not
/// shown "regenerated".
#[tokio::test]
async fn regenerate_type_db_without_configured_paths_reports_why() {
    let fixture = fixture("unconfigured", false);
    let root = fixture.root();
    let (mut service, _commands) = boot(root).await;

    let payload = execute(&mut service, "openplanet.regenerateTypeDb")
        .await
        .expect("the command is dispatched even with nothing configured");
    assert_eq!(payload["ok"], json!(false), "{payload}");
    assert!(
        payload["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not configured"),
        "the message should say what is missing: {payload}"
    );
}

/// The audit in the other direction, on the server's own output.
///
/// Turning an unknown command into an ERROR is only safe while the server
/// never hands a client a `Command` to send back. A code action or code lens
/// carrying a `command` field is exactly that: the client returns it through
/// `workspace/executeCommand`, and an unadvertised name would now be a
/// visible error rather than a silent no-op. Every code action this server
/// produces today is a `CodeAction` with `command: None`; this pins that.
#[tokio::test]
async fn no_code_action_hands_the_client_a_command_to_send_back() {
    let fixture = fixture("codeactions", false);
    let root = fixture.root();
    let (mut service, _commands) = boot(root).await;

    // An unused local: a diagnostic this server offers quick fixes for.
    let file = root.join("src/Main.as");
    let source = "void Main() {\n    int unusedLocal = 1;\n}\n";
    std::fs::write(&file, source).unwrap();
    let uri = Url::from_file_path(&file).unwrap();
    notify(
        &mut service,
        Request::build("textDocument/didOpen")
            .params(json!({
                "textDocument": {
                    "uri": uri, "languageId": "angelscript", "version": 1, "text": source
                }
            }))
            .finish(),
    )
    .await;

    let actions = call(
        &mut service,
        Request::build("textDocument/codeAction")
            .params(json!({
                "textDocument": { "uri": uri },
                "range": {
                    "start": { "line": 1, "character": 8 },
                    "end": { "line": 1, "character": 19 }
                },
                "context": { "diagnostics": [] }
            }))
            .id(7)
            .finish(),
    )
    .await
    .expect("codeAction succeeds");

    let items = actions.as_array().cloned().unwrap_or_default();
    assert!(
        !items.is_empty(),
        "no code actions were offered, so this proves nothing: {actions}"
    );
    for item in items {
        assert!(
            item.get("command").is_none() || item["command"].is_null(),
            "a code action carries a command the client will send back through \
             workspace/executeCommand; it must be advertised and dispatched: {item}"
        );
    }
}
