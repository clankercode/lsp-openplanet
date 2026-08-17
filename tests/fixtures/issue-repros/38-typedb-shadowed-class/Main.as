// Minimal repro for GH #38 (fixed db6e46b, needs typedb):
// A workspace class whose name collides with an engine typedb short name
// (here `Status`, which exists as `Discord::Status` in OpenplanetCore.json)
// used to resolve to the engine type, so every member lookup failed:
//     error: type `Status` has no member `Set`
// The game compiler prefers the plugin's own declaration — workspace types
// must shadow typedb short names. Requires --typedb-dir (gate test passes it).

namespace Repro {
    enum StatusKind { A, B }

    class Status {
        void Set(StatusKind k) { m2 = k; }
        StatusKind get_Kind() const property { return m2; }
        private StatusKind m2 = StatusKind::A;
    }

    Status g_Status;

    void Use() {
        g_Status.Set(StatusKind::B);  // GH #38: must stay silent
        StatusKind b = g_Status.Kind; // GH #38: must stay silent
    }
}

void Main() {}
