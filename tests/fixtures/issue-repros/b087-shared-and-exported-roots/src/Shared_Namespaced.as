// `shared` inside a namespace: the exemption keys on the DECLARATION, and
// the pooled name carries the namespace.
namespace Surface {
    shared class NsSharedClass {
        void NsMethod() {}
    }

    shared void NsSharedFunction() {}

    // Negative control in the same namespace.
    class NsPlainClass {
        void NsPlainMethod() {}
    }
}
