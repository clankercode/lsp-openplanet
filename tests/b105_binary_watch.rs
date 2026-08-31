//! B105 end-to-end: the server notices its own binary was replaced and gets
//! the editor onto the new one.
//!
//! The rules themselves are unit-tested over the pure seam in
//! `server::binary_watch`. What *this* file exists to prove is the part a pure
//! test cannot: that the seam is actually wired to a running server — a real
//! `Backend`, a real tokio timer, a real file swapped on disk — and that the
//! notification really reaches the client socket under the method name the
//! extension will subscribe to.
//!
//! Without this file every rule could be green while `spawn_binary_watch` was
//! never called, watched the wrong path, or sent nothing.
//!
//! Timing: the watch interval is injected (milliseconds, not the production
//! 30s), so nothing here sleeps for a fixed duration hoping the work is done.
//! Waits are bounded polls with a deadline.

use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::StreamExt;
use openplanet_lsp::server::binary_watch::{ReplacedMode, ServerBinaryReplaced};
use openplanet_lsp::server::Backend;
use openplanet_lsp::testing::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::{Service, ServiceExt};
use tower_lsp::lsp_types::notification::Notification;
use tower_lsp::LspService;

/// A tick fast enough that a test never waits long, but not so fast that the
/// runtime spends its time in the timer.
const TICK: Duration = Duration::from_millis(10);

/// Generous upper bound on "the watcher should have fired by now" — ~200
/// ticks. Only reached when the test is about to fail anyway.
const DEADLINE: Duration = Duration::from_secs(2);

fn temp_dir(label: &str) -> TempDir {
    TempDir::new(&format!("openplanet-lsp-b105-{label}"))
}

/// A stand-in for the installed executable. Never actually executed; the
/// watcher only ever stats it.
fn fake_exe(dir: &Path, contents: &[u8]) -> PathBuf {
    let path = dir.join("openplanet-lsp");
    fs::write(&path, contents).unwrap();
    path
}

/// Replace `target` exactly the way `update::replace_executable` does: stage a
/// sidecar beside it, then rename over the top.
fn rename_swap(target: &Path, contents: &[u8]) {
    let staging = target.with_extension(format!("new.{}", std::process::id()));
    fs::write(&staging, contents).unwrap();

    #[cfg(windows)]
    {
        let old = target.with_extension(format!("old.{}", std::process::id()));
        let _ = fs::remove_file(&old);
        fs::rename(target, &old).unwrap();
        assert!(
            !target.exists(),
            "the Windows updater exposes a transient target-away window"
        );
        let _ = fs::remove_file(old);
    }

    fs::rename(&staging, target).unwrap();
}

/// A service that has completed `initialize`, as a real editor session has.
///
/// This is not ceremony. tower-lsp **suppresses every `send_notification`
/// until the service is initialized** (`Client::send_notification` checks
/// `State::Initialized` first), so a watcher wired to an uninitialized service
/// silently sends nothing. That is the same class of defect as B040, and it is
/// invisible without driving the real `initialize` through the service — which
/// is exactly what these tests do.
async fn initialized_service(
    init_options: Option<serde_json::Value>,
) -> (LspService<Backend>, tower_lsp::ClientSocket) {
    let (mut service, socket) = LspService::new(Backend::new);
    let mut params = serde_json::json!({"capabilities": {}});
    if let Some(options) = init_options {
        params["initializationOptions"] = options;
    }
    let request = tower_lsp::jsonrpc::Request::build("initialize")
        .id(1)
        .params(params)
        .finish();
    let response = service
        .ready()
        .await
        .expect("the service is ready")
        .call(request)
        .await
        .expect("initialize does not fail")
        .expect("initialize has a response");
    let response = serde_json::to_value(&response).unwrap();
    assert!(
        response["result"]["capabilities"].is_object(),
        "initialize must succeed or the notification gate stays shut: {response}"
    );
    (service, socket)
}

/// The next `openplanet/serverBinaryReplaced` on the socket, or `None` if
/// `deadline` passes without one. Other traffic (`window/showMessage`,
/// diagnostics) is skipped rather than mistaken for a signal.
async fn binary_replaced_within(
    socket: &mut tower_lsp::ClientSocket,
    deadline: Duration,
) -> Option<serde_json::Value> {
    tokio::time::timeout(deadline, async {
        loop {
            let request = socket.next().await.expect("client socket remains open");
            if request.method() == <ServerBinaryReplaced as Notification>::METHOD {
                return request
                    .params()
                    .expect("notification carries params")
                    .clone();
            }
        }
    })
    .await
    .ok()
}

async fn next_binary_replaced(socket: &mut tower_lsp::ClientSocket) -> Option<serde_json::Value> {
    binary_replaced_within(socket, DEADLINE).await
}

#[tokio::test]
async fn a_replaced_binary_notifies_an_opted_in_client_exactly_once() {
    let dir = temp_dir("notify");
    let exe = fake_exe(&dir, b"the original binary");

    let (service, mut socket) = initialized_service(None).await;
    service
        .inner()
        .spawn_binary_watch_at(exe.clone(), ReplacedMode::Notify, TICK);

    // Let a few ticks pass over an UNCHANGED binary first: if the watcher
    // fired on its own baseline, the assertion below would pass for the wrong
    // reason and this test would prove nothing.
    tokio::time::sleep(TICK * 5).await;

    rename_swap(&exe, b"a considerably newer binary");

    let params = next_binary_replaced(&mut socket)
        .await
        .expect("a replaced binary notifies an opted-in client");

    assert_eq!(
        params["path"],
        serde_json::Value::String(exe.display().to_string()),
        "the notification names the path that changed"
    );
    assert!(
        params["runningVersion"].is_string(),
        "the notification carries the stale version: {params}"
    );
    assert_ne!(
        params["previous"], params["current"],
        "the notification carries both stamps, and they differ: {params}"
    );

    // Exactly once, not once per tick. Keep replacing the file — a watcher
    // that re-fires would produce a second notification well inside this
    // window.
    for generation in 0..5u8 {
        rename_swap(&exe, &[b'x'; 64].map(|b| b + generation));
        tokio::time::sleep(TICK * 4).await;
    }
    assert!(
        next_binary_replaced(&mut socket).await.is_none(),
        "the server must signal a replacement exactly once, never per poll"
    );
}

#[tokio::test]
async fn an_unchanged_binary_notifies_nothing() {
    // The negative control for the test above: same wiring, no swap.
    let dir = temp_dir("unchanged");
    let exe = fake_exe(&dir, b"the original binary");

    let (service, mut socket) = initialized_service(None).await;
    service
        .inner()
        .spawn_binary_watch_at(exe, ReplacedMode::Notify, TICK);

    assert!(
        next_binary_replaced(&mut socket).await.is_none(),
        "an untouched binary must never notify, however many ticks pass"
    );
}

#[tokio::test]
async fn a_client_that_did_not_opt_in_gets_the_clean_exit_fallback() {
    let dir = temp_dir("fallback");
    let exe = fake_exe(&dir, b"the original binary");

    let (service, mut socket) = initialized_service(None).await;
    let in_flight = service.inner().in_flight();
    service
        .inner()
        .spawn_binary_watch_at(exe.clone(), ReplacedMode::Exit, TICK);

    tokio::time::sleep(TICK * 5).await;
    assert!(
        !in_flight.is_draining(),
        "an unchanged binary must not end the process"
    );

    rename_swap(&exe, b"a considerably newer binary");

    let deadline = tokio::time::Instant::now() + DEADLINE;
    while !in_flight.is_draining() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(TICK).await;
    }
    assert!(
        in_flight.is_draining(),
        "a client that did not opt in gets a clean exit once idle"
    );

    // And it exits instead of notifying — a client with no handler must not be
    // sent a method it never subscribed to.
    assert!(
        next_binary_replaced(&mut socket).await.is_none(),
        "the fallback path sends no notification"
    );

    // Still exactly once, even as the binary keeps changing.
    for generation in 0..5u8 {
        rename_swap(&exe, &[b'y'; 64].map(|b| b + generation));
        tokio::time::sleep(TICK * 4).await;
    }
    assert!(
        in_flight.is_draining(),
        "the fallback must end the process once, not once per poll"
    );
}

#[tokio::test]
async fn an_in_flight_request_defers_the_restart_until_the_server_is_idle() {
    // The deferral rule, end to end: the in-flight count is the real one the
    // wrapper maintains, not a bool a test made up.
    let dir = temp_dir("deferred");
    let exe = fake_exe(&dir, b"the original binary");

    let (service, mut socket) = initialized_service(None).await;
    let in_flight = service.inner().in_flight();

    // Hold a message "in flight" exactly as the service wrapper does while a
    // request is being handled.
    let busy = in_flight.enter();
    assert!(!service.inner().is_idle());

    service
        .inner()
        .spawn_binary_watch_at(exe.clone(), ReplacedMode::Notify, TICK);
    tokio::time::sleep(TICK * 3).await;
    rename_swap(&exe, b"a considerably newer binary");

    // Many ticks pass with the replacement already on disk and the server
    // busy: nothing may be sent.
    tokio::time::sleep(TICK * 25).await;
    assert!(
        binary_replaced_within(&mut socket, TICK * 20)
            .await
            .is_none(),
        "a busy server must not be restarted out from under a pending request"
    );

    // Finishing the request releases it.
    drop(busy);
    assert!(service.inner().is_idle());
    assert!(
        next_binary_replaced(&mut socket).await.is_some(),
        "going idle must release the deferred signal"
    );
}

#[tokio::test]
async fn an_off_client_is_never_restarted() {
    let dir = temp_dir("off");
    let exe = fake_exe(&dir, b"the original binary");

    let (service, mut socket) = initialized_service(None).await;
    let in_flight = service.inner().in_flight();
    service
        .inner()
        .spawn_binary_watch_at(exe.clone(), ReplacedMode::Off, TICK);

    tokio::time::sleep(TICK * 3).await;
    rename_swap(&exe, b"a considerably newer binary");

    assert!(
        next_binary_replaced(&mut socket).await.is_none(),
        "an opted-out client is never notified"
    );
    assert!(
        !in_flight.is_draining(),
        "an opted-out client is never exited"
    );
}

#[tokio::test]
async fn a_binary_that_disappears_is_not_a_replacement() {
    // The safety rule that matters most: exiting toward a path with no file is
    // a restart loop, not an upgrade.
    let dir = temp_dir("deleted");
    let exe = fake_exe(&dir, b"the original binary");

    let (service, _socket) = initialized_service(None).await;
    let in_flight = service.inner().in_flight();
    service
        .inner()
        .spawn_binary_watch_at(exe.clone(), ReplacedMode::Exit, TICK);

    tokio::time::sleep(TICK * 3).await;
    fs::remove_file(&exe).unwrap();
    tokio::time::sleep(TICK * 25).await;

    assert!(
        !in_flight.is_draining(),
        "a missing binary must never end the process"
    );

    // ...but its replacement arriving still counts.
    fs::write(&exe, b"the new binary, finally installed").unwrap();
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while !in_flight.is_draining() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(TICK).await;
    }
    assert!(
        in_flight.is_draining(),
        "the reappearance of a new binary is the signal"
    );
}

#[tokio::test]
async fn the_client_opt_in_survives_the_initialize_round_trip() {
    // Closes the gap between "the parser understands the key" (unit-tested)
    // and "the server acts on what the client actually sent": the option has
    // to survive `initialize`, land in the cached initializationOptions, and
    // be read back by the same accessor `initialized` uses.
    for (sent, want) in [
        (
            Some(serde_json::json!({"on_binary_replaced": "notify"})),
            ReplacedMode::Notify,
        ),
        (
            Some(serde_json::json!({"on_binary_replaced": "exit"})),
            ReplacedMode::Exit,
        ),
        (
            Some(serde_json::json!({"on_binary_replaced": "off"})),
            ReplacedMode::Off,
        ),
        // A client that says nothing gets the fallback, never the notification.
        (
            Some(serde_json::json!({"game_target": "TMNEXT"})),
            ReplacedMode::Exit,
        ),
        (
            Some(serde_json::json!({"on_binary_replaced": "nonsense"})),
            ReplacedMode::Exit,
        ),
        (None, ReplacedMode::Exit),
    ] {
        let (service, _socket) = initialized_service(sent.clone()).await;
        assert_eq!(
            service.inner().binary_watch_mode().await,
            want,
            "initializationOptions {sent:?} must select {want:?}"
        );
    }
}

#[tokio::test]
async fn the_in_flight_wrapper_is_transparent_to_the_protocol() {
    // The counter lives in a wrapper around the whole LSP service, so a bug in
    // it would break every request rather than one feature. Drive a real
    // `initialize` through the wrapper and check both halves: the response is
    // unchanged, and the count returns to zero afterwards.
    // Deliberately NOT `initialized_service`: this test sends the `initialize`
    // request itself, through the wrapper, which is the point.
    let (service, _socket) = LspService::new(Backend::new);
    let in_flight = service.inner().in_flight();
    let mut service =
        openplanet_lsp::server::inflight::CountedService::new(in_flight.clone(), service);

    assert!(in_flight.is_idle());

    let request = tower_lsp::jsonrpc::Request::build("initialize")
        .id(1)
        .params(serde_json::json!({"capabilities": {}}))
        .finish();

    let response = service
        .ready()
        .await
        .unwrap()
        .call(request)
        .await
        .expect("the wrapper passes the call through")
        .expect("initialize has a response");

    let response = serde_json::to_value(&response).expect("a serialisable response");
    assert!(
        response["result"]["capabilities"].is_object(),
        "the wrapper must not alter the response: {response}"
    );
    assert!(
        in_flight.is_idle(),
        "a completed request releases its in-flight slot"
    );
}

#[tokio::test]
async fn production_builder_captures_baseline_before_initialized_and_activates_the_watch() {
    let dir = temp_dir("construction-baseline");
    let exe = fake_exe(&dir, b"the original binary");

    // This is the production builder seam: it captures the executable stamp
    // and installs the same counted wrapper run_stdio serves.
    let (mut service, mut socket) =
        openplanet_lsp::server::build_restartable_service_with_binary_watch_at(exe.clone(), TICK);

    // Replacement happens after construction but before the LSP client sends
    // `initialized`. Taking the baseline in the initialized hook would miss it.
    rename_swap(&exe, b"the replacement installed during initialize");

    let initialize = tower_lsp::jsonrpc::Request::build("initialize")
        .id(1)
        .params(serde_json::json!({
            "capabilities": {},
            "initializationOptions": {"on_binary_replaced": "notify"}
        }))
        .finish();
    service
        .ready()
        .await
        .unwrap()
        .call(initialize)
        .await
        .unwrap()
        .expect("initialize responds");

    let initialized = tower_lsp::jsonrpc::Request::build("initialized")
        .params(serde_json::json!({}))
        .finish();
    assert!(
        service
            .ready()
            .await
            .unwrap()
            .call(initialized)
            .await
            .unwrap()
            .is_none(),
        "initialized is a notification"
    );

    let params = next_binary_replaced(&mut socket)
        .await
        .expect("the production initialized hook activates the construction-time watch");
    assert_ne!(params["previous"], params["current"]);
}

#[tokio::test]
async fn fallback_drains_the_real_transport_after_its_final_response() {
    use std::convert::Infallible;
    use tokio::sync::oneshot;

    struct DelayedResponse {
        started: Option<oneshot::Sender<()>>,
        release: Option<oneshot::Receiver<()>>,
    }

    impl Service<tower_lsp::jsonrpc::Request> for DelayedResponse {
        type Response = Option<tower_lsp::jsonrpc::Response>;
        type Error = Infallible;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: tower_lsp::jsonrpc::Request) -> Self::Future {
            self.started.take().unwrap().send(()).unwrap();
            let release = self.release.take().unwrap();
            Box::pin(async move {
                release.await.unwrap();
                let response = serde_json::from_value(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 7,
                    "result": {"final": true}
                }))
                .unwrap();
                Ok(Some(response))
            })
        }
    }

    let in_flight = openplanet_lsp::server::inflight::InFlight::new();
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let service = DelayedResponse {
        started: Some(started_tx),
        release: Some(release_rx),
    };

    // ClientSocket is only the server-to-client loopback half; this test's
    // custom service does not use it. The byte transport is real tower-lsp.
    let (_unused, socket) = LspService::new(Backend::new);
    let (mut client_input, server_input) = tokio::io::duplex(4096);
    let (server_output, mut client_output) = tokio::io::duplex(4096);
    let served_in_flight = in_flight.clone();
    let server = tokio::spawn(async move {
        openplanet_lsp::server::serve_restartable(
            server_input,
            server_output,
            socket,
            service,
            served_in_flight,
        )
        .await;
    });

    let body = r#"{"jsonrpc":"2.0","id":7,"method":"openplanet/final"}"#;
    client_input
        .write_all(format!("Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes())
        .await
        .unwrap();
    started_rx.await.unwrap();
    assert_eq!(in_flight.count(), 1, "the request is genuinely in flight");

    release_tx.send(()).unwrap();
    while !in_flight.begin_draining_if_idle() {
        tokio::task::yield_now().await;
    }

    tokio::time::timeout(DEADLINE, server)
        .await
        .expect("coordinated EOF lets tower-lsp finish")
        .expect("server task does not panic");
    drop(client_input);

    let mut bytes = Vec::new();
    client_output.read_to_end(&mut bytes).await.unwrap();
    let output = String::from_utf8(bytes).unwrap();
    assert!(
        output.contains(r#""id":7"#) && output.contains(r#""final":true"#),
        "the response completed before the fallback and was flushed before exit: {output:?}"
    );
}
