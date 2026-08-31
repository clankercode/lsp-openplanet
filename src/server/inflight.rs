//! B105: how much work the server has in flight, so a restart can wait.
//!
//! [`crate::server::binary_watch`] must only act when the server is idle, and
//! before this module there was nothing in the server that knew whether it was
//! busy — no counter, no semaphore, no flag.
//!
//! # Why a tower middleware rather than a counter in the handlers
//!
//! `Backend` has ~30 request and notification handlers. Incrementing a counter
//! at the top of each is 30 chances to forget one, and a forgotten decrement
//! (an early `return`, a `?`) pins the server "busy" forever, which silently
//! disables the restart rather than breaking a test. Both failure modes are
//! invisible.
//!
//! `LspService` is a `tower::Service`, and `Server::serve` accepts any
//! `Service<Request, Response = Option<Response>>`, so one wrapper around the
//! whole service counts *every* message with no per-handler code and no way to
//! miss one. The count is taken when the service is handed a message and
//! released when that message's future finishes **or is dropped** — a
//! cancelled request must not leave the server permanently "busy".
//!
//! # What "idle" does and does not cover
//!
//! This is deliberately a count of protocol messages, not of internal work:
//! "idle" here means the editor is not waiting on us for an answer, which is
//! the condition under which dropping the process costs nothing it cannot get
//! back.
//!
//! It does **not** cover work in tasks the server spawned for itself — the
//! background update check, the config-file reload loop and the diagnostics it
//! publishes. Restarting during one of those can lose a `publishDiagnostics`
//! the editor never asked for; the client re-opens its documents on restart
//! and gets fresh diagnostics anyway, so the cost is a moment of stale
//! squiggles rather than a dropped response. Counting those too would mean
//! defining idleness over every background task in the server, and a watcher
//! that can be starved by a periodic task is worse than one that occasionally
//! restarts a beat early.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use tower::Service;
use tower_lsp::jsonrpc::{Request, Response};

/// A shared count of messages handed to the server and not yet finished.
///
/// Cheap to clone; every clone observes the same count.
#[derive(Clone, Debug, Default)]
pub struct InFlight(Arc<AtomicUsize>);

impl InFlight {
    pub fn new() -> Self {
        Self::default()
    }

    /// Count one message as in flight until the returned guard is dropped.
    pub fn enter(&self) -> InFlightGuard {
        self.0.fetch_add(1, Ordering::AcqRel);
        InFlightGuard(Some(self.0.clone()))
    }

    /// How many messages are currently being handled.
    pub fn count(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }

    /// True when the editor is not waiting on us for anything.
    pub fn is_idle(&self) -> bool {
        self.count() == 0
    }
}

/// Releases its slot on drop — including when the future it lives in is
/// cancelled rather than completed.
#[derive(Debug)]
pub struct InFlightGuard(Option<Arc<AtomicUsize>>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if let Some(count) = self.0.take() {
            count.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Wraps the LSP service so every message is counted. Transparent otherwise:
/// requests, responses and errors pass through untouched.
#[derive(Debug)]
pub struct CountedService<S> {
    inner: S,
    in_flight: InFlight,
}

impl<S> CountedService<S> {
    pub fn new(in_flight: InFlight, inner: S) -> Self {
        Self { inner, in_flight }
    }
}

impl<S> Service<Request> for CountedService<S>
where
    S: Service<Request, Response = Option<Response>>,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = Option<Response>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        // Taken BEFORE the inner call so a message is counted for the whole
        // time the server owns it, and moved into the future so it is released
        // by completion or cancellation alike.
        let guard = self.in_flight.enter();
        let call = self.inner.call(req);
        Box::pin(async move {
            let _guard = guard;
            call.await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;

    #[test]
    fn a_fresh_counter_is_idle() {
        let in_flight = InFlight::new();
        assert_eq!(in_flight.count(), 0);
        assert!(in_flight.is_idle());
    }

    #[test]
    fn a_guard_marks_the_server_busy_until_it_drops() {
        let in_flight = InFlight::new();
        let guard = in_flight.enter();
        assert_eq!(in_flight.count(), 1);
        assert!(!in_flight.is_idle(), "a message in flight is not idle");
        drop(guard);
        assert!(in_flight.is_idle(), "finishing the message returns to idle");
    }

    #[test]
    fn every_concurrent_message_has_to_finish() {
        let in_flight = InFlight::new();
        let guards: Vec<_> = (0..8).map(|_| in_flight.enter()).collect();
        assert_eq!(in_flight.count(), 8);

        let mut guards = guards;
        for remaining in (0..8).rev() {
            guards.pop();
            assert_eq!(in_flight.count(), remaining);
            assert_eq!(in_flight.is_idle(), remaining == 0);
        }
    }

    #[test]
    fn clones_share_one_count() {
        let in_flight = InFlight::new();
        let observer = in_flight.clone();
        let _guard = in_flight.enter();
        assert_eq!(
            observer.count(),
            1,
            "the watcher holds a clone, not the original"
        );
    }

    /// A service whose future never completes, so a message can be held "in
    /// flight" for as long as the test likes.
    #[derive(Clone)]
    struct PendingService;

    impl Service<Request> for PendingService {
        type Response = Option<Response>;
        type Error = Infallible;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request) -> Self::Future {
            Box::pin(std::future::pending())
        }
    }

    fn a_request() -> Request {
        Request::build("shutdown").id(1).finish()
    }

    #[tokio::test]
    async fn the_service_counts_a_message_while_it_is_being_handled() {
        let in_flight = InFlight::new();
        let mut service = CountedService::new(in_flight.clone(), PendingService);
        assert!(in_flight.is_idle());

        let call = service.call(a_request());
        assert_eq!(
            in_flight.count(),
            1,
            "the count is taken when the service is handed the message, \
             not when its future is first polled"
        );

        let second = service.call(a_request());
        assert_eq!(in_flight.count(), 2);

        drop(call);
        assert_eq!(in_flight.count(), 1);
        drop(second);
        assert!(in_flight.is_idle());
    }

    #[tokio::test]
    async fn a_cancelled_request_does_not_pin_the_server_busy() {
        // The failure this guards: a dropped (cancelled) request leaking its
        // slot would leave `is_idle()` false forever, which does not break any
        // feature — it silently disables the restart. Exactly the shape of bug
        // that never gets noticed.
        let in_flight = InFlight::new();
        let mut service = CountedService::new(in_flight.clone(), PendingService);
        for _ in 0..100 {
            drop(service.call(a_request()));
        }
        assert!(in_flight.is_idle(), "cancelled requests release their slot");
    }

    #[tokio::test]
    async fn a_completed_request_releases_its_slot() {
        #[derive(Clone)]
        struct ReadyService;
        impl Service<Request> for ReadyService {
            type Response = Option<Response>;
            type Error = Infallible;
            type Future =
                Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
            fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }
            fn call(&mut self, _req: Request) -> Self::Future {
                Box::pin(async { Ok(None) })
            }
        }

        let in_flight = InFlight::new();
        let mut service = CountedService::new(in_flight.clone(), ReadyService);
        for _ in 0..25 {
            assert_eq!(service.call(a_request()).await.unwrap(), None);
            assert!(in_flight.is_idle());
        }
    }
}
