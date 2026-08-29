// B044: bare value names of ANONYMOUS (inline member) engine enums.
//
// The engine dump declares these enums inline on a member and gives them no
// name. Nothing in source can spell such an enum, so nothing in source can
// reach its values by a bare name — the game answers `No matching symbol`.
// Reaching the same values through the declaring member's type stays legal;
// see `b016-anon-member-enums` for that half of the rule.
//
// GENERATED — do not hand-edit. Regenerate with `scripts/gen_b044_probe.py`
// after refreshing `tests/fixtures/typedb`.
//
// One statement per line: AngelScript reports one error per statement, at the
// first unresolved symbol, so every rejected name gets its own line number.
//
// The list is every value name of the 132 anonymous member enums in the
// fixture type DB, minus the 126 that also appear in a named enum (a bare
// reference to those resolves legitimately, e.g. `VCenter2`). 261 names.
//
// Game verdict (RemoteBuild probe, OP 1.29.5, 2026-08-30): 258 rejected,
// 3 accepted. The 3 are marked `KNOWN FP` below — each is a value of the
// MODULE-LEVEL enum `Scene::ESceneCharVisNotice`, which the Nadeo dump cannot
// represent (its `ns` map holds types only), so the LSP has never known those
// 47 module-level enums. 52 of their value names already fail to resolve; these
// 3 were resolving only by accident, through the scan B044 narrows. Fixing that
// gap is its own ticket, and when it lands these 3 lines go quiet.

void Probe() {
    print("" + API_Calls);
    print("" + AVI);
    print("" + Anaglyph);
    print("" + Ascii7bit);
    print("" + Average);
    print("" + BackFlip);
    print("" + BackFlipGliding);
    print("" + BottomToTop);
    print("" + Bottom_Top);
    print("" + CBR);
    print("" + CQ);
    print("" + CameraFx);
    print("" + CameraMap);
    print("" + Campaign);
    print("" + CastAndReceive);
    print("" + CatmullRom);
    print("" + Child);
    print("" + ChildToParent);
    print("" + Class);
    print("" + Clear_Fog);
    print("" + ClimbOver);
    print("" + Color_0);
    print("" + Color_1);
    print("" + Color_2);
    print("" + Color_3);
    print("" + Color_4);
    print("" + Color_5);
    print("" + Color_6);
    print("" + Color_7);
    print("" + Compute);
    print("" + Config);
    print("" + CoreCmd);
    print("" + Cubemap);
    print("" + Current_Camera_s_FarZ);
    print("" + DXT1);
    print("" + DXT2);
    print("" + DXT3);
    print("" + DXT4);
    print("" + DXT5);
    print("" + DayTimeRemapInU);
    print("" + DbgBlend);
    print("" + Deactivated);
    print("" + DeactivatedShot);
    print("" + DepthAsAlpha);
    print("" + Depth_DofBlur);
    print("" + Depth_Motion_MotionLen);
    print("" + Deutranopia);
    print("" + Direct_w__attenuation);
    print("" + Directional);
    print("" + DoubleJump);  // KNOWN FP: the game accepts this — Scene::ESceneCharVisNotice value
    print("" + Download_Done);
    print("" + Download_OnTermination);
    print("" + Download_ProcessingSources);
    print("" + Download_WaitingSources);
    print("" + Download_WaitingTermination);
    print("" + Dynamic);
    print("" + Editable);
    print("" + EndMarker);
    print("" + Equirectangular);
    print("" + Explicit);
    print("" + FaceAverage);
    print("" + FaceNormalInC);
    print("" + FallBlocked);
    print("" + FastMove);
    print("" + Flock);
    print("" + Fog_Clear);
    print("" + ForceFarZ);
    print("" + ForceHard3d);
    print("" + ForceHide);
    print("" + ForceShow);
    print("" + From_Club);
    print("" + FrontFlip);
    print("" + FrontFlipGliding);
    print("" + Gaussian);
    print("" + GlowInput);
    print("" + GrayNGFull);
    print("" + GrayNGHalf);
    print("" + GrayedOut);
    print("" + Half);
    print("" + HdrRadius);
    print("" + Hexa);
    print("" + HmsItem);
    print("" + HueSlider);
    print("" + HyperNorm2);
    print("" + HyperTnL);
    print("" + Icon);
    print("" + IconPose);
    print("" + Id);
    print("" + Image_space);
    print("" + Implicit);
    print("" + InProgress);
    print("" + In_Map);
    print("" + Included);
    print("" + Int2);
    print("" + Integer);
    print("" + Intens_GDerivAt0);
    print("" + ItemFilterBackGnd);
    print("" + ItemIsBackGnd);
    print("" + Jump);  // KNOWN FP: the game accepts this — Scene::ESceneCharVisNotice value
    print("" + Kaiser);
    print("" + Lanczos);
    print("" + LeftNRight);
    print("" + LeftToRight);
    print("" + Left_Right);
    print("" + Left_Right_);
    print("" + Legends);
    print("" + LightFromMap);
    print("" + LineEven_Odd);
    print("" + LineOdd_Even);
    print("" + Line_Even_Odd);
    print("" + Line_Odd_Even);
    print("" + Mask);
    print("" + Maximum);
    print("" + Med);
    print("" + MinimalReadOnly);
    print("" + Mitchell);
    print("" + MotionRbx);
    print("" + Mud);
    print("" + MultiNadeo);
    print("" + NV_TXAA);
    print("" + Nations);
    print("" + Natural);
    print("" + No_Ladder);
    print("" + NotTimed);
    print("" + Oculus);
    print("" + Old0);
    print("" + Opacity);
    print("" + Optimized);
    print("" + ParamArray);
    print("" + ParentToChild);
    print("" + Pixel);
    print("" + PlaneDToAlpha);
    print("" + PlaneY);
    print("" + Protanopia);
    print("" + Pssm);
    print("" + Pylons_x_4);
    print("" + Pylons_x_8);
    print("" + Quarter);
    print("" + RGBA);
    print("" + RGBAPA);
    print("" + Real);
    print("" + Real3decimals);
    print("" + RealFormated);
    print("" + Receive);
    print("" + RedNCyan);
    print("" + Red_Cyan);
    print("" + RenderCameraDepth);
    print("" + RenderPlaneReflect);
    print("" + RenderWaterReflect);
    print("" + Resting);
    print("" + Retro);
    print("" + RightToLeft);
    print("" + Right_Left);
    print("" + Run2);
    print("" + SLSquare);
    print("" + SS_2x2);
    print("" + SS_3x3);
    print("" + Seated);
    print("" + Separate);
    print("" + ShadowDynaInAlpha);
    print("" + Site);
    print("" + SkyDive);
    print("" + SlideNoControl);
    print("" + SlideSurf);
    print("" + SlideSurfGliding);
    print("" + SlideSurfJump);
    print("" + SoloNadeo);
    print("" + Source_ConnectingForTransfer);
    print("" + Source_Done);
    print("" + Source_OnQueue);
    print("" + Source_RefreshingDownload);
    print("" + Source_RequestingDownload);
    print("" + Source_Transfer);
    print("" + Source_WaitingConnection);
    print("" + Spacialised_Omni);
    print("" + Spatialised);
    print("" + SpecIntens);
    print("" + Straight);
    print("" + Struct);
    print("" + Sustain_Road_Core);
    print("" + Sustain_Road_Exterior);
    print("" + Swim);
    print("" + T3_Clouds);
    print("" + T3_Decal3d);
    print("" + T3_TreeFoliageSH);
    print("" + TargetJump);
    print("" + Tech3AutoScale01);
    print("" + TechJump);  // KNOWN FP: the game accepts this — Scene::ESceneCharVisNotice value
    print("" + Text_Icon);
    print("" + TimeElapsed);
    print("" + TimeHhMm);
    print("" + TimeHhMmSs);
    print("" + TimeHhMmSsMil);
    print("" + TimeHhMmSsOrMmSs);
    print("" + TimeHhMmSs_24);
    print("" + TimeMmSs);
    print("" + TimeMmSsCc);
    print("" + Timed);
    print("" + TopToBottom);
    print("" + Top_Bottom);
    print("" + TreeId);
    print("" + Tritanopia);
    print("" + URLSource_Done);
    print("" + URLSource_Transfer);
    print("" + URLSource_WaitingConnection);
    print("" + UnitedBlockUs);
    print("" + Upload_Done);
    print("" + Upload_OnQueue);
    print("" + Upload_RequestingUpload);
    print("" + Upload_Transfer);
    print("" + Upload_WaitingConnection);
    print("" + VBR);
    print("" + VertexPosAvg);
    print("" + Vision);
    print("" + Visual_space);
    print("" + Walk);
    print("" + Webm);
    print("" + Webp);
    print("" + Wired);
    print("" + _);
    print("" + _03);
    print("" + _05);
    print("" + _0c);
    print("" + _100x);
    print("" + _16_10);
    print("" + _16_9);
    print("" + _16b);
    print("" + _196x);
    print("" + _1N_2);
    print("" + _1_D_2);
    print("" + _1_D_2_N_k);
    print("" + _1x);
    print("" + _2008_07);
    print("" + _2009_Slope1);
    print("" + _25x);
    print("" + _2_4_16);
    print("" + _2d);
    print("" + _2dNoClipZ);
    print("" + _32b);
    print("" + _3d);
    print("" + _400x);
    print("" + _49x);
    print("" + _4_3);
    print("" + _5_4);
    print("" + _9x);
    print("" + _HotSeat_Attempts);
    print("" + _HotSeat_Time);
    print("" + _HotSeat_Tries);
    print("" + _Keyboard_Left_Alt);
    print("" + _Keyboard_Left_Control);
    print("" + _Keyboard_Right_Alt);
    print("" + _Keyboard_Right_Control);
    print("" + _OldRunDoubleJump);
    print("" + _OldRunFall);
    print("" + _OldRunJump);
    print("" + _OldRunStop);
    print("" + _OldRunWallJump);
    print("" + __Camera);
    print("" + __WorldXZ);
    print("" + per_object);
    print("" + per_sprite);
}

void Main() {
    Probe();
}
