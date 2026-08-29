// B016: members whose type is an inline enum the engine dump leaves unnamed.
// Every member touched here belongs to a group that used to collapse onto one
// `<Module>::UnnamedEnum` key, so all but one of each group was dropped from
// the index:
//   Control::CControlBase::AlignHorizontal / AlignVertical  (Control's 15)
//   Control::CControlEntry::Type
//   Game::CGameCtnChallenge::Kind / CGameCtnChallengeInfo::Kind
//
// GAME GROUND TRUTH (RemoteBuild probe 2026-08-30, OP 1.29.5): every line
// down to `Main` compiles clean. The game exposes these members as plain
// integers — the enum has no name, so no source can spell it — and reading,
// writing, comparing and passing them is all legal. Keying the enums by
// their declaring context must not change that.
//
// The same probe reports THREE errors, one per line of `BareEnumValueNames`
// at the bottom; the LSP now agrees (B044) and reports one per NAME, so eight.
// Everything else must stay silent.

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
    BareEnumValueNames();
}

// B044: bare value names of those anonymous enums. The game REJECTS all of
// them — RemoteBuild probe 2026-08-30, OP 1.29.5, one error per line because
// AngelScript stops at the first bad symbol in an expression:
//     (83,16) ERR : No matching symbol 'HyperTnL'
//     (84,16) ERR : No matching symbol 'RenderPlaneReflect'
//     (85,16) ERR : No matching symbol 'MinimalReadOnly'
// An unnamed enum's values are not global constants; they are reachable only
// as integers through the declaring member, which is what everything above
// this line exercises.
//
// The LSP agrees since B044 narrowed `GlobalScope::has_global_ident`'s
// external enum-value scan to enums a source can NAME. It checks every
// identifier rather than stopping at the first, so it reports all EIGHT
// names, not three. Same verdict, finer granularity.
//
// Both classes are here on purpose. `HyperTnL`, `SlideNoControl`,
// `NotTimed`, `RenderPlaneReflect` and `Download_WaitingSources` belong to
// enums that won their module's key collision before B016 and were accepted
// long before it existed; `MinimalReadOnly`, `TimeHhMmSsMil` and `Text_Icon`
// belong to enums B016 recovered. One call site governs both.
//
// The exhaustive list — all 261 such names — is `b044-anon-enum-value-names`.
void BareEnumValueNames() {
    print("" + HyperTnL + SlideNoControl + NotTimed);
    print("" + RenderPlaneReflect + Download_WaitingSources);
    print("" + MinimalReadOnly + TimeHhMmSsMil + Text_Icon);
}
