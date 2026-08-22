// Minimal repro for GH #53: source-tree `info.toml` often leaves DEV /
// UNIT_TEST out of `defines` (the build script injects them only into the
// staged copy). `openplanet-lsp check` then skips `#if DEV` files
// entirely, so errors that WILL fail the staged dev build stay hidden.
// `check --define DEV` must add the define so the gate matches the
// staged compile.
// The undefined identifier below must surface ONLY when DEV is defined
// (via --define); without it, the branch stays dead and check is clean.

#if DEV
void DevOnlyBroken() {
    int x = CollectTerrainBlockIndices_MISSING();  // undefined when DEV on
}
#endif

void Main() {}
