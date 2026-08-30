// B055 ground truth, live RemoteBuild probe 2026-08-30.
//
// Stronger than "invisible": the export file is not compiled into the
// defining plugin AT ALL. This plugin loads CLEAN even though the export
// file below calls a name that exists nowhere in the workspace. The same
// probe run with a deliberate SYNTAX ERROR in the export file also loaded
// clean and ran Main().
//
// Consequence for rename (B055): a rename of a strict-export declaration
// cannot leave an own-file caller broken, because the game rejects any such
// caller outright — so refusing the rename buys nothing.
//
// KNOWN LSP DIVERGENCE (pre-existing, measured on the B054 merge-base and
// unchanged by B055): openplanet-lsp reports
//     Exports_Bodies.as:3:20: error: undefined identifier `NoSuchThingAnywhere`
// where the game is clean. It checks the export file's body against the
// DEFINING plugin's module, which the game never does. Whether a consumer
// would reject the same call is a question about the consumer's module,
// which the server cannot see. Not fixed here: B055 touches only the rename
// and references handlers, whose gate is byte-identical diagnostics.
void Main() { print("x"); }
