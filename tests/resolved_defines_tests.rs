//! B050: end-to-end proof that `openplanet/resolvedDefines` is REACHABLE.
//!
//! Deliberately not a unit test of the handler. The failure mode this file
//! exists for is the one B040 was filed for: a handler that is correct,
//! tested, and never wired into the router the real server serves. So every
//! test here goes through [`openplanet_lsp::server::build_service`] — the one
//! constructor `run_stdio` uses — and dispatches a raw JSON-RPC
//! [`Request`], exactly as a client would. A handler reachable only through
//! `service.inner()` would pass a direct-call test and fail every test here.
//!
//! The payload is checked as JSON, not as the Rust struct, because the wire
//! shape is the contract with `packages/vscode-ext/src/resolvedDefines.ts`:
//! a renamed serde field would be invisible to a typed assertion.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tower::{Service, ServiceExt};
use tower_lsp::jsonrpc::Request;
use tower_lsp::lsp_types::{InitializeParams, Url, WorkspaceFolder};
use tower_lsp::LspService;

use openplanet_lsp::server::{build_service, Backend};

const METHOD: &str = "openplanet/resolvedDefines";

/// A plugin root with `info.toml` + `.openplanet-lsp.toml`, and an adjacent
/// plugins dir the config points at. Mirrors the B019 config-level fixture so
/// the two layers are exercised by the same tree shape.
fn fixture(name: &str, script: &str) -> (PathBuf, PathBuf) {
    let tmp = std::env::temp_dir().join(format!("oplsp-b050-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let root = tmp.join("plugin");
    let plugins = tmp.join("plugins");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::write(
        root.join("info.toml"),
        format!("[meta]\nname = \"B050 {name}\"\nversion = \"0.1.0\"\n\n[script]\n{script}"),
    )
    .unwrap();
    std::fs::write(root.join("src/Main.as"), "void Main() {}\n").unwrap();
    std::fs::write(
        root.join(".openplanet-lsp.toml"),
        format!("plugins_dir = {:?}\n", plugins.to_str().unwrap()),
    )
    .unwrap();
    (root, plugins)
}

/// Install a resolvable directory plugin, so its `DEPENDENCY_*` is defined.
fn install(plugins: &Path, id: &str) {
    let dir = plugins.join(id);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("info.toml"),
        format!("[meta]\nname = \"{id}\"\nversion = \"1.0.0\"\n"),
    )
    .unwrap();
}

fn init_params(root: &Path) -> InitializeParams {
    let uri = Url::from_directory_path(root).unwrap();
    #[allow(deprecated)]
    InitializeParams {
        root_uri: Some(uri.clone()),
        workspace_folders: Some(vec![WorkspaceFolder {
            uri,
            name: "b050".to_string(),
        }]),
        ..Default::default()
    }
}

async fn call(service: &mut LspService<Backend>, request: Request) -> Value {
    let response = service
        .ready()
        .await
        .expect("service ready")
        .call(request)
        .await
        .expect("no transport error")
        .expect("a request gets a response");
    let (_id, result) = response.into_parts();
    result.unwrap_or_else(|err| panic!("{METHOD} returned a JSON-RPC error: {err:?}"))
}

/// Boot a real service on `root` and ask it for the resolved define set.
async fn resolved_defines_for(root: &Path, params: Value) -> Value {
    let (mut service, _socket) = build_service();
    let init = Request::build("initialize")
        .params(serde_json::to_value(init_params(root)).unwrap())
        .id(1)
        .finish();
    let _ = call(&mut service, init).await;
    let request = Request::build(METHOD).params(params).id(2).finish();
    call(&mut service, request).await
}

/// One row of the response by name.
fn row<'a>(result: &'a Value, name: &str) -> &'a Value {
    result["defines"]
        .as_array()
        .unwrap_or_else(|| panic!("`defines` must be an array: {result}"))
        .iter()
        .find(|d| d["name"] == name)
        .unwrap_or_else(|| panic!("`{name}` missing from the response: {result:#}"))
}

fn has_row(result: &Value, name: &str) -> bool {
    result["defines"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|d| d["name"] == name))
}

/// The ticket's headline case: a manifest-derived `DEPENDENCY_*` reaches the
/// client, with the right origin, over the real router.
#[tokio::test]
async fn resolved_defines_reports_manifest_dependency_origins_over_the_wire() {
    let (root, plugins) = fixture(
        "deps",
        "defines = [\"FROM_MANIFEST\"]\n\
         optional_dependencies = [\"ZzzE2ePresent\", \"ZzzE2eAbsent99\"]\n",
    );
    install(&plugins, "ZzzE2ePresent");

    let result = resolved_defines_for(&root, serde_json::json!({})).await;

    let present = row(&result, "DEPENDENCY_ZZZE2EPRESENT");
    assert_eq!(present["origin"], "dependency", "{present}");
    assert_eq!(present["active"], true, "{present}");
    assert_eq!(present["derivedActive"], true, "{present}");

    let absent = row(&result, "DEPENDENCY_ZZZE2EABSENT99");
    assert_eq!(
        absent["origin"], "dependency-missing",
        "a declared-but-uninstalled optional dependency must be REPORTED, \
         not omitted — that row is what explains a dead `#if` branch: {absent}"
    );
    assert_eq!(absent["active"], false, "{absent}");
    assert_eq!(absent["derivedActive"], false, "{absent}");

    let manifest = row(&result, "FROM_MANIFEST");
    assert_eq!(manifest["origin"], "manifest", "{manifest}");
    assert_eq!(manifest["active"], true, "{manifest}");

    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

/// The derived base set the client must never re-derive itself: every origin
/// class arrives named.
#[tokio::test]
async fn resolved_defines_reports_the_whole_derived_base_set() {
    let (root, _plugins) = fixture("base", "");
    let result = resolved_defines_for(&root, serde_json::json!({})).await;

    for (name, origin) in [
        ("TMNEXT", "target"),
        ("SIG_DEVELOPER", "signature"),
        ("WINDOWS", "os"),
        ("MANIA64", "arch"),
        ("DEVELOPER", "build"),
    ] {
        let found = row(&result, name);
        assert_eq!(found["origin"], origin, "{found}");
        assert_eq!(found["active"], true, "{found}");
    }
    assert_eq!(result["gameTarget"], "TMNEXT", "{result:#}");

    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

/// Remove and disable are different states (B034). A `-NAME` on a derived
/// define arrives present-but-off, with its derived origin intact.
#[tokio::test]
async fn resolved_defines_reports_a_user_disabled_derived_define_as_present() {
    let (root, plugins) = fixture("disabled", "optional_dependencies = [\"ZzzE2eOff\"]\n");
    install(&plugins, "ZzzE2eOff");
    std::fs::write(
        root.join(".openplanet-lsp.toml"),
        format!(
            "plugins_dir = {:?}\ndefines = [\"-LOGS\", \"-DEPENDENCY_ZZZE2EOFF\"]\n",
            plugins.to_str().unwrap()
        ),
    )
    .unwrap();

    let result = resolved_defines_for(&root, serde_json::json!({})).await;

    let logs = row(&result, "LOGS");
    assert_eq!(logs["origin"], "build", "the derived origin must survive");
    assert_eq!(logs["active"], false, "{logs}");
    assert_eq!(logs["derivedActive"], true, "{logs}");

    let dep = row(&result, "DEPENDENCY_ZZZE2EOFF");
    assert_eq!(dep["origin"], "dependency", "{dep}");
    assert_eq!(dep["active"], false, "{dep}");
    assert_eq!(dep["derivedActive"], true, "{dep}");

    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

/// The platform-family table travels with the answer, so the extension can
/// ask instead of duplicating `LspConfig::PLATFORM_FAMILIES`.
#[tokio::test]
async fn resolved_defines_carries_the_platform_family_table() {
    let (root, _plugins) = fixture("families", "");
    let result = resolved_defines_for(&root, serde_json::json!({})).await;

    let families = result["platformFamilies"]
        .as_array()
        .unwrap_or_else(|| panic!("`platformFamilies` must be an array: {result:#}"));
    let mp4 = families
        .iter()
        .find(|f| f.as_array().is_some_and(|f| f.iter().any(|n| n == "MP4")))
        .unwrap_or_else(|| panic!("no MP4 family: {result:#}"));
    for member in ["MP4", "MP40", "MP41"] {
        assert!(
            mp4.as_array().unwrap().iter().any(|n| n == member),
            "`{member}` must share MP4's family, or the client's same-family \
             check regresses to a flat membership test: {mp4}"
        );
    }
    assert!(
        families.iter().any(|f| f
            .as_array()
            .is_some_and(|f| f.iter().any(|n| n == "TMNEXT"))),
        "{result:#}"
    );

    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}

/// A `uri` inside one shard answers for THAT shard's config, not the
/// server-wide one. Two roots with different game targets in one window is
/// exactly the multi-root case the flat `config` field cannot serve.
#[tokio::test]
async fn resolved_defines_routes_a_uri_to_its_own_plugin_workspace() {
    let tmp = std::env::temp_dir().join(format!("oplsp-b050-e2e-multi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    for (name, target) in [("alpha", "TMNEXT"), ("beta", "TURBO")] {
        let root = tmp.join(name);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("info.toml"),
            format!("[meta]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
        )
        .unwrap();
        std::fs::write(root.join("src/Main.as"), "void Main() {}\n").unwrap();
        std::fs::write(
            root.join(".openplanet-lsp.toml"),
            format!("game_target = \"{target}\"\n"),
        )
        .unwrap();
    }

    let (mut service, _socket) = build_service();
    let init = Request::build("initialize")
        .params(serde_json::to_value(init_params(&tmp)).unwrap())
        .id(1)
        .finish();
    let _ = call(&mut service, init).await;

    for (name, live, dead) in [("alpha", "TMNEXT", "TURBO"), ("beta", "TURBO", "TMNEXT")] {
        let uri = Url::from_directory_path(tmp.join(name).join("src")).unwrap();
        let request = Request::build(METHOD)
            .params(serde_json::json!({ "uri": uri.join("Main.as").unwrap() }))
            .id(2)
            .finish();
        let result = call(&mut service, request).await;
        assert_eq!(result["gameTarget"], live, "{name}: {result:#}");
        assert_eq!(row(&result, live)["origin"], "target", "{name}");
        assert!(
            !has_row(&result, dead),
            "{name}: the other shard's selector leaked in: {result:#}"
        );
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Dispatch through the real router but keep a JSON-RPC error intact, for the
/// tests that are specifically asserting a refusal.
async fn call_result(
    service: &mut LspService<Backend>,
    request: Request,
) -> tower_lsp::jsonrpc::Result<Value> {
    let response = service
        .ready()
        .await
        .expect("service ready")
        .call(request)
        .await
        .expect("no transport error")
        .expect("a request gets a response");
    response.into_parts().1
}

/// A window serving `roots` plugin dirs under one opened folder, each with its
/// own `game_target`. Returns the opened folder.
fn multi_root_window(name: &str, roots: &[(&str, &str)]) -> PathBuf {
    let tmp = std::env::temp_dir().join(format!("oplsp-b050-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    for (dir, target) in roots {
        let root = tmp.join(dir);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("info.toml"),
            format!("[meta]\nname = \"{dir}\"\nversion = \"0.1.0\"\n"),
        )
        .unwrap();
        std::fs::write(root.join("src/Main.as"), "void Main() {}\n").unwrap();
        std::fs::write(
            root.join(".openplanet-lsp.toml"),
            format!("game_target = \"{target}\"\n"),
        )
        .unwrap();
    }
    tmp
}

async fn booted(root: &Path) -> LspService<Backend> {
    let (mut service, _socket) = build_service();
    let init = Request::build("initialize")
        .params(serde_json::to_value(init_params(root)).unwrap())
        .id(1)
        .finish();
    let _ = call(&mut service, init).await;
    service
}

fn ask(uri: Option<Url>) -> Request {
    Request::build(METHOD)
        .params(match uri {
            Some(uri) => serde_json::json!({ "uri": uri }),
            None => serde_json::json!({}),
        })
        .id(2)
        .finish()
}

// ---------------------------------------------------------------------------
// Scoping. `config_for` answers a stray document with the legacy `self.config`,
// which MIRRORS THE FIRST SHARD — fine for a feature handler asked about a
// file it does not own, a silent lie for a request whose whole job is to say
// which defines actually apply. These pin where the line is.
// ---------------------------------------------------------------------------

/// One plugin open: a URI outside it is ordinary and unambiguous, and must be
/// ANSWERED, not refused. Covers the commonest single-plugin layout there is
/// — opening the PARENT of the plugin directory, which server-side discovery
/// deliberately supports — plus a scratch buffer and a stray sibling file.
#[tokio::test]
async fn resolved_defines_answers_an_unowned_uri_when_one_plugin_is_open() {
    let opened = multi_root_window("scope-single", &[("alpha", "TURBO")]);
    std::fs::write(opened.join("stray.as"), "void Main() {}\n").unwrap();
    let mut service = booted(&opened).await;

    for uri in [
        // The opened parent folder itself — what the extension sends when no
        // editor is active.
        Some(Url::from_directory_path(&opened).unwrap()),
        // A file beside the plugin, in no plugin.
        Some(Url::from_file_path(opened.join("stray.as")).unwrap()),
        // A scratch buffer: not a file URI at all.
        Some(Url::parse("untitled:Untitled-1").unwrap()),
        // No URI at all.
        None,
    ] {
        let label = format!("{uri:?}");
        let result = call_result(&mut service, ask(uri))
            .await
            .unwrap_or_else(|err| panic!("{label} must be answered, not refused: {err:?}"));
        assert_eq!(result["gameTarget"], "TURBO", "{label}: {result:#}");
        assert!(
            result["root"]
                .as_str()
                .is_some_and(|r| r.ends_with("alpha")),
            "{label}: the answer must name the shard it came from: {result:#}"
        );
    }

    let _ = std::fs::remove_dir_all(&opened);
}

/// A directory URI AT a shard root is an owned lookup, not a stray one.
#[tokio::test]
async fn resolved_defines_accepts_a_shard_root_directory_uri() {
    let opened = multi_root_window("scope-dir", &[("alpha", "TURBO"), ("beta", "TMNEXT")]);
    let mut service = booted(&opened).await;

    let uri = Url::from_directory_path(opened.join("beta")).unwrap();
    let result = call_result(&mut service, ask(Some(uri)))
        .await
        .expect("a shard-root directory URI must answer");
    assert_eq!(result["gameTarget"], "TMNEXT", "{result:#}");
    assert!(result["root"].as_str().is_some_and(|r| r.ends_with("beta")));

    let _ = std::fs::remove_dir_all(&opened);
}

/// SEVERAL plugins open and the URI is inside none of them: refuse.
///
/// `self.config` mirrors the first shard, so the pre-fix fallback answered
/// with `alpha`'s define set — a different plugin's manifest defines and
/// dependencies, presented as this file's. There is no honest answer here,
/// and the extension degrades to its overrides-only view on the rejection.
#[tokio::test]
async fn resolved_defines_refuses_an_unowned_uri_in_a_multi_root_window() {
    let opened = multi_root_window("scope-multi", &[("alpha", "TURBO"), ("beta", "TMNEXT")]);
    std::fs::write(opened.join("stray.as"), "void Main() {}\n").unwrap();
    let mut service = booted(&opened).await;

    for uri in [
        Url::from_file_path(opened.join("stray.as")).unwrap(),
        Url::from_directory_path(&opened).unwrap(),
        Url::parse("untitled:Untitled-1").unwrap(),
    ] {
        let label = uri.to_string();
        let err = call_result(&mut service, ask(Some(uri)))
            .await
            .expect_err(&format!("{label} must not borrow another plugin's config"));
        assert_eq!(
            err.code,
            tower_lsp::jsonrpc::ErrorCode::InvalidParams,
            "{label}: {err:?}"
        );
    }

    // And an OWNED uri in the same window still answers, so the refusal is
    // about ambiguity and not about the request being broken.
    let owned = Url::from_file_path(opened.join("beta/src/Main.as")).unwrap();
    let result = call_result(&mut service, ask(Some(owned)))
        .await
        .expect("an owned uri still answers");
    assert_eq!(result["gameTarget"], "TMNEXT", "{result:#}");

    let _ = std::fs::remove_dir_all(&opened);
}

/// When two live shards nest, the DEEPEST owning root wins — matching
/// `workspace_root_for`, so this request agrees with every other feature
/// about which workspace serves a file.
///
/// Discovery does not descend into a plugin's own subdirectories, so the
/// nesting is created the way a user would: an explicit `plugin_roots`
/// override (the t2 hybrid), which is exactly what that override is for.
#[tokio::test]
async fn resolved_defines_picks_the_deepest_owning_root() {
    let opened = multi_root_window("scope-nested", &[("alpha", "TURBO")]);
    let nested = opened.join("alpha/vendor/beta");
    std::fs::create_dir_all(nested.join("src")).unwrap();
    std::fs::write(
        nested.join("info.toml"),
        "[meta]\nname = \"beta\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(nested.join("src/Main.as"), "void Main() {}\n").unwrap();
    std::fs::write(
        nested.join(".openplanet-lsp.toml"),
        "game_target = \"TMNEXT\"\n",
    )
    .unwrap();

    let (mut service, _socket) = build_service();
    let mut params = init_params(&opened);
    params.initialization_options = Some(serde_json::json!({
        "plugin_roots": [
            opened.join("alpha").to_str().unwrap(),
            nested.to_str().unwrap(),
        ]
    }));
    let init = Request::build("initialize")
        .params(serde_json::to_value(params).unwrap())
        .id(1)
        .finish();
    let _ = call(&mut service, init).await;

    let uri = Url::from_file_path(nested.join("src/Main.as")).unwrap();
    let result = call_result(&mut service, ask(Some(uri)))
        .await
        .expect("a nested plugin's file must answer");
    assert_eq!(
        result["gameTarget"], "TMNEXT",
        "the OUTER plugin's TURBO must not win: {result:#}"
    );
    assert!(
        result["root"].as_str().is_some_and(|r| r.ends_with("beta")),
        "{result:#}"
    );

    // The outer plugin still answers for its own files.
    let outer = Url::from_file_path(opened.join("alpha/src/Main.as")).unwrap();
    let result = call_result(&mut service, ask(Some(outer)))
        .await
        .expect("the outer plugin's file must answer");
    assert_eq!(result["gameTarget"], "TURBO", "{result:#}");

    let _ = std::fs::remove_dir_all(&opened);
}

/// Negative control for every test above.
///
/// Those tests are only evidence that the method is REGISTERED if an
/// unregistered one would have failed. `LspService`'s router answers a method
/// it does not know with `MethodNotFound`, so this pins the thing that makes
/// the passes meaningful: had `build_service` forgotten the `custom_method`
/// call, `openplanet/resolvedDefines` would land here too.
#[tokio::test]
async fn an_unregistered_custom_method_is_rejected_by_the_same_router() {
    let (root, _plugins) = fixture("control", "");
    let (mut service, _socket) = build_service();
    let init = Request::build("initialize")
        .params(serde_json::to_value(init_params(&root)).unwrap())
        .id(1)
        .finish();
    let _ = call(&mut service, init).await;

    let request = Request::build("openplanet/definitelyNotAMethod")
        .params(serde_json::json!({}))
        .id(2)
        .finish();
    let response = service
        .ready()
        .await
        .expect("service ready")
        .call(request)
        .await
        .expect("no transport error")
        .expect("a request gets a response");
    let (_id, result) = response.into_parts();
    let err = result.expect_err("an unregistered method must not answer");
    assert_eq!(
        err.code,
        tower_lsp::jsonrpc::ErrorCode::MethodNotFound,
        "{err:?}"
    );

    let _ = std::fs::remove_dir_all(root.parent().unwrap());
}
