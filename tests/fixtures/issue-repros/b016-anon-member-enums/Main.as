// B016: members whose type is an inline enum the engine dump leaves unnamed.
// Every member touched here belongs to a group that used to collapse onto one
// `<Module>::UnnamedEnum` key, so all but one of each group was dropped from
// the index:
//   Control::CControlBase::AlignHorizontal / AlignVertical  (Control's 15)
//   Control::CControlEntry::Type
//   Game::CGameCtnChallenge::Kind / CGameCtnChallengeInfo::Kind
//
// The game exposes these as plain integers — the enum has no name, so no
// source can spell it — and every line below compiles in-game. Keying the
// enums by their declaring context must not make the checker reject any of
// them: nothing here may produce a diagnostic.

void Read(CControlBase@ ctrl, CControlEntry@ entry, CGameCtnChallenge@ map) {
    int h = ctrl.AlignHorizontal;             // read into an int
    auto v = ctrl.AlignVertical;              // read into an auto
    auto kind = map.Kind;                     // a different collision group
    auto entryType = entry.Type;              // member name shared with Json::Type
    print("" + h + v + kind + entryType);
}

void Compare(CControlBase@ a, CControlBase@ b, CGameCtnChallenge@ map) {
    bool sameFamily = a.AlignHorizontal == b.AlignHorizontal;   // same enum
    bool crossFamily = a.AlignHorizontal == a.AlignVertical;    // two ex-collided enums
    bool crossClass = map.Kind == map.MapInfo.Kind;             // same name, two classes
    bool literal = a.AlignVertical == 2;                        // enum vs int literal
    print("" + sameFamily + crossFamily + crossClass + literal);
}

void Write(CControlBase@ ctrl, CControlEntry@ entry) {
    ctrl.AlignHorizontal = 0;                 // literal into the member
    ctrl.AlignVertical = ctrl.AlignHorizontal;// across two ex-collided enums
    entry.Type = 3;
}

int Widen(int value) {
    return value + 1;
}

void PassAsArgument(CControlBase@ ctrl, CGameCtnChallenge@ map) {
    print("" + Widen(ctrl.AlignHorizontal)); // user fn taking int
    print(tostring(map.Kind));               // typedb `?` slot
}

void Main() {
    Read(null, null, null);
    Compare(null, null, null);
    Write(null, null);
    PassAsArgument(null, null);
}
