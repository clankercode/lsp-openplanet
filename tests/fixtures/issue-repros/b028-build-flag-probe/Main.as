// B028 ground-truth probe #2: build-shape defines in the live game (a normal
// player's client under Wine). Expected all OFF for a client build (docs:
// openplanet.dev/docs/reference/preprocessor): SERVER = dedicated server
// build, LOGS = Logs-configured build, HAS_DEV = Nadeo development build,
// DEVELOPER = debug build. Each trap sits under `#if FLAG`; a clean in-game
// compile means all four are undefined in the live client.
#if SERVER
void ProbeServer() { int trap = SERVER_DEFINED_B028; }
#else
void ProbeServer() {}
#endif

#if LOGS
void ProbeLogs() { int trap = LOGS_DEFINED_B028; }
#else
void ProbeLogs() {}
#endif

#if HAS_DEV
void ProbeHasDev() { int trap = HAS_DEV_DEFINED_B028; }
#else
void ProbeHasDev() {}
#endif

#if DEVELOPER
void ProbeDeveloper() { int trap = DEVELOPER_DEFINED_B028; }
#else
void ProbeDeveloper() {}
#endif

void Main() {
    ProbeServer();
    ProbeLogs();
    ProbeHasDev();
    ProbeDeveloper();
}
