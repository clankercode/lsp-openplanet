// Negative guard for the GH #28 No-matching-symbol rule: a namespace the
// checker has NO authoritative surface for must stay silent — it may be a
// typedb gap or a namespace another module contributes to.
// `DrawProbe` shows the contrast: Draw is in the removed-API table (known
// game namespace, absent from 1.29.5 typedb) so it DOES diagnose.
//
// GH #58 narrowed this: a namespace DECLARED in the workspace *is*
// authoritative once the dependency closure is complete, so an unknown
// member of `MyHelpers` is now an error (line 18). Only namespaces absent
// from both the workspace and the typedb keep the old silence (line 19).

namespace MyHelpers {
    void UsefulThing() {}
}

void Main() {
    MyHelpers::UsefulThing();       // line 17: declared member — silent
    MyHelpers::NotInDbEither(42);   // line 18: GH #58 — now an error
    NotDeclaredAnywhere::Foo();     // line 19: unknown namespace — silent
    Display::GetWidth();            // line 20: legal relocated API — silent
    UI::Begin("x");                 // line 21: known member of known ns — silent
}
