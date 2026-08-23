// GH #58: the workspace `No matching symbol` rule must not need a typedb to
// fire on user namespaces, and must never fire on an ENGINE namespace — a
// plugin may add helpers to `namespace UI` and still call the real API, which
// only the typedb knows about. This fixture is checked WITHOUT a typedb.
namespace UI {
    void MyHelper() {}
}

namespace Local {
    void Real() {}
}

void Main() {
    UI::MyHelper(); // 14: clean — workspace addition to an engine namespace
    UI::Begin("x"); // 15: SILENT — engine API, no typedb loaded to confirm
    Local::Real();  // 16: clean
    Local::Nope();  // 17: ERROR — user namespace, member matches nothing
}
