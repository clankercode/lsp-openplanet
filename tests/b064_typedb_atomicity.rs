//! B064: the type database and the stamps that describe it must never
//! disagree.
//!
//! Two INDEPENDENT defects live here, and a fix for either leaves the other
//! standing. Each test below says which one it pins.
//!
//! **(a) Shared-state interleaving.** The loaded index and its stamps lived
//! behind two separate `RwLock`s, written one after the other. A reader takes
//! one lock at a time, so between those two writes it observed a NEW index
//! carrying the PREVIOUS load's stamps — and two regenerations racing could
//! leave that pair permanently crossed.
//!
//! **(b) Load/probe TOCTOU.** `TypeIndex::load` parsed the dump and a second
//! read of the same files took the stamps. Nothing about that needs a second
//! thread: if the game rewrote the dump in between, the stamps described a
//! database that was never loaded. No lock touches this.
//!
//! The discriminator every test here relies on: the two fixture databases
//! differ in their STAMPS *and* in how many symbols they hold. So "which
//! database do these stamps name" and "which database is actually loaded" are
//! separately observable, and a crossed pair is a fact rather than an
//! inference.
//!
//! What this file can and cannot pin, measured against half-fixes rather than
//! assumed. Collapse the locks but keep the second read — the change that
//! "looks complete" — and eight tests here go red. Take the second read away
//! but leave the two locks and NONE of them do: what is left of (a) is two
//! adjacent lock operations with no work between them, and no polling reader
//! catches a window that narrow. The tests therefore pin (b) independently and
//! pin (a) only as it actually shipped. (a) is closed structurally instead:
//! the stamps live inside `TypeIndex`, so there is no second value for the
//! server to store and nothing to split.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures_util::StreamExt;
use serde_json::{json, Value};
use tower::{Service, ServiceExt};
use tower_lsp::jsonrpc::{Error as RpcError, Request};
use tower_lsp::lsp_types::{InitializeParams, InitializeResult, Url, WorkspaceFolder};
use tower_lsp::LspService;

use openplanet_lsp::cli::TypeDbVersions;
use openplanet_lsp::server::{build_service, regen_barrier, Backend};
use openplanet_lsp::typedb::{TypeDbMeta, TypeIndex};

// ---------------------------------------------------------------------------
// Two databases that are distinguishable in both directions
// ---------------------------------------------------------------------------

/// The game dump's stamps, held CONSTANT across both versions.
///
/// Only the Core file varies between [`Version::A`] and [`Version::B`], and
/// that is load-bearing for the concurrent tests: `TypeIndex::load` reads Core
/// and then the game dump, so a test that swapped BOTH files while a load was
/// in flight could produce an index whose halves came from different
/// databases. That is a third defect (the load's own internal TOCTOU between
/// its two files) and not the one under test; keeping the game dump fixed puts
/// it out of reach instead of letting it masquerade as B064.
const GAME_OP: &str = "1.29.5-b064";
const GAME_MP: &str = "2026-08-30 00:00:00";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Version {
    A,
    B,
}

impl Version {
    fn core_op(self) -> &'static str {
        match self {
            Version::A => "core-stamp-A",
            Version::B => "core-stamp-B",
        }
    }

    /// Deliberately large, and deliberately different between the two.
    ///
    /// Different, so the loaded database is identifiable from its symbol
    /// count alone. Large, because the window defect (a) opens — between
    /// publishing an index and publishing its stamps — used to contain a
    /// second full parse of both files, and a reader can only catch a window
    /// wide enough to be caught. A toy database would close it in
    /// microseconds and the test would prove nothing.
    fn function_count(self) -> usize {
        match self {
            Version::A => 8000,
            Version::B => 8004,
        }
    }

    /// Total symbols `TypeIndex` reports for this database: the functions
    /// above plus the single game class.
    fn symbol_count(self) -> usize {
        self.function_count() + 1
    }

    fn from_core_op(op: Option<&str>) -> Option<Version> {
        match op {
            Some("core-stamp-A") => Some(Version::A),
            Some("core-stamp-B") => Some(Version::B),
            _ => None,
        }
    }

    fn from_symbol_count(count: usize) -> Option<Version> {
        [Version::A, Version::B]
            .into_iter()
            .find(|v| v.symbol_count() == count)
    }
}

fn core_json(version: Version) -> String {
    let mut out = String::with_capacity(version.function_count() * 96 + 128);
    out.push_str("{\"op\":\"");
    out.push_str(version.core_op());
    out.push_str("\",\"classes\":[],\"enums\":[],\"funcdefs\":[],\"functions\":[");
    for i in 0..version.function_count() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"ns\":\"Bench\",\"name\":\"Fn{i}\",\"returntypedecl\":\"void\",\"args\":[],\"decl\":\"void Fn{i}()\"}}"
        ));
    }
    out.push_str("]}");
    out
}

fn game_json(op: &str, mp: &str) -> String {
    format!("{{\"op\":\"{op}\",\"mp\":\"{mp}\",\"ns\":{{\"Test\":{{\"CThing\":{{\"m\":[]}}}}}}}}")
}

/// Replace a file in one step.
///
/// `rename` rather than truncate-and-write on purpose: a reader must see the
/// old bytes or the new bytes, never a half-written file. Without this these
/// tests could go red on a torn READ, which is not the defect.
fn replace_file(path: &Path, contents: &str) {
    let tmp = path.with_extension("tmp-write");
    std::fs::write(&tmp, contents).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

fn write_core(op_dir: &Path, version: Version) {
    replace_file(&op_dir.join("OpenplanetCore.json"), &core_json(version));
}

fn write_game(op_dir: &Path, op: &str, mp: &str) {
    replace_file(&op_dir.join("OpenplanetNext.json"), &game_json(op, mp));
}

fn write_db(op_dir: &Path, version: Version) {
    write_core(op_dir, version);
    write_game(op_dir, GAME_OP, GAME_MP);
}

fn core_path(op_dir: &Path) -> PathBuf {
    op_dir.join("OpenplanetCore.json")
}

fn game_path(op_dir: &Path) -> PathBuf {
    op_dir.join("OpenplanetNext.json")
}

// ---------------------------------------------------------------------------
// Scratch trees, cleaned up on the way out (B065)
// ---------------------------------------------------------------------------

/// A temp tree removed when the test ends, red or green.
struct Scratch {
    base: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let base =
            std::env::temp_dir().join(format!("oplsp-b064-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        Scratch { base }
    }

    /// A bare directory holding just the two database files.
    fn typedb_dir(&self, version: Version) -> PathBuf {
        let dir = self.base.join("OpenplanetNext");
        std::fs::create_dir_all(&dir).unwrap();
        write_db(&dir, version);
        dir
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// A plugin root wired to its own `OpenplanetNext` directory.
///
/// `openplanet_dir` and `game_target` are both pinned for the reason
/// `tests/execute_command_tests.rs` documents at length: left unset, the
/// config auto-detects `$HOME/OpenplanetNext` and these tests would load the
/// developer's real database instead of the two-file fixture written here.
struct ServerFixture {
    /// Held only to delete the tree when the test ends.
    _scratch: Scratch,
    root: PathBuf,
    op_dir: PathBuf,
}

impl ServerFixture {
    fn new(name: &str, version: Version) -> Self {
        let scratch = Scratch::new(name);
        let root = scratch.base.join("plugin");
        let op_dir = scratch.base.join("OpenplanetNext");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(&op_dir).unwrap();
        write_db(&op_dir, version);
        std::fs::write(
            root.join("info.toml"),
            format!("[meta]\nname = \"B064 {name}\"\nversion = \"0.1.0\"\n"),
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
        ServerFixture {
            _scratch: scratch,
            root,
            op_dir,
        }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn op_dir(&self) -> &Path {
        &self.op_dir
    }
}

// ---------------------------------------------------------------------------
// LSP plumbing (same shape as tests/execute_command_tests.rs)
// ---------------------------------------------------------------------------

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

async fn boot(root: &Path) -> LspService<Backend> {
    let (mut service, socket) = build_service();
    tokio::spawn(async move {
        let mut socket = socket;
        while socket.next().await.is_some() {}
    });
    let uri = Url::from_directory_path(root).unwrap();
    #[allow(deprecated)]
    let params = InitializeParams {
        root_uri: Some(uri.clone()),
        workspace_folders: Some(vec![WorkspaceFolder {
            uri,
            name: "b064".to_string(),
        }]),
        ..Default::default()
    };
    let initialized = call(
        &mut service,
        Request::build("initialize")
            .params(serde_json::to_value(params).unwrap())
            .id(1)
            .finish(),
    )
    .await
    .expect("initialize succeeds");
    let _: InitializeResult =
        serde_json::from_value(initialized).expect("a well-formed InitializeResult");
    service
}

async fn regenerate(service: &mut LspService<Backend>) -> Value {
    call(
        service,
        Request::build("workspace/executeCommand")
            .params(json!({ "command": "openplanet.regenerateTypeDb", "arguments": [] }))
            .id(99)
            .finish(),
    )
    .await
    .expect("the command is dispatched")
}

/// What the server is serving right now, as ONE observation: the version its
/// loaded index actually is, and the version its stamps claim.
///
/// `None` on either side means "no database", or a database this test did not
/// write — both of which are failures for every caller here, so they are
/// reported rather than silently treated as a match.
async fn observe(backend: &Backend) -> (Option<Version>, Option<Version>, String) {
    let Some((index, stamps)) = backend.loaded_type_db().await else {
        return (None, None, "no type database loaded".to_string());
    };
    let count = index.type_count() + index.function_count() + index.enum_count();
    let detail = format!(
        "index holds {count} symbols; stamps say core_op={:?} game_op={:?} game_mp={:?}",
        stamps.core_op, stamps.game_op, stamps.game_mp
    );
    (
        Version::from_symbol_count(count),
        Version::from_core_op(stamps.core_op.as_deref()),
        detail,
    )
}

/// The B064 invariant, stated once: the database in memory and the stamps
/// reported for it name the SAME load.
fn assert_pair_agrees(loaded: Option<Version>, claimed: Option<Version>, detail: &str, when: &str) {
    assert!(
        loaded.is_some() && loaded == claimed,
        "{when}: the loaded index and its stamps describe different databases — {detail}"
    );
}

// ---------------------------------------------------------------------------
// (b) Load/probe TOCTOU — no concurrency anywhere in this section
// ---------------------------------------------------------------------------

/// The stamps that describe `index`, obtained the way the server obtains them.
///
/// The paths are handed in ON PURPOSE and the fix is what makes them
/// irrelevant: before B064 the only way to answer "what stamps describe this
/// index?" was to read those two files a second time, so the answer tracked
/// the FILE rather than the index. Every test below rewrites the dump after
/// the load and then asks this question; the shape of the helper is the
/// defect, and its body is the fix.
fn reported_stamps(index: &TypeIndex, core: &Path, game: &Path) -> TypeDbVersions {
    let _ = (core, game);
    index.versions().clone()
}

/// (b). The dump is regenerated by the game the instant after it was parsed.
/// The stamps must still describe the bytes in memory.
///
/// Would this pass with only (a) — the single lock — applied? No. There is no
/// lock in this test and no second thread; collapsing the server's two locks
/// cannot change what a re-read of a rewritten file returns.
#[test]
fn stamps_describe_the_parsed_dump_not_the_one_now_on_disk() {
    let scratch = Scratch::new("toctou-both");
    let dir = scratch.typedb_dir(Version::A);
    let (core, game) = (core_path(&dir), game_path(&dir));

    let index = TypeIndex::load(&core, &game).expect("version A loads");
    write_core(&dir, Version::B);
    write_game(&dir, "9.9.9-later", "2099-01-01 00:00:00");

    let stamps = reported_stamps(&index, &core, &game);
    assert_eq!(
        stamps.core_op.as_deref(),
        Some(Version::A.core_op()),
        "core stamp came from the file on disk, not the parse"
    );
    assert_eq!(stamps.game_op.as_deref(), Some(GAME_OP));
    assert_eq!(stamps.game_mp.as_deref(), Some(GAME_MP));
    // And the index really is the one those stamps name.
    assert!(index.lookup_function("Bench::Fn7999").is_some());
    assert!(index.lookup_function("Bench::Fn8003").is_none());
}

/// (b), Core alone. Each file's stamps must come from that file's parse.
#[test]
fn stamps_survive_the_core_dump_alone_being_rewritten() {
    let scratch = Scratch::new("toctou-core");
    let dir = scratch.typedb_dir(Version::A);
    let (core, game) = (core_path(&dir), game_path(&dir));

    let index = TypeIndex::load(&core, &game).expect("version A loads");
    write_core(&dir, Version::B);

    let stamps = reported_stamps(&index, &core, &game);
    assert_eq!(stamps.core_op.as_deref(), Some(Version::A.core_op()));
    assert_eq!(stamps.game_op.as_deref(), Some(GAME_OP));
}

/// (b), game dump alone.
#[test]
fn stamps_survive_the_game_dump_alone_being_rewritten() {
    let scratch = Scratch::new("toctou-game");
    let dir = scratch.typedb_dir(Version::A);
    let (core, game) = (core_path(&dir), game_path(&dir));

    let index = TypeIndex::load(&core, &game).expect("version A loads");
    write_game(&dir, "9.9.9-later", "2099-01-01 00:00:00");

    let stamps = reported_stamps(&index, &core, &game);
    assert_eq!(stamps.core_op.as_deref(), Some(Version::A.core_op()));
    assert_eq!(stamps.game_op.as_deref(), Some(GAME_OP));
    assert_eq!(stamps.game_mp.as_deref(), Some(GAME_MP));
}

/// (b), the harshest variant: the files are GONE. A loaded database still has
/// a version, and a server that can only answer by reading the disk has to
/// report `<none>` for a database it is actively serving.
#[test]
fn stamps_survive_the_dump_being_deleted_after_the_load() {
    let scratch = Scratch::new("toctou-deleted");
    let dir = scratch.typedb_dir(Version::A);
    let (core, game) = (core_path(&dir), game_path(&dir));

    let index = TypeIndex::load(&core, &game).expect("version A loads");
    std::fs::remove_file(&core).unwrap();
    std::fs::remove_file(&game).unwrap();

    let stamps = reported_stamps(&index, &core, &game);
    assert_eq!(stamps.core_op.as_deref(), Some(Version::A.core_op()));
    assert_eq!(stamps.game_op.as_deref(), Some(GAME_OP));
    assert_eq!(stamps.game_mp.as_deref(), Some(GAME_MP));
}

/// (b), the half-written case: a harvest in progress leaves bytes that do not
/// parse. The loaded database is untouched by that and its stamps must be too.
#[test]
fn stamps_survive_the_dump_being_replaced_with_unparseable_bytes() {
    let scratch = Scratch::new("toctou-garbage");
    let dir = scratch.typedb_dir(Version::A);
    let (core, game) = (core_path(&dir), game_path(&dir));

    let index = TypeIndex::load(&core, &game).expect("version A loads");
    replace_file(&core, "{\"op\": \"truncated");
    replace_file(&game, "not json at all");

    let stamps = reported_stamps(&index, &core, &game);
    assert_eq!(stamps.core_op.as_deref(), Some(Version::A.core_op()));
    assert_eq!(stamps.game_op.as_deref(), Some(GAME_OP));
}

/// (b), through the other loader. `load_with_header` is what `typedb diff` and
/// the `--typedb-dir` path reach for, so the stamps must be carried out of it
/// too — otherwise the fix covers one entry point and not the other.
#[test]
fn load_with_header_carries_the_same_stamps() {
    let scratch = Scratch::new("toctou-header");
    let dir = scratch.typedb_dir(Version::A);
    let (core, game) = (core_path(&dir), game_path(&dir));

    let index = TypeIndex::load_with_header(&core, &game, None).expect("version A loads");
    write_core(&dir, Version::B);

    let stamps = reported_stamps(&index, &core, &game);
    assert_eq!(stamps.core_op.as_deref(), Some(Version::A.core_op()));
    assert_eq!(stamps.game_op.as_deref(), Some(GAME_OP));
    assert_eq!(stamps.game_mp.as_deref(), Some(GAME_MP));
}

/// (b), the no-regression half: when NOTHING moves, the stamps carried out of
/// the load are exactly what a read of the files says. Without this the tests
/// above could be satisfied by stamps that are merely stable and wrong.
#[test]
fn stamps_from_the_load_match_the_untouched_files() {
    let scratch = Scratch::new("toctou-agrees");
    let dir = scratch.typedb_dir(Version::B);
    let (core, game) = (core_path(&dir), game_path(&dir));

    let index = TypeIndex::load(&core, &game).expect("version B loads");
    let stamps = reported_stamps(&index, &core, &game);
    let core_meta = TypeDbMeta::probe(&core).expect("core probes");
    let game_meta = TypeDbMeta::probe(&game).expect("game probes");

    assert_eq!(stamps.core_op, core_meta.op);
    assert_eq!(stamps.game_op, game_meta.op);
    assert_eq!(stamps.game_mp, game_meta.mp);
}

// ---------------------------------------------------------------------------
// (b) through the server, deterministically
// ---------------------------------------------------------------------------

/// (b), at the level the user sees it. The regeneration is frozen at the point
/// where the new index exists and nothing has been published, the dump is
/// swapped underneath it, and it is let go.
///
/// `regenerated` says which database was loaded; `after.op` says which one the
/// server claims to have loaded. They must name the same one.
///
/// Would this pass with only (a) applied? No. The barrier sits BEFORE any
/// publishing, so a fix that merely collapses the two locks still takes its
/// stamps from a read that happens after the swap, and still reports the file
/// that is on disk at that moment rather than the one in memory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn regenerate_reports_the_database_it_loaded_when_the_dump_moves_mid_flight() {
    let _guard = regen_barrier::lock().lock().await;
    let fixture = ServerFixture::new("midflight", Version::A);
    let mut service = boot(fixture.root()).await;
    let backend = service.inner().clone();

    // The user regenerated because the game rewrote the dump: disk is now B.
    write_core(fixture.op_dir(), Version::B);

    let armed = regen_barrier::arm();
    let handle = tokio::spawn(async move {
        let payload = regenerate(&mut service).await;
        (service, payload)
    });
    armed.wait_until_reached().await;

    // ... and it rewrites it AGAIN while the reload is in flight. Version B is
    // parsed and in hand; version A is what a second read would find.
    write_core(fixture.op_dir(), Version::A);
    armed.release();

    let (_service, payload) = handle.await.expect("the regeneration task finishes");
    drop(armed);

    assert_eq!(payload["ok"], json!(true), "{payload}");
    assert_eq!(
        payload["regenerated"].as_u64().unwrap_or(0),
        Version::B.symbol_count() as u64,
        "the reload parsed version B; this test is built on that: {payload}"
    );
    assert_eq!(
        payload["after"]["op"],
        json!(Version::B.core_op()),
        "`after` must name the database that was loaded, not the later disk contents: {payload}"
    );

    let (loaded, claimed, detail) = observe(&backend).await;
    assert_pair_agrees(loaded, claimed, &detail, "after a mid-flight rewrite");
}

// ---------------------------------------------------------------------------
// (a) Shared-state interleaving
// ---------------------------------------------------------------------------

/// (a). A reader that looks while a regeneration is publishing must see one
/// load or the other, never half of each.
///
/// The barrier parks the regeneration with the new index parsed and nothing
/// published, so the reader is already spinning when the publish begins — it
/// samples the exact interval the defect lives in rather than hoping to land
/// in it. Repeated, because one sample of a window is an anecdote.
///
/// Would this pass with only (b) applied? MEASURED, AND YES — which is worth
/// knowing rather than glossing. Against the original code this test fails on
/// round 1 (`index holds 8001 symbols; stamps say core-stamp-B`), because the
/// stamps published second were a whole second read of two ~800KB files and the
/// crossing stood open for milliseconds. Take that read away but leave the two
/// locks, and the gap shrinks to "release one lock, acquire another" with no
/// work in between — around a hundred nanoseconds — and this reader stopped
/// catching it in either lock order.
///
/// So: this pins the defect as it SHIPPED, and it does not discriminate a
/// residual two-lock split once the stamps come out of the load. No polling
/// test could. What closes that hole is not a better test but the shape of the
/// fix: the stamps live inside `TypeIndex`, so the server has no second value
/// to store and the split is unrepresentable rather than merely absent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reader_never_sees_an_index_with_another_loads_stamps() {
    let _guard = regen_barrier::lock().lock().await;
    let fixture = ServerFixture::new("reader", Version::A);
    let mut service = boot(fixture.root()).await;
    let backend = service.inner().clone();

    let (loaded, claimed, detail) = observe(&backend).await;
    assert_pair_agrees(loaded, claimed, &detail, "at rest, before anything ran");

    let mut want = Version::A;
    for round in 0..3 {
        // Flip the database so the incoming load's stamps differ from the
        // ones already stored. Without that the two are indistinguishable and
        // a crossed pair would look identical to a whole one.
        want = if want == Version::A {
            Version::B
        } else {
            Version::A
        };
        write_core(fixture.op_dir(), want);

        let armed = regen_barrier::arm();
        let handle = tokio::spawn(async move {
            let payload = regenerate(&mut service).await;
            (service, payload)
        });
        armed.wait_until_reached().await;

        // The watcher stops when the publish is OVER, not on a timer.
        // Everything this test can see happens between the release below and
        // the regeneration returning; sampling an idle server after that adds
        // no coverage and would put the dead time into every future run of the
        // suite. Measured: 17s of suite time down to under 2s, with the
        // detection unchanged (the crossed pair is still caught on round 0
        // against the pre-fix code). The deadline is only a backstop, so a
        // wedged regeneration fails here rather than hanging.
        let stop = Arc::new(AtomicBool::new(false));
        let reader = backend.clone();
        let watch_stop = stop.clone();
        let watcher = tokio::spawn(async move {
            let mut torn: Option<String> = None;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while torn.is_none() && !watch_stop.load(Ordering::SeqCst) {
                let (loaded, claimed, detail) = observe(&reader).await;
                if loaded.is_none() || loaded != claimed {
                    torn = Some(detail);
                }
                if std::time::Instant::now() > deadline {
                    break;
                }
                tokio::task::yield_now().await;
            }
            torn
        });

        armed.release();
        let (returned, payload) = handle.await.expect("the regeneration task finishes");
        service = returned;
        stop.store(true, Ordering::SeqCst);
        drop(armed);
        assert_eq!(payload["ok"], json!(true), "round {round}: {payload}");

        let torn = watcher.await.expect("the watcher finishes");
        assert!(
            torn.is_none(),
            "round {round}: a reader saw a crossed pair while a regeneration \
             was publishing — {}",
            torn.unwrap_or_default()
        );

        let (loaded, claimed, detail) = observe(&backend).await;
        assert_pair_agrees(loaded, claimed, &detail, &format!("after round {round}"));
    }
}

/// (a). Concurrent regenerations must not interleave into a pair that
/// describes two different loads — the outcome the ticket was filed for.
///
/// No barrier: this one races on purpose, because the permanent crossing needs
/// two publishes to interleave and a barrier that stops one of them removes
/// the race it is trying to reproduce. Only the Core file moves, so every load
/// gets a whole database (see [`GAME_OP`]).
///
/// Would this pass with only (a) applied? Measured: no — it fails on the
/// per-answer check, because a regeneration that probes the files after
/// parsing them reports a version the flipper has already moved past. With
/// only (b) applied it passes, for the reason spelled out on
/// [`a_reader_never_sees_an_index_with_another_loads_stamps`]: the crossing
/// two locks still permit is too narrow to be caught by racing it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_regenerations_leave_the_pair_agreeing() {
    let _guard = regen_barrier::lock().lock().await;
    let fixture = ServerFixture::new("concurrent", Version::A);
    let service = boot(fixture.root()).await;
    let backend = service.inner().clone();
    drop(service);

    let op_dir = fixture.op_dir().to_path_buf();
    let flipper = tokio::spawn(async move {
        let mut want = Version::A;
        for _ in 0..40 {
            want = if want == Version::A {
                Version::B
            } else {
                Version::A
            };
            write_core(&op_dir, want);
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    });

    let mut regens = Vec::new();
    for _ in 0..6 {
        let backend = backend.clone();
        regens.push(tokio::spawn(async move {
            use tower_lsp::LanguageServer;
            let mut payloads = Vec::new();
            for _ in 0..4 {
                let value = backend
                    .execute_command(tower_lsp::lsp_types::ExecuteCommandParams {
                        command: "openplanet.regenerateTypeDb".to_string(),
                        arguments: Vec::new(),
                        work_done_progress_params: Default::default(),
                    })
                    .await
                    .expect("the command runs")
                    .expect("the command answers");
                payloads.push(value);
            }
            payloads
        }));
    }

    for regen in regens {
        for payload in regen.await.expect("a regeneration task finishes") {
            // Each ANSWER must be self-consistent too: `after` names the
            // database whose symbol count `regenerated` reports.
            let claimed = Version::from_core_op(payload["after"]["op"].as_str());
            let loaded =
                Version::from_symbol_count(payload["regenerated"].as_u64().unwrap_or(0) as usize);
            assert!(
                loaded.is_some() && loaded == claimed,
                "a regeneration reported stamps for a database it did not load: {payload}"
            );
        }
    }
    flipper.await.expect("the flipper finishes");

    let (loaded, claimed, detail) = observe(&backend).await;
    assert_pair_agrees(loaded, claimed, &detail, "after concurrent regenerations");
}

/// (a) and (b). A regeneration that FAILS must leave the server exactly as it
/// was: still serving the database it had, still able to say which one that is.
///
/// Would this pass with only one half applied? Today, yes — it is a guard
/// rather than a reproduction, and it is here because the failure path is the
/// one place a refactor can drop the pair without any test noticing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_regeneration_leaves_the_pair_agreeing() {
    let _guard = regen_barrier::lock().lock().await;
    let fixture = ServerFixture::new("failed", Version::A);
    let mut service = boot(fixture.root()).await;
    let backend = service.inner().clone();

    replace_file(&core_path(fixture.op_dir()), "{\"op\": \"truncated");

    let payload = regenerate(&mut service).await;
    assert_eq!(
        payload["ok"],
        json!(false),
        "an unparseable dump must fail the regeneration: {payload}"
    );
    assert_eq!(
        payload["before"]["op"],
        json!(Version::A.core_op()),
        "the failure answer must still name the database still loaded: {payload}"
    );

    let (loaded, claimed, detail) = observe(&backend).await;
    assert_pair_agrees(loaded, claimed, &detail, "after a failed regeneration");
    assert_eq!(
        loaded,
        Some(Version::A),
        "the failed reload replaced the serving database: {detail}"
    );
}

/// Neither (a) nor (b): the single-threaded answer must not have MOVED. With
/// nothing touching the dump, what the server reports is what a read of the
/// files says — the same values B039's tests pin, restated here against a
/// fixture whose stamps this file controls.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quiet_regeneration_reports_what_is_on_disk() {
    let _guard = regen_barrier::lock().lock().await;
    let fixture = ServerFixture::new("quiet", Version::A);
    let mut service = boot(fixture.root()).await;

    let payload = regenerate(&mut service).await;
    assert_eq!(payload["ok"], json!(true), "{payload}");
    for side in ["before", "after"] {
        assert_eq!(
            payload[side]["op"],
            json!(Version::A.core_op()),
            "{payload}"
        );
        assert_eq!(payload[side]["game_op"], json!(GAME_OP), "{payload}");
        assert_eq!(payload[side]["mp"], json!(GAME_MP), "{payload}");
    }
    assert_eq!(
        payload["changed"],
        json!(false),
        "nothing moved, so nothing changed: {payload}"
    );
    assert_eq!(
        payload["regenerated"].as_u64().unwrap_or(0),
        Version::A.symbol_count() as u64,
        "{payload}"
    );
}

/// Neither (a) nor (b): the ordinary sequential case still REPORTS a change
/// when there is one. A fix that froze the stamps at the first load would pass
/// every test above and break the command's whole purpose.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sequential_regeneration_still_reports_a_real_change() {
    let _guard = regen_barrier::lock().lock().await;
    let fixture = ServerFixture::new("changed", Version::A);
    let mut service = boot(fixture.root()).await;

    write_core(fixture.op_dir(), Version::B);
    let payload = regenerate(&mut service).await;

    assert_eq!(payload["ok"], json!(true), "{payload}");
    assert_eq!(
        payload["before"]["op"],
        json!(Version::A.core_op()),
        "{payload}"
    );
    assert_eq!(
        payload["after"]["op"],
        json!(Version::B.core_op()),
        "{payload}"
    );
    assert_eq!(payload["changed"], json!(true), "{payload}");
    assert_eq!(
        payload["regenerated"].as_u64().unwrap_or(0),
        Version::B.symbol_count() as u64,
        "{payload}"
    );

    let backend = service.inner();
    let (loaded, claimed, detail) = observe(backend).await;
    assert_pair_agrees(loaded, claimed, &detail, "after a sequential regeneration");
}

/// Keeps the discriminator honest. Every test above reads "which database is
/// loaded" out of a symbol count, so the two fixtures must actually differ in
/// it — if a future edit made them equal, the assertions would still pass and
/// mean nothing.
#[test]
fn the_two_fixture_databases_are_distinguishable() {
    assert_ne!(Version::A.core_op(), Version::B.core_op());
    assert_ne!(Version::A.symbol_count(), Version::B.symbol_count());

    let scratch = Scratch::new("distinguishable");
    for version in [Version::A, Version::B] {
        let dir = scratch.base.join(format!("db-{version:?}"));
        std::fs::create_dir_all(&dir).unwrap();
        write_db(&dir, version);
        let index = TypeIndex::load(&core_path(&dir), &game_path(&dir)).expect("loads");
        let count = index.type_count() + index.function_count() + index.enum_count();
        assert_eq!(
            count,
            version.symbol_count(),
            "the symbol count this file predicts for {version:?} is wrong, so \
             every `which database is loaded` assertion is unanchored"
        );
    }
}

/// The barrier is a test seam in production code, so it needs its own guard:
/// disarmed, it must be invisible. A regeneration that stopped at it in a
/// normal run would hang the server.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_barrier_is_inert_when_no_test_armed_it() {
    let _guard = regen_barrier::lock().lock().await;
    let fixture = ServerFixture::new("inert", Version::A);
    let mut service = boot(fixture.root()).await;

    let payload =
        tokio::time::timeout(std::time::Duration::from_secs(30), regenerate(&mut service))
            .await
            .expect("a regeneration with the barrier disarmed must not block");
    assert_eq!(payload["ok"], json!(true), "{payload}");
}

/// An armed regeneration must not remain stranded when an assertion unwinds
/// past its guard. Dropping the guard after the generation reaches the seam
/// cancels that exact wait and lets it publish.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_an_armed_guard_releases_the_generation_that_reached_it() {
    let _guard = regen_barrier::lock().lock().await;
    let fixture = ServerFixture::new("drop-releases", Version::A);
    let mut service = boot(fixture.root()).await;

    let armed = regen_barrier::arm();
    let handle = tokio::spawn(async move {
        let payload = regenerate(&mut service).await;
        (service, payload)
    });
    armed.wait_until_reached().await;
    drop(armed);

    let (_service, payload) = tokio::time::timeout(std::time::Duration::from_secs(30), handle)
        .await
        .expect("dropping the armed guard must release the paused regeneration")
        .expect("the regeneration task finishes");
    assert_eq!(payload["ok"], json!(true), "{payload}");
}

/// Arming is for the NEXT regeneration only. A second command may parse and
/// publish while the first one is paused at the seam; otherwise one test
/// rendezvous would stall unrelated concurrent requests.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_armed_barrier_pauses_only_one_concurrent_regeneration() {
    use tower_lsp::LanguageServer;

    let _guard = regen_barrier::lock().lock().await;
    let fixture = ServerFixture::new("one-shot", Version::A);
    let service = boot(fixture.root()).await;
    let backend = service.inner().clone();
    drop(service);

    let armed = regen_barrier::arm();
    let paused_backend = backend.clone();
    let paused = tokio::spawn(async move {
        paused_backend
            .execute_command(tower_lsp::lsp_types::ExecuteCommandParams {
                command: "openplanet.regenerateTypeDb".to_string(),
                arguments: Vec::new(),
                work_done_progress_params: Default::default(),
            })
            .await
            .expect("the paused command runs")
            .expect("the paused command answers")
    });
    armed.wait_until_reached().await;

    let second = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        backend.execute_command(tower_lsp::lsp_types::ExecuteCommandParams {
            command: "openplanet.regenerateTypeDb".to_string(),
            arguments: Vec::new(),
            work_done_progress_params: Default::default(),
        }),
    )
    .await
    .expect("a second regeneration must not wait behind the one-shot barrier")
    .expect("the second command runs")
    .expect("the second command answers");
    assert_eq!(second["ok"], json!(true), "{second}");

    armed.release();
    let first = tokio::time::timeout(std::time::Duration::from_secs(30), paused)
        .await
        .expect("releasing the armed guard finishes the paused regeneration")
        .expect("the paused regeneration task finishes");
    assert_eq!(first["ok"], json!(true), "{first}");
}
