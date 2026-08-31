// Bodies for the namespaced stubs in Exports_Decls.as. Nothing in this plugin
// calls them.
namespace Surface {
    int NsExported() { return 1; }

    // THE OVER-ROOTING GATE inside the namespace: no stub names it and it is
    // not `shared`, so it must still warn.
    void NsDeadBesideAnExport() {}
}

namespace Outer {
    namespace Inner {
        // A body two namespaces deep. Roots match by SIMPLE name (B062's
        // deliberate under-approximation), so depth must not matter.
        int DeepNsExported() { return 2; }
    }
}

// THE MEMBER GATE. Same simple name as an exported stub, but a class METHOD.
// An `import` stub names a function, never a method — B062 excluded members by
// testing `name.contains("::")`, which cannot tell a method from a namespaced
// function. The replacement tests SPAN CONTAINMENT instead, so this must still
// warn while `Surface::NsExported` above does not.
class MethodHolder {
    int NsExported() { return 3; }
}

// The same gate for an INTERFACE method. Interface methods are already exempt
// under B054 (contract declarations, reached by virtual dispatch), so this one
// is silent for that reason — pinned so the new span rule is not what is
// keeping it quiet, and so a later change that drops B054's rule shows up
// here as a NEW warning rather than silently.
interface IMethodHolder {
    int NsExported();
}
