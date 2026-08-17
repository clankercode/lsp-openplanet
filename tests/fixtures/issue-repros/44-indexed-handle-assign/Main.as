// Minimal repro for GH #44 (fixed 70f3f97):
// `@arr[0] = tiny` assigns a handle into the *indexed result* of a
// Json::Value opCall. The AS compiler rejects this:
//     ERR : Expression is not an l-value
// The value-copy form (`arr[0] = tiny`, no `@`) is the legal counterpart
// and must NOT diagnose.

namespace LspProbe {
    void IndexedAssign(Json::Value@ arr) {
        Json::Value@ tiny = Json::Object();
        @arr[0] = tiny; // GH #44: game rejects; LSP must flag
    }

    void IndexedValueAssignIsLegal(Json::Value@ arr) {
        Json::Value@ tiny = Json::Object();
        arr[0] = tiny; // legal value-copy assign — must stay silent
    }
}

void Main() {}
