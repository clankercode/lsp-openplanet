// B056, second half: the plugin's OWN names, and a name reached through a
// CLASS. The false-POSITIVE direction, which is the damaging one for a tool
// that flags user code.
//
// Both names here were found by auditing the shipping plugins in
// `tests/fixtures/tm-*` for every one of the 436 bare names the narrowing
// rejects. Eleven of those names occur in that source; nine are string
// literals, member accesses or a loop variable. These two are real, and each
// exercises a different lookup that must answer BEFORE the narrowing.
//
//   Axis         `tm-editor-plus-plus/src/Components/Cursor/Gizmo.as:1`
//                declares `enum Axis`, and 67 uses resolve to it. The engine
//                also has `UI::Plot::Axis`, which the game rejects bare
//                (`Identifier 'Axis' is not a data type in global namespace`).
//                The WORKSPACE declaration must win.
//
//   WindowFlags  `tm-archivist/src/Tabs/Tab.as:82` and
//                `tm-editor-plus-plus/src/UI/UI_Tab.as:265` both write
//                `UI::Begin(name, open, WindowFlags)`, and 13 files declare
//                `int get_WindowFlags()`. The engine has `UI::WindowFlags`,
//                rejected bare. The CLASS MEMBER must win.
//
// The WindowFlags case is worth being precise about, because if it ever goes
// red the bug will not be where a reader expects. The corpus verdict is about
// TYPE position (`is not a data type`); this use is a VALUE read. B056
// narrows type-position resolution only, so a correctly scoped narrowing
// never sees it at all. A failure here means the change LEAKED into value
// position -- it does not mean the rule is wrong.
//
// Game ground truth: RemoteBuild, Trackmania + Openplanet 1.29.5,
// 2026-08-30. This file compiled with zero diagnostics.

// The plugin's own enum, colliding with `UI::Plot::Axis`.
enum Axis
{
    X,
    Y,
    Z,
}

// The plugin's own class, colliding with `UI::Cond` on a member and with
// `UI::WindowFlags` on a virtual property.
class Tab
{
    int get_WindowFlags()
    {
        return 0;
    }

    Axis PreferredAxis()
    {
        return Axis::Y;
    }

    // Reference the remaining values so the fixture asserts a clean run and
    // no unused-enum-value warning masks a real change.
    bool IsHorizontal(Axis a)
    {
        return a == Axis::X || a == Axis::Z;
    }

    void Render()
    {
        // A bare read of the plugin's own virtual property, on a name the
        // engine also has and the game rejects bare.
        print("" + WindowFlags);
    }
}

// A bare TYPE reference to the plugin's own enum, in every position that
// matters: local, parameter, return.
Axis AxisOf(Axis a)
{
    Axis local = a;
    return local;
}

void Main()
{
    Tab t;
    t.Render();
    AxisOf(t.PreferredAxis());
    print("" + t.IsHorizontal(Axis::X));
}
