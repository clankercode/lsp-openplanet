// `shared_exports` compiles into BOTH this plugin and consumers, so the
// pre-existing whole-FILE exemption is already right for it. Pinned here so
// the new `shared`-keyword rooting does not disturb the path-based rule, and
// so the two mechanisms are exercised in one plugin.
class FileExemptClass {
    void Method() {}
}
void FileExemptFunction() {}
