// GH #58 false-positive fences for the workspace `No matching symbol` rule:
//   * AngelScript resolves a namespace-qualified call RELATIVE to the
//     enclosing namespace first, so `Inner::Deep()` inside `Outer` binds
//     `Outer::Inner::Deep` even though a top-level `Inner` also exists.
//   * Only single-segment namespaces are authoritative in this release —
//     deeper paths (`Outer::Inner::Nope`) stay silent.
namespace Outer {
    namespace Inner {
        void Deep() {}
    }
}

namespace Inner {
    void Shallow() {}
}

namespace Outer {
    void Use() {
        Inner::Deep();    // 19: SILENT — relative hit on Outer::Inner::Deep
        Inner::Shallow(); // 20: clean — absolute hit
    }
}

void Main() {
    Outer::Use();          // 25: clean
    Outer::Inner::Deep();  // 26: clean
    Outer::Inner::Nope();  // 27: SILENT — nested path, not authoritative yet
    Inner::Nope();         // 28: ERROR — top-level Inner has no Nope
}
