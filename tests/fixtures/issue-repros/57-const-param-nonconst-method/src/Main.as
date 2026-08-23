// Game-verified (2026-08-23, spike-live-add-kinematic-ao):
// non-const method invoked on a `const T &in` receiver fails the game
// compile: No matching signatures to 'SceneHandle::PayloadValid() const'.
class SceneHandle {
    bool PayloadValid() { return true; }
    bool ConstOk() const { return true; }
}

Json::Value HandleJson(const SceneHandle &in h) {
    return h.PayloadValid();
}

void Main() {
    SceneHandle@ s = SceneHandle();
    Json::Value@ j = HandleJson(s);
    j.DeleteAll();
}
