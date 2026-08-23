// GH #58: the namespace surface a sibling file calls into. Every kind
// declared here counts as resolution (decision 4: class, enum, funcdef,
// funcdef-typed global, plain global) — only a name that matches NOTHING
// under the namespace is the game's `No matching symbol 'Ns::Fn'`.
namespace TournamentStation {
    void Known() {}

    funcdef void Handler();
    Handler@ OnEvent = null;
    int Counter = 0;

    class Widget {
        int Value;
    }

    enum Mode {
        Idle,   // lsp: ignore unused
        Racing, // lsp: ignore unused
    }
}

// Near-name donor for the hint half of the rule (mirrors UI::GetIO).
namespace Hints {
    void Refresh() {}
}
