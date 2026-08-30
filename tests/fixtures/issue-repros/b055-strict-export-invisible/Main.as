// B055 ground truth, live RemoteBuild probe 2026-08-30.
//
// A file listed in `[script] exports` compiles into CONSUMERS only. Calling
// one of its declarations from this plugin's own module is a COMPILE ERROR:
//
//     ERR: No matching symbol 'SendEvent'   (on the Main() line below)
//
// Probed with and without `module = "T"` — the verdict is identical, so it
// is `exports` alone that hides the file, not the strict-module key. The
// control (a strict module with no `exports` at all) compiles clean, and so
// does the same shape written with `shared_exports`.
void Main() { SendEvent(); }
