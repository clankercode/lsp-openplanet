// Game-verified 2026-08-23 (spike-live-add-kinematic-ao + DipsItemExperiment):
//  1. unknown members on parentless script classes fail the game compile —
//     LSP must flag ctx.AssertSame(...) style misses (no bool overload /
//     no such member at all).
//  2. `.WithRunContext(` on an awaitable compiles clean but the game logs
//     DEPRECATED at runtime and silently discards the run context.
class SceneHandle {
    bool PayloadValid() { return true; }
}

void Main() {
    SceneHandle@ h = SceneHandle();
    h.PayloadValid();       // ok: real member
    h.AssertSame(false, false, "no such member"); // ERROR: unknown member
#if TMNEXT
    startnew(Worker).WithRunContext(Meta::RunContext::AfterMainLoop); // removed API
#endif
}

void Worker() { }
