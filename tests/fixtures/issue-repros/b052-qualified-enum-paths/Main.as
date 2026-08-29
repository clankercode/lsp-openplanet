// B052: a qualified path to a GLOBAL engine enum.
//
// AngelScript resolves `P::T` by looking `T` up in scope `P` and then walking
// P's ancestors out to the global namespace. So a global enum answers to ANY
// real scope's qualifier -- and to no other, because a path through something
// that is not a scope never starts the walk.
//
// Only three named enum keys in the shipped type DB are global: `VirtualKey`,
// `ESceneVehicleVisEvent` and `ESceneCharVisNotice`. Every other key is
// `Module::Class::Enum` or `CoreNs::Enum`, so this rule -- and the pre-B052
// tail fallback whose whole surface it was -- reaches exactly these three.
// The fallback was never "generic to every enum": it asked
// `lookup_source_enum(tail)`, which only answers for a COMPLETE key.
//
// What counts as a scope, and what does not, is the fix. The `ns` MODULE
// names this crate keys the Nadeo dump under (`Game`, `Scene`, `Plug`,
// `System`, ...) are its own invention: the game has no such namespaces and
// says so. An engine CLASS is a namespace only when it declares an enum,
// named or anonymous -- that is how Openplanet registers one -- and it is
// spelled without any module prefix.
//
// Game ground truth: RemoteBuild probe, Trackmania + Openplanet 1.29.5,
// 2026-08-30. Every name below was submitted individually, one statement per
// line, and the verdict recorded; the whole file was then compiled in-game as
// one plugin and produced exactly the 77 errors of `Rejected()` and nothing
// else. Nothing here is inferred from the dump.
//
// A caution for anyone re-running the probe: namespaces from OTHER installed
// plugins leak into the compile. `MLFeed::VirtualKey` resolves in-game purely
// because MLFeedRaceData.op is installed. A game REJECT is an engine fact; a
// game ACCEPT on an unrecognised prefix may only be an environment one.
//
// One statement per line: AngelScript reports one error per statement, at the
// first unresolved symbol, so every rejected name gets its own line number.

// ---------------------------------------------------------------------------
// REJECTED by the game -- 77 names, and the LSP must diagnose all of them.
// This is the B052 defect: before the fix the LSP was silent on 75 of these
// 77. (It already caught the last two: `EMenuOrder` is not a complete index
// key, so the tail fallback never fired for them.)
//
// The game's own messages, by count:
//     4x  Namespace 'Bogus' doesn't exist.
//     3x  Namespace 'Scene' doesn't exist.
//     3x  Namespace 'Game' doesn't exist.
//     3x  Namespace 'Plug' doesn't exist.
//     3x  Namespace 'System' doesn't exist.
//     3x  Namespace 'Hms' doesn't exist.
//     3x  Namespace 'Input' doesn't exist.
//     3x  Namespace 'Control' doesn't exist.
//     3x  Namespace 'Function' doesn't exist.
//     3x  Namespace 'Graphic' doesn't exist.
//     3x  Namespace 'Script' doesn't exist.
//     3x  Namespace 'Vision' doesn't exist.
//     3x  Namespace 'Xml' doesn't exist.
//     3x  Namespace 'TrackMania' doesn't exist.
//     3x  Namespace 'ShootMania' doesn't exist.
//     3x  Namespace 'MwFoundations' doesn't exist.
//     3x  Namespace 'MetaNotPersistent' doesn't exist.
//     3x  Namespace 'Enums' doesn't exist.
//     3x  Namespace 'Game::CGameMenu' doesn't exist.
//     3x  Namespace 'MetaNotPersistent::NPlugAnim' doesn't exist.
//     1x  Identifier 'VirtualKey' is not a data type in namespace 'CSceneVehicleVis' or parent
//     1x  Identifier 'ESceneVehicleVisEvent' is not a data type in namespace 'CSceneVehicleVis' or parent
//     1x  Identifier 'ESceneCharVisNotice' is not a data type in namespace 'CSceneVehicleVis' or parent
//     1x  Identifier 'VirtualKey' is not a data type in namespace 'CTrackMania' or parent
//     1x  Identifier 'ESceneVehicleVisEvent' is not a data type in namespace 'CTrackMania' or parent
//     1x  Identifier 'ESceneCharVisNotice' is not a data type in namespace 'CTrackMania' or parent
//     1x  Identifier 'VirtualKey' is not a data type in namespace 'CMwNod' or parent
//     1x  Identifier 'ESceneVehicleVisEvent' is not a data type in namespace 'CMwNod' or parent
//     1x  Identifier 'ESceneCharVisNotice' is not a data type in namespace 'CMwNod' or parent
//     1x  Identifier 'VirtualKey' is not a data type in namespace 'CSystemFidFile' or parent
//     1x  Identifier 'ESceneVehicleVisEvent' is not a data type in namespace 'CSystemFidFile' or parent
//     1x  Identifier 'ESceneCharVisNotice' is not a data type in namespace 'CSystemFidFile' or parent
//     1x  Identifier 'VirtualKey' is not a data type in namespace 'Plugin' or parent
//     1x  Identifier 'ESceneVehicleVisEvent' is not a data type in namespace 'Plugin' or parent
//     1x  Identifier 'ESceneCharVisNotice' is not a data type in namespace 'Plugin' or parent
//     1x  Identifier 'EMenuOrder' is not a data type in namespace 'CGameHighScore' or parent
// ---------------------------------------------------------------------------
void Rejected() {
    Scene::VirtualKey v_Scene_VirtualKey;
    Scene::ESceneVehicleVisEvent v_Scene_ESceneVehicleVisEvent;
    Scene::ESceneCharVisNotice v_Scene_ESceneCharVisNotice;
    Game::VirtualKey v_Game_VirtualKey;
    Game::ESceneVehicleVisEvent v_Game_ESceneVehicleVisEvent;
    Game::ESceneCharVisNotice v_Game_ESceneCharVisNotice;
    Plug::VirtualKey v_Plug_VirtualKey;
    Plug::ESceneVehicleVisEvent v_Plug_ESceneVehicleVisEvent;
    Plug::ESceneCharVisNotice v_Plug_ESceneCharVisNotice;
    System::VirtualKey v_System_VirtualKey;
    System::ESceneVehicleVisEvent v_System_ESceneVehicleVisEvent;
    System::ESceneCharVisNotice v_System_ESceneCharVisNotice;
    Hms::VirtualKey v_Hms_VirtualKey;
    Hms::ESceneVehicleVisEvent v_Hms_ESceneVehicleVisEvent;
    Hms::ESceneCharVisNotice v_Hms_ESceneCharVisNotice;
    Input::VirtualKey v_Input_VirtualKey;
    Input::ESceneVehicleVisEvent v_Input_ESceneVehicleVisEvent;
    Input::ESceneCharVisNotice v_Input_ESceneCharVisNotice;
    Control::VirtualKey v_Control_VirtualKey;
    Control::ESceneVehicleVisEvent v_Control_ESceneVehicleVisEvent;
    Control::ESceneCharVisNotice v_Control_ESceneCharVisNotice;
    Function::VirtualKey v_Function_VirtualKey;
    Function::ESceneVehicleVisEvent v_Function_ESceneVehicleVisEvent;
    Function::ESceneCharVisNotice v_Function_ESceneCharVisNotice;
    Graphic::VirtualKey v_Graphic_VirtualKey;
    Graphic::ESceneVehicleVisEvent v_Graphic_ESceneVehicleVisEvent;
    Graphic::ESceneCharVisNotice v_Graphic_ESceneCharVisNotice;
    Script::VirtualKey v_Script_VirtualKey;
    Script::ESceneVehicleVisEvent v_Script_ESceneVehicleVisEvent;
    Script::ESceneCharVisNotice v_Script_ESceneCharVisNotice;
    Vision::VirtualKey v_Vision_VirtualKey;
    Vision::ESceneVehicleVisEvent v_Vision_ESceneVehicleVisEvent;
    Vision::ESceneCharVisNotice v_Vision_ESceneCharVisNotice;
    Xml::VirtualKey v_Xml_VirtualKey;
    Xml::ESceneVehicleVisEvent v_Xml_ESceneVehicleVisEvent;
    Xml::ESceneCharVisNotice v_Xml_ESceneCharVisNotice;
    TrackMania::VirtualKey v_TrackMania_VirtualKey;
    TrackMania::ESceneVehicleVisEvent v_TrackMania_ESceneVehicleVisEvent;
    TrackMania::ESceneCharVisNotice v_TrackMania_ESceneCharVisNotice;
    ShootMania::VirtualKey v_ShootMania_VirtualKey;
    ShootMania::ESceneVehicleVisEvent v_ShootMania_ESceneVehicleVisEvent;
    ShootMania::ESceneCharVisNotice v_ShootMania_ESceneCharVisNotice;
    MwFoundations::VirtualKey v_MwFoundations_VirtualKey;
    MwFoundations::ESceneVehicleVisEvent v_MwFoundations_ESceneVehicleVisEvent;
    MwFoundations::ESceneCharVisNotice v_MwFoundations_ESceneCharVisNotice;
    MetaNotPersistent::VirtualKey v_MetaNotPersistent_VirtualKey;
    MetaNotPersistent::ESceneVehicleVisEvent v_MetaNotPersistent_ESceneVehicleVisEvent;
    MetaNotPersistent::ESceneCharVisNotice v_MetaNotPersistent_ESceneCharVisNotice;
    Enums::VirtualKey v_Enums_VirtualKey;
    Enums::ESceneVehicleVisEvent v_Enums_ESceneVehicleVisEvent;
    Enums::ESceneCharVisNotice v_Enums_ESceneCharVisNotice;
    Bogus::VirtualKey v_Bogus_VirtualKey;
    Bogus::ESceneVehicleVisEvent v_Bogus_ESceneVehicleVisEvent;
    Bogus::ESceneCharVisNotice v_Bogus_ESceneCharVisNotice;
    Game::CGameMenu::VirtualKey v_Game_CGameMenu_VirtualKey;
    Game::CGameMenu::ESceneVehicleVisEvent v_Game_CGameMenu_ESceneVehicleVisEvent;
    Game::CGameMenu::ESceneCharVisNotice v_Game_CGameMenu_ESceneCharVisNotice;
    MetaNotPersistent::NPlugAnim::VirtualKey v_MetaNotPersistent_NPlugAnim_VirtualKey;
    MetaNotPersistent::NPlugAnim::ESceneVehicleVisEvent v_MetaNotPersistent_NPlugAnim_ESceneVehicleVisEvent;
    MetaNotPersistent::NPlugAnim::ESceneCharVisNotice v_MetaNotPersistent_NPlugAnim_ESceneCharVisNotice;
    CSceneVehicleVis::VirtualKey v_CSceneVehicleVis_VirtualKey;
    CSceneVehicleVis::ESceneVehicleVisEvent v_CSceneVehicleVis_ESceneVehicleVisEvent;
    CSceneVehicleVis::ESceneCharVisNotice v_CSceneVehicleVis_ESceneCharVisNotice;
    CTrackMania::VirtualKey v_CTrackMania_VirtualKey;
    CTrackMania::ESceneVehicleVisEvent v_CTrackMania_ESceneVehicleVisEvent;
    CTrackMania::ESceneCharVisNotice v_CTrackMania_ESceneCharVisNotice;
    CMwNod::VirtualKey v_CMwNod_VirtualKey;
    CMwNod::ESceneVehicleVisEvent v_CMwNod_ESceneVehicleVisEvent;
    CMwNod::ESceneCharVisNotice v_CMwNod_ESceneCharVisNotice;
    CSystemFidFile::VirtualKey v_CSystemFidFile_VirtualKey;
    CSystemFidFile::ESceneVehicleVisEvent v_CSystemFidFile_ESceneVehicleVisEvent;
    CSystemFidFile::ESceneCharVisNotice v_CSystemFidFile_ESceneCharVisNotice;
    Meta::Plugin::VirtualKey v_Meta_Plugin_VirtualKey;
    Meta::Plugin::ESceneVehicleVisEvent v_Meta_Plugin_ESceneVehicleVisEvent;
    Meta::Plugin::ESceneCharVisNotice v_Meta_Plugin_ESceneCharVisNotice;
    Bogus::EMenuOrder v_Bogus_EMenuOrder;
    CGameHighScore::EMenuOrder v_CGameHighScore_EMenuOrder;
}

// ---------------------------------------------------------------------------
// ACCEPTED by the game -- 103 names, and the LSP must stay silent.
//
// These are the fence. Narrowing the fallback by deleting it would reject
// every `CoreNs::GlobalEnum` spelling here, which the game compiles happily:
// that would trade a false negative for a false POSITIVE, the damaging
// direction. The last four are nested engine enums spelled the way the game
// wants them -- class scope, no module prefix -- which the LSP got WRONG in
// the other direction until B052 taught `has_enum` a genuine suffix match.
// ---------------------------------------------------------------------------
void Accepted() {
    VirtualKey v_VirtualKey;
    ESceneVehicleVisEvent v_ESceneVehicleVisEvent;
    ESceneCharVisNotice v_ESceneCharVisNotice;
    UI::VirtualKey v_UI_VirtualKey;
    UI::ESceneVehicleVisEvent v_UI_ESceneVehicleVisEvent;
    UI::ESceneCharVisNotice v_UI_ESceneCharVisNotice;
    Meta::VirtualKey v_Meta_VirtualKey;
    Meta::ESceneVehicleVisEvent v_Meta_ESceneVehicleVisEvent;
    Meta::ESceneCharVisNotice v_Meta_ESceneCharVisNotice;
    Json::VirtualKey v_Json_VirtualKey;
    Json::ESceneVehicleVisEvent v_Json_ESceneVehicleVisEvent;
    Json::ESceneCharVisNotice v_Json_ESceneCharVisNotice;
    Net::VirtualKey v_Net_VirtualKey;
    Net::ESceneVehicleVisEvent v_Net_ESceneVehicleVisEvent;
    Net::ESceneCharVisNotice v_Net_ESceneCharVisNotice;
    Audio::VirtualKey v_Audio_VirtualKey;
    Audio::ESceneVehicleVisEvent v_Audio_ESceneVehicleVisEvent;
    Audio::ESceneCharVisNotice v_Audio_ESceneCharVisNotice;
    IO::VirtualKey v_IO_VirtualKey;
    IO::ESceneVehicleVisEvent v_IO_ESceneVehicleVisEvent;
    IO::ESceneCharVisNotice v_IO_ESceneCharVisNotice;
    nvg::VirtualKey v_nvg_VirtualKey;
    nvg::ESceneVehicleVisEvent v_nvg_ESceneVehicleVisEvent;
    nvg::ESceneCharVisNotice v_nvg_ESceneCharVisNotice;
    Text::VirtualKey v_Text_VirtualKey;
    Text::ESceneVehicleVisEvent v_Text_ESceneVehicleVisEvent;
    Text::ESceneCharVisNotice v_Text_ESceneCharVisNotice;
    Math::VirtualKey v_Math_VirtualKey;
    Math::ESceneVehicleVisEvent v_Math_ESceneVehicleVisEvent;
    Math::ESceneCharVisNotice v_Math_ESceneCharVisNotice;
    Regex::VirtualKey v_Regex_VirtualKey;
    Regex::ESceneVehicleVisEvent v_Regex_ESceneVehicleVisEvent;
    Regex::ESceneCharVisNotice v_Regex_ESceneCharVisNotice;
    Dev::VirtualKey v_Dev_VirtualKey;
    Dev::ESceneVehicleVisEvent v_Dev_ESceneVehicleVisEvent;
    Dev::ESceneCharVisNotice v_Dev_ESceneCharVisNotice;
    Import::VirtualKey v_Import_VirtualKey;
    Import::ESceneVehicleVisEvent v_Import_ESceneVehicleVisEvent;
    Import::ESceneCharVisNotice v_Import_ESceneCharVisNotice;
    XML::VirtualKey v_XML_VirtualKey;
    XML::ESceneVehicleVisEvent v_XML_ESceneVehicleVisEvent;
    XML::ESceneCharVisNotice v_XML_ESceneCharVisNotice;
    Time::VirtualKey v_Time_VirtualKey;
    Time::ESceneVehicleVisEvent v_Time_ESceneVehicleVisEvent;
    Time::ESceneCharVisNotice v_Time_ESceneCharVisNotice;
    Icons::VirtualKey v_Icons_VirtualKey;
    Icons::ESceneVehicleVisEvent v_Icons_ESceneVehicleVisEvent;
    Icons::ESceneCharVisNotice v_Icons_ESceneCharVisNotice;
    Fids::VirtualKey v_Fids_VirtualKey;
    Fids::ESceneVehicleVisEvent v_Fids_ESceneVehicleVisEvent;
    Fids::ESceneCharVisNotice v_Fids_ESceneCharVisNotice;
    Path::VirtualKey v_Path_VirtualKey;
    Path::ESceneVehicleVisEvent v_Path_ESceneVehicleVisEvent;
    Path::ESceneCharVisNotice v_Path_ESceneCharVisNotice;
    Crypto::VirtualKey v_Crypto_VirtualKey;
    Crypto::ESceneVehicleVisEvent v_Crypto_ESceneVehicleVisEvent;
    Crypto::ESceneCharVisNotice v_Crypto_ESceneCharVisNotice;
    Auth::VirtualKey v_Auth_VirtualKey;
    Auth::ESceneVehicleVisEvent v_Auth_ESceneVehicleVisEvent;
    Auth::ESceneCharVisNotice v_Auth_ESceneCharVisNotice;
    Display::VirtualKey v_Display_VirtualKey;
    Display::ESceneVehicleVisEvent v_Display_ESceneVehicleVisEvent;
    Display::ESceneCharVisNotice v_Display_ESceneCharVisNotice;
    Reflection::VirtualKey v_Reflection_VirtualKey;
    Reflection::ESceneVehicleVisEvent v_Reflection_ESceneVehicleVisEvent;
    Reflection::ESceneCharVisNotice v_Reflection_ESceneCharVisNotice;
    Permissions::VirtualKey v_Permissions_VirtualKey;
    Permissions::ESceneVehicleVisEvent v_Permissions_ESceneVehicleVisEvent;
    Permissions::ESceneCharVisNotice v_Permissions_ESceneCharVisNotice;
    SQLite::VirtualKey v_SQLite_VirtualKey;
    SQLite::ESceneVehicleVisEvent v_SQLite_ESceneVehicleVisEvent;
    SQLite::ESceneCharVisNotice v_SQLite_ESceneCharVisNotice;
    Settings::VirtualKey v_Settings_VirtualKey;
    Settings::ESceneVehicleVisEvent v_Settings_ESceneVehicleVisEvent;
    Settings::ESceneCharVisNotice v_Settings_ESceneCharVisNotice;
    Tests::VirtualKey v_Tests_VirtualKey;
    Tests::ESceneVehicleVisEvent v_Tests_ESceneVehicleVisEvent;
    Tests::ESceneCharVisNotice v_Tests_ESceneCharVisNotice;
    mat3::VirtualKey v_mat3_VirtualKey;
    mat3::ESceneVehicleVisEvent v_mat3_ESceneVehicleVisEvent;
    mat3::ESceneCharVisNotice v_mat3_ESceneCharVisNotice;
    mat4::VirtualKey v_mat4_VirtualKey;
    mat4::ESceneVehicleVisEvent v_mat4_ESceneVehicleVisEvent;
    mat4::ESceneCharVisNotice v_mat4_ESceneCharVisNotice;
    UI::Font::VirtualKey v_UI_Font_VirtualKey;
    UI::Font::ESceneVehicleVisEvent v_UI_Font_ESceneVehicleVisEvent;
    UI::Font::ESceneCharVisNotice v_UI_Font_ESceneCharVisNotice;
    UI::Plot::VirtualKey v_UI_Plot_VirtualKey;
    UI::Plot::ESceneVehicleVisEvent v_UI_Plot_ESceneVehicleVisEvent;
    UI::Plot::ESceneCharVisNotice v_UI_Plot_ESceneCharVisNotice;
    CGameMenu::VirtualKey v_CGameMenu_VirtualKey;
    CGameMenu::ESceneVehicleVisEvent v_CGameMenu_ESceneVehicleVisEvent;
    CGameMenu::ESceneCharVisNotice v_CGameMenu_ESceneCharVisNotice;
    CGameCtnApp::VirtualKey v_CGameCtnApp_VirtualKey;
    CGameCtnApp::ESceneVehicleVisEvent v_CGameCtnApp_ESceneVehicleVisEvent;
    CGameCtnApp::ESceneCharVisNotice v_CGameCtnApp_ESceneCharVisNotice;
    CGameCtnChallenge::VirtualKey v_CGameCtnChallenge_VirtualKey;
    CGameCtnChallenge::ESceneVehicleVisEvent v_CGameCtnChallenge_ESceneVehicleVisEvent;
    CGameCtnChallenge::ESceneCharVisNotice v_CGameCtnChallenge_ESceneCharVisNotice;
    CGameMenu::EMenuOrder v_CGameMenu_EMenuOrder;
    CGameCtnBlock::ECardinalDirections v_CGameCtnBlock_ECardinalDirections;
    CGameHighScore::EMedal v_CGameHighScore_EMedal;
    CHmsViewport::EPC3Quality v_CHmsViewport_EPC3Quality;
}

// ---------------------------------------------------------------------------
// KNOWN FALSE POSITIVE -- 3 names, PRE-EXISTING and unrelated to B052.
//
// The game accepts all three (probed individually, 2026-08-30): `string` is a
// real namespace, so the ancestor walk finds the global enum exactly as it
// does for `mat3::` and `mat4::` two sections above. openplanet-lsp never gets
// that far -- its lexer treats `string` as a keyword, so `string::VirtualKey`
// fails to PARSE (`expected Semi, found Ident`) before any name resolution
// runs. The merge-base binary emits the same three errors, so B052 neither
// causes nor fixes them; they are pinned here so the parser gap is visible and
// so a future fix has a test that tells it to update this section.
// ---------------------------------------------------------------------------
void KnownParserGap() {
    string::VirtualKey v_string_VirtualKey;
    string::ESceneVehicleVisEvent v_string_ESceneVehicleVisEvent;
    string::ESceneCharVisNotice v_string_ESceneCharVisNotice;
}

void Main() {
    Rejected();
    Accepted();
    KnownParserGap();
}
