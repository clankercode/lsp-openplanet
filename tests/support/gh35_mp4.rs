use std::fs;
use std::path::{Path, PathBuf};

pub fn mp4_snapshot() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb-mp4/1.29.14")
}

pub const MP4_SOURCE: &str = r#"void Main() {}
#if MP4
bool ProbeMp4(CGameCtnApp@ app) {
    auto race = cast<CTrackManiaRaceNew>(app.CurrentPlayground);
    auto player = cast<CTrackManiaPlayer>(race.GameTerminals[0].ControlledPlayer);
    auto script = cast<CTrackManiaScriptPlayer>(player.ScriptAPI);
    return script.RequestsSpectate;
}
#endif
#if TMNEXT
MissingTmnextType must_stay_inactive;
#endif
"#;

pub fn plugin(root: &Path, source: &str) -> PathBuf {
    let plugin = root.join("plugin");
    fs::create_dir_all(plugin.join("src")).unwrap();
    fs::write(
        plugin.join("info.toml"),
        "[meta]\nname = \"GH35 MP4\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(plugin.join("src/Main.as"), source).unwrap();
    plugin
}

pub fn copy_mp4_snapshot(into: &Path, game_name: &str) {
    fs::create_dir_all(into).unwrap();
    for name in ["OpenplanetCore.json", "Openplanet4.json", "Openplanet.h"] {
        let destination = if name == "Openplanet4.json" {
            game_name
        } else {
            name
        };
        fs::copy(mp4_snapshot().join(name), into.join(destination)).unwrap();
    }
}
