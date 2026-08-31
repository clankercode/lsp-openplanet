// The body the strict stub in Exports_Decls.as binds to. Nothing in this
// plugin calls it (consumers bind by signature), so before B062 it warned.
bool IsConnected() { return true; }

// THE OVER-ROOTING GATE for the export half, in the SAME FILE as the rooted
// body: no stub names it and it is not `shared`, so it must still warn.
void DeadBesideAnExport() {}
