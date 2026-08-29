// B053: a bare enum value carried by two globally visible enums.
//
// The game resolves a bare enum-value name against the enums in scope. When
// two of them carry it, it refuses to choose:
//
//     Found multiple matching enum values
//
// openplanet-lsp took the first match and said nothing -- a false negative,
// invisible until the plugin is compiled in-game.
//
// WHICH enums are in scope for a bare name is the whole question, and it is
// much narrower than "every named enum". Only the ones the game puts in the
// GLOBAL namespace take part: the `Openplanet.h` module-level enums, the
// Nadeo dump's module-level enums (whose `Game::`/`Scene::` prefix is this
// crate's own key, not a namespace), and Core enums with no namespace. That
// is 68 keys and 60 distinct NAMES -- distinct names, because the loader
// keys one engine enum under several modules and `EHmsLightMapQuality`
// appearing twice is still one enum.
//
// Ground truth: all 2989 bare value names of the shipped type DB's named
// enums were submitted to the live game (RemoteBuild, Trackmania +
// Openplanet 1.29.5, 2026-08-30), one statement per line:
//
//     2373  No matching symbol
//      592  accepted
//       22  Found multiple matching enum values
//        2  resolved to a non-enum symbol (`Main`, `RGBAColor`)
//
// The rule above predicts 2987 of those 2989 verdicts. The ticket's own rule
// -- "carried by two or more NAMED enums" -- predicts 758 names, 34x too
// many, because it counts nested and namespaced enums whose values are not
// reachable bare at all.
//
// The 2373 are a real and much larger false negative in the same family, and
// they are deliberately NOT addressed here: narrowing the bare-value scan
// needs its own probe and its own false-positive gate. B053 is the ambiguity
// half only, and `Unambiguous()` below is silent on both sides today.
//
// How a plugin's OWN declarations join or outrank this pool is a separate
// fixture, `b053-ambiguous-enum-values-workspace`, because those declarations
// would perturb the counts here.

// ---------------------------------------------------------------------------
// AMBIGUOUS -- all 22 names the game rejects with "Found multiple matching
// enum values", with no residual. Each must be diagnosed, and the message
// must name the candidate enums so that qualifying it is the obvious fix.
// ---------------------------------------------------------------------------
void Ambiguous() {
    print("" + Blend);   // EPlugAnimGraphNodeType, EPlugStateMachineTransitionType
    print("" + Cling);   // EPlugAnimClipAlignOffset, EPlugAnimRootYawType
    print("" + Down);   // ESceneVehicleVisReactorBoostType, VirtualKey
    print("" + Energy);   // EPlugParticleEmitterSubModel, EPlugSurfaceMaterialId
    print("" + FreeWheeling);   // EPlugSurfaceGameplayId, ESceneCharVisNotice
    print("" + Heavy);   // EMeleeHitType, EParryType
    print("" + High);   // EHmsLightMapQuality, EPlugRoadDensity
    print("" + Light);   // EMeleeHitType, EParryType
    print("" + Medium);   // EMeleeHitType, EParryType, EPlugRoadDensity
    print("" + Mesh);   // EGmSurfType, EPlugParticleStandardRenderMode
    print("" + None);   // EGameItemWaypointType, EGhostNameLogoType, EHmsLightMapBump, EHmsOverlayAdaptRatio, EMeleeHitType, EMeleeState, EParryType, EPlugAnimClipAlignOffset, EPlugDynaSleepingMethod, EPlugParticleSpawnCond, EPlugParticleTextureAtlas, EPlugSurfaceGameplayId, ESceneVehicleVisReactorBoostLvl, ESceneVehicleVisReactorBoostType, VirtualKey
    print("" + Player);   // EAudioBalanceGroup, EPlugSurfaceMaterialId
    print("" + Road);   // EPlugRoadChunkType, EPlugRoadType
    print("" + Static);   // EImportMeshType, EPlugModelShadingType
    print("" + Turbo);   // EPlugSurfaceGameplayId, ESceneCharVisNotice, ESceneVehicleVisEvent
    print("" + Undefined);   // EGameCollectorSearchScope, EPlugRoadType
    print("" + Up);   // ESceneVehicleVisReactorBoostType, VirtualKey
    print("" + Vehicle);   // EImportMeshType, EPlugModelShadingType
    print("" + WallJump);   // EPlugSurfaceMaterialId, ESceneCharVisNotice
    print("" + Water);   // EPlugAnimClipAlignOffset, EPlugSurfaceMaterialId
    print("" + WaterSplash);   // EPlugParticleMultiStateRenderMode, EPlugParticleStandardRenderMode
    print("" + XXX_Null);   // EPlugSurfaceGameplayId, EPlugSurfaceMaterialId
}

// ---------------------------------------------------------------------------
// UNAMBIGUOUS -- carried by exactly ONE globally visible enum. The game
// compiles every one of these, and so must openplanet-lsp: this is the fence
// that keeps the ambiguity rule from becoming a false positive.
// ---------------------------------------------------------------------------
void Unambiguous() {
    print("" + A);   // VirtualKey
    print("" + Accept);   // VirtualKey
    print("" + Action1IsEvent);   // EPlugAnimGraphStateSelectCond
    print("" + Action2Is);   // EPlugAnimGraphStateSelectCond
    print("" + Action2IsEvent);   // EPlugAnimGraphStateSelectCond
    print("" + Action3Is);   // EPlugAnimGraphStateSelectCond
    print("" + ActionIs);   // EPlugAnimGraphStateSelectCond
    print("" + Active);   // EPlugParticleSpawnCond
    print("" + Active_FixedDist);   // EPlugParticleSpawnCond
    print("" + Active_FixedPeriod);   // EPlugParticleSpawnCond
    print("" + Active_MinDist);   // EPlugParticleSpawnCond
    print("" + Add);   // VirtualKey
    print("" + Additive);   // EPlugAnimGraphBlending
    print("" + Aim);   // EPlugAnimRootYawType
    print("" + AimHysteresis);   // EPlugAnimRootYawType
    print("" + AirSpawnRelease);   // ESceneVehicleVisEvent
    print("" + AirTrajectoryPrediction);   // EPlugAnimGraphNodeType
    print("" + Ambiance);   // EAudioBalanceGroup
    print("" + AnimatedRandom);   // EPlugParticleTextureAtlas
    print("" + AnimatedSynchro);   // EPlugParticleTextureAtlas
    print("" + Apps);   // VirtualKey
    print("" + Armor);   // EGameInventoryItemClass
    print("" + Asphalt);   // EPlugSurfaceMaterialId
    print("" + AssertVar);   // EPlugAnimGraphNodeType
    print("" + Attn);   // VirtualKey
    print("" + Attractor);   // ESceneCharVisNotice
    print("" + Auto);   // EAudioBalanceGroup
    print("" + AvatarPoseEditor);   // EPlugAnimGraphNodeType
    print("" + AvatarV0);   // EPlugAnimGraphNodeType
    print("" + AvatarV3_Global);   // EPlugAnimGraphNodeType
    print("" + AvatarV3_Idle);   // EPlugAnimGraphNodeType
    print("" + AvatarV3_Jump);   // EPlugAnimGraphNodeType
    print("" + AvatarV3_Locomotion);   // EPlugAnimGraphNodeType
    print("" + AvatarV3_Resting);   // EPlugAnimGraphNodeType
    print("" + AvatarV3_Seated);   // EPlugAnimGraphNodeType
    print("" + AvatarV3_Swim);   // EPlugAnimGraphNodeType
    print("" + Avatar_Climb);   // EPlugAnimGraphNodeType
    print("" + B);   // VirtualKey
    print("" + Back);   // VirtualKey
    print("" + BackFlipJump);   // ESceneCharVisNotice
}

// ---------------------------------------------------------------------------
// QUALIFYING IT IS THE FIX -- the game accepts each of these, and they are
// what the diagnostic tells the user to write.
// ---------------------------------------------------------------------------
void Qualified() {
    print("" + ESceneCharVisNotice::Turbo);
    print("" + ESceneVehicleVisEvent::Turbo);
    print("" + ESceneCharVisNotice::WallJump);
    print("" + ESceneCharVisNotice::FreeWheeling);
}

void Main() {
    Ambiguous();
    Unambiguous();
    Qualified();
}
