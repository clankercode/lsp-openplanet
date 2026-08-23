// shared_exports compile into BOTH the defining plugin and consumers —
// decls here stay visible internally (fixture evidence: E++ GBX.as).
class SharedHelper {
    int Helper() { return 1; }
}
