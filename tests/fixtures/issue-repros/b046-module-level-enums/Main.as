// B046: bare value names of MODULE-LEVEL engine enums.
//
// The Nadeo dump's `ns` map is namespace -> type name -> type, so it can only
// carry an enum nested in a class. It reaches most module-level enums anyway,
// because a class member typed by one carries the enum inline (`"t":
// "ESpectatorCameraType"`), which the loader keys `Module::EName`. Two
// module-level enums have no such member anywhere in the dump, so the dump
// cannot express them at all and `Openplanet.h` is the only source:
// `ESceneCharVisNotice` and `ESceneVehicleVisEvent`.
//
// Game verdict (RemoteBuild probe, OP 1.29.5, 2026-08-30, re-probed the same
// day for B053): every statement below compiles EXCEPT the four in the
// `SharedValues_*` functions, which the game rejects as AMBIGUOUS -- see
// there. Bare value names, the bare enum name used as a type, and a value
// reached through the bare enum name are all accepted.
//
// A `Scene::` or `Enums::` prefix is NOT accepted. The game answers
// "Namespace 'Scene' doesn't exist." for both, so a module-level enum has no
// namespace to be attributed to and this fixture spells nothing that way.
// `Enums` is the bucket `Openplanet.h` files these enums under; it is a
// documentation artefact, not a namespace the game knows.
//
// One statement per line: AngelScript reports one error per statement, at the
// first unresolved symbol, so every rejected name gets its own line.

// ESceneCharVisNotice: 33 values, 30 of which no other enum carries.
void BareValues_ESceneCharVisNotice() {
    print("" + Jump);
    print("" + JumpChain1);
    print("" + JumpChain2);
    print("" + DoubleJump);
    print("" + Land);
    print("" + Spawn);
    print("" + InjuredLight);
    print("" + InjuredEliminatedShot);
    print("" + InjuredEliminated);
    print("" + Fire);
    print("" + JumpPadSmall);
    print("" + Attractor);
    print("" + GestureTaunt);
    print("" + SpeedUp);
    print("" + Evolution);
    print("" + BulletHit);
    print("" + BackFlipJump);
    print("" + FrontFlipJump);
    print("" + ZoomIn);
    print("" + ZoomOut);
    print("" + MissingStamina);
    print("" + GesturePointTo);
    print("" + JumpPadMed);
    print("" + JumpPadBig);
    print("" + TechJump);
    print("" + TeleportUnspawn);
    print("" + TeleportSpawn);
    print("" + FallDamage);
    print("" + MeleeHit);
    print("" + GestureVictory);
}

// Values ESceneCharVisNotice shares with another GLOBALLY VISIBLE enum.
//
// The divergence B046 recorded here is now CLOSED by B053: the game rejects
// each of these with "Found multiple matching enum values", and so does
// openplanet-lsp. These four statements (three here, one in the
// ESceneVehicleVisEvent block below) are the whole of this fixture's error
// count, and the live game produces exactly the same four statements --
// re-probed 2026-08-30 against this exact file, line for line with what
// openplanet-lsp reports. (No line numbers recorded on purpose: editing this
// comment moves them.)
//
// They still prove what B046 needed them to prove. The names RESOLVE: what
// they get is an ambiguity error naming the candidate enums, not `No matching
// symbol`, so the module-level enums are still in the pool and no fix that
// worked by shadowing them would pass.
void SharedValues_ESceneCharVisNotice() {
    print("" + WallJump);
    print("" + Turbo);
    print("" + FreeWheeling);
}

// ESceneVehicleVisEvent: 26 values, 25 of which no other enum carries.
void BareValues_ESceneVehicleVisEvent() {
    print("" + GlassSmash);
    print("" + Horn);
    print("" + ImpactBody);
    print("" + ImpactWheelFront);
    print("" + ImpactWheelBack);
    print("" + BeginFreeWheeling);
    print("" + EndFreeWheeling);
    print("" + AirSpawnRelease);
    print("" + PartDetached);
    print("" + ImpactDetachedPart);
    print("" + BeginNoGrip);
    print("" + EndNoGrip);
    print("" + BeginBulletTime);
    print("" + EndBulletTime);
    print("" + BeginBoost_1);
    print("" + EndBoost);
    print("" + _Unused0);
    print("" + BeginBoost_2);
    print("" + ResetHandicaps);
    print("" + TriggeredWaypoint);
    print("" + BeginFragile);
    print("" + _Unused1);
    print("" + _Unused2);
    print("" + VehicleTransform);
    print("" + WallBounce);
}

// Values ESceneVehicleVisEvent shares with another GLOBALLY VISIBLE enum.
// Closed by B053, exactly as the ESceneCharVisNotice block above.
void SharedValues_ESceneVehicleVisEvent() {
    print("" + Turbo);
}

// The enum name itself is a type the game accepts.
void AsType_ESceneCharVisNotice() {
    ESceneCharVisNotice v = ESceneCharVisNotice::Jump;
    print("" + v);
}

void AsType_ESceneVehicleVisEvent() {
    ESceneVehicleVisEvent v = ESceneVehicleVisEvent::GlassSmash;
    print("" + v);
}

// A value reached through the bare enum name.
void Qualified() {
    print("" + ESceneCharVisNotice::Jump);
    print("" + ESceneCharVisNotice::JumpChain1);
    print("" + ESceneCharVisNotice::JumpChain2);
    print("" + ESceneVehicleVisEvent::GlassSmash);
    print("" + ESceneVehicleVisEvent::Horn);
    print("" + ESceneVehicleVisEvent::Turbo);
}

void Main() {
    BareValues_ESceneCharVisNotice();
    SharedValues_ESceneCharVisNotice();
    BareValues_ESceneVehicleVisEvent();
    SharedValues_ESceneVehicleVisEvent();
    AsType_ESceneCharVisNotice();
    AsType_ESceneVehicleVisEvent();
    Qualified();
}
