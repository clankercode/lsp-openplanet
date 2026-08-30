// Declared in a DIFFERENT file, and namespaced: the resolver reaches an
// unqualified `CoroutineFuncUserdataString` through the tail table, so the
// shadowing predicate has to consult that table too — an exact-name lookup
// alone leaves this one falling through to the Core funcdef.
namespace Dep {
    class CoroutineFuncUserdataString {
        int v;
    }
}
