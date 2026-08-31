// `shared_exports` compile into BOTH the defining plugin and consumers, so
// their declarations stay visible internally (fixture evidence: E++ GBX.as).
// `Payload` lives here because the handle-parameter stubs in Exports_Decls.as
// name it, and a consumer can only see a type the export surface gives it.
class SharedHelper {
    int Helper() { return 1; }
}

class Payload {
    int id;
}
