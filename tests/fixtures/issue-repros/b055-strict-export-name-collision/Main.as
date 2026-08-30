// B055 ground truth, live RemoteBuild probe 2026-08-30: CLEAN.
//
// `SendEvent` is declared on BOTH sides of the export boundary and the game
// accepts it — they are different symbols in different modules. So rename
// and find-references must never cross that boundary: reporting the export
// file's declaration as an occurrence of this one is exactly the
// cross-module false positive B049 exists to remove, pointed inward.
//
// KNOWN LSP DIVERGENCE (pre-existing, measured on the B054 merge-base and
// unchanged by B055): openplanet-lsp warns
//     Main.as: warning: a function `SendEvent` is never used
// on the declaration below, even though Main() calls it on the next line.
// The two same-named declarations are pooled together and the usage join
// cannot tell them apart. Not fixed here, for the same reason as above.
void SendEvent() { print("own"); }
void Main() { SendEvent(); }
