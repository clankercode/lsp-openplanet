// B028 regression fixture: the same function declared in two mutually
// exclusive arch/OS branches. The LSP used to define BOTH MANIA32+MANIA64
// (and WINDOWS+LINUX) at once, so both branches were analyzed and the second
// declaration was a phantom "A function with the same name and parameters
// already exists" error. The live game defines exactly one arch and one OS,
// compiles one branch, and this plugin is clean (verified 2026-08-29 — see
// README row).
#if MANIA32
void DupFunc() {}
#endif

#if MANIA64
void DupFunc() {}
#endif

#if WINDOWS
void DupOsFunc() {}
#endif

#if LINUX
void DupOsFunc() {}
#endif

void Main() {
    DupFunc();
    DupOsFunc();
}
