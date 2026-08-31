// `shared_exports` compile into BOTH this plugin and consumers, so their
// declarations are the real thing and the pre-existing whole-file exemption
// is already correct for them. Pinned so the new rooting does not disturb it.
class SharedHelper {
    int Helper() { return 1; }
}
