// B060 variant matrix (repo CLAUDE.md: generalize the case). Main.as pins the
// reported shape; this file sweeps the axes that shape could vary along, so a
// fix that only handles the reported spelling shows up here.
//
//   value type      uint64 · string · bool · vec3 (const T&in slot) · uint16
//   receiver form   bare ident · implicit `this` · inherited from a parent ·
//                   member on a handle · nested member · factory-call result ·
//                   array index · namespace-qualified · namespaced class
//   getter flavour  plain · `const`
//
// Master reports 14 errors here, every one of them false. The four REJECT
// lines at the bottom put a real function in each slot shape used above — if
// a fix silences those too it has stopped distinguishing a function from a
// value, which is the whole point.

uint64 get_PtrP() { return 8; }
string get_NameP() { return "n"; }
bool get_FracP() { return true; }
vec3 get_VecP() { return vec3(); }
uint16 get_ShortP() { return 2; }

class VInner {
    uint64 get_PtrP() { return 8; }
}

class VBase {
    uint64 get_BasePtr() { return 8; }
}

class VKid : VBase {
    VInner@ inner;
    VKid() { @inner = VInner(); }
    uint64 get_OwnPtr() const { return 9; }
    VInner@ Get() { return inner; }
    void Inside() {
        Dev::Write(OwnPtr, 1);  // implicit `this`, const getter
        Dev::Write(BasePtr, 1); // implicit `this`, getter inherited from VBase
    }
}

namespace VNs {
    uint64 get_PtrP() { return 8; }
    class D {
        uint64 get_PtrP() { return 8; }
    }
}

void VRealFn() {}

void VUse() {
    VKid@ k = VKid();
    VNs::D@ d = VNs::D();
    array<VInner@> arr = { VInner() };

    // value types: each is a different slot shape in Dev::Write's 20-overload
    // set, plus a two-overload set and a bool-carrying set
    Dev::Write(PtrP, 1);           // uint64
    Dev::Write(1, NameP);          // const string&in
    Dev::Write(1, VecP);           // const vec3&in
    Dev::Write(1, ShortP);         // uint16
    Time::Format(PtrP, FracP);     // uint64 + bool
    Reflection::GetType(NameP);    // const string&in, 2-overload set

    // receiver forms
    Dev::Write(k.OwnPtr, 1);       // member on a handle, const getter
    Dev::Write(k.BasePtr, 1);      // member, inherited from the parent class
    Dev::Write(k.inner.PtrP, 1);   // nested member
    Dev::Write(k.Get().PtrP, 1);   // through a factory call
    Dev::Write(arr[0].PtrP, 1);    // through an array index
    Dev::Write(VNs::PtrP, 1);      // namespace-qualified
    Dev::Write(d.PtrP, 1);         // getter on a namespaced class

    // REJECT: a real function in each of the slot shapes swept above
    Dev::Write(VRealFn, 1);
    Dev::Write(1, VRealFn);
    Reflection::GetType(VRealFn);
    Time::Format(VRealFn, true);
}
