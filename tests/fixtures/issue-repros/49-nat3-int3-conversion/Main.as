// Minimal repro for GH #49 (false negative):
// AngelScript has no implicit nat3 -> int3 conversion. The game rejects
//   int3 size = map.Size;            // ERR: Can't implicitly convert from 'const nat3' to 'int3'
//   int3 also = int3(map.Size);      // ERR: No matching signatures to 'int3(const nat3&)'
// The legal form is explicit member-wise construction from components.
// Legal counterparts (int3 from three ints; nat3 var from nat3 member) stay silent.

void F(CGameCtnChallenge@ map) {
    int3 size = map.Size;            // GH #49: game ERR, LSP must flag
    nat3 asNat = map.Size;           // legal: exact nat3 copy
    int3 ok = int3(1, 2, 3);         // legal: three-int ctor
    int3 also = int3(map.Size);      // GH #49: game ERR (ctor form), LSP must flag
    int x = size.x;                  // legal member use (keeps size live)
    print(tostring(x) + tostring(asNat.x) + tostring(ok.x) + tostring(also.x));
}

void Main() {}
