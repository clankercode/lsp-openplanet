// B028 ground-truth probe #1: OS/arch defines in the live game (Openplanet
// under Wine). One probe per flag: the expected-live branch is legal
// AngelScript, the expected-dead branch holds an undeclared identifier.
// CLEAN in-game compile ⇒ every expectation held; an error naming a trap
// line ⇒ that flag's expectation is wrong (the flag is the opposite).
// Expectations (Max, verified 2026-08-29 — see README row): WINDOWS on,
// WINDOWS_WINE on, LINUX off, MANIA64 on, MANIA32 off.
#if WINDOWS
void ProbeWindows() {}
#else
void ProbeWindows() { int trap = WINDOWS_NOT_DEFINED_B028; }
#endif

#if WINDOWS_WINE
void ProbeWine() {}
#else
void ProbeWine() { int trap = WINDOWS_WINE_NOT_DEFINED_B028; }
#endif

#if LINUX
void ProbeLinux() { int trap = LINUX_DEFINED_B028; }
#else
void ProbeLinux() {}
#endif

#if MANIA64
void ProbeArch64() {}
#else
void ProbeArch64() { int trap = MANIA64_NOT_DEFINED_B028; }
#endif

#if MANIA32
void ProbeArch32() { int trap = MANIA32_DEFINED_B028; }
#else
void ProbeArch32() {}
#endif

void Main() {
    ProbeWindows();
    ProbeWine();
    ProbeLinux();
    ProbeArch64();
    ProbeArch32();
}
