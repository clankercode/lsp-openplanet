// GH #28 namespace guards: declared workspace and typedb members stay legal.
// A complete compile surface can prove both an absent member and an absent
// namespace invalid. Incomplete dependency closures cannot prove absence.
// `DrawProbe` contrasts known removed APIs, which carry relocation hints.
// The complete workspace here declares MyHelpers but no NotDeclaredAnywhere.
//
// GH #58 diagnoses the absent member MyHelpers::NotInDbEither (line 18).
// GH #70/#71 also diagnose NotDeclaredAnywhere::Foo (line 19), because no
// missing dependency or omitted source can contribute its declaration.
// Legal known-member and relocated calls remain silent (lines 17/20/21).

namespace MyHelpers {
    void UsefulThing() {}
}

void Main() {
    MyHelpers::UsefulThing();       // line 17: declared member — silent
    MyHelpers::NotInDbEither(42);   // line 18: GH #58 — now an error
    NotDeclaredAnywhere::Foo();     // line 19: absent namespace in complete module — error
    Display::GetWidth();            // line 20: legal relocated API — silent
    UI::Begin("x");                 // line 21: known member of known ns — silent
}
