// Minimal repro for GH #54 (false positive):
// A single-argument value-type constructor IS an implicit conversion in
// AngelScript. `mat4` registers `mat4(const mat3&in)`, so the game accepts
//   mat4::Inverse(someMat3)
// which the LSP used to reject with
//   argument 1 of `Inverse`: expected `mat4`, got `mat3`
// The relation is symmetric (`mat3(const mat4&in)`) and general — the pairs
// come from the typedb's own asBEHAVE_CONSTRUCT behaviors.
// Counterpart: `mat4` has NO ctor over `vec3`, so that call must still flag.

mat4 ItemTransform(vec3 worldPos, mat3 rot) {
    return mat4::Translate(worldPos) * mat4::Inverse(rot);   // legal: mat4(const mat3&in)
}

void F(mat3 rot, mat4 world, vec3 v) {
    mat3 back = mat3::Inverse(world);   // legal: mat3(const mat4&in)
    mat4 exact = mat4::Inverse(world);  // legal: exact match, no conversion
    mat4 bad = mat4::Inverse(v);        // GH #54 negative: no mat4(const vec3&in)
    print(tostring(back.xx) + tostring(exact.xx) + tostring(bad.xx));
    print(tostring(ItemTransform(v, rot).xx));
}

void Main() {}
