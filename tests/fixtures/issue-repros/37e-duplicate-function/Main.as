// Minimal repro for GH #37 (duplicate function definition parity).
// Game: second decl with same name + param types ->
//       ERR A function with the same name and parameters already exists.
// Overloads (different param types) are legal.

void DupFn(int a) {}
void DupFn(int a) {}      // must error
void DupFn(string s) {}   // overload — silent

void Main() {}
