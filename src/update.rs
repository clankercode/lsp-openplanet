//! Self-update check and apply helpers.
//!
//! Latest version is resolved from the **npm registry** (not the GitHub API).
//! Install method is inferred from the running binary path; update applies the
//! matching package-manager command when one is known.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const PACKAGE_NAME: &str = "openplanet-lsp";
const NPM_LATEST_URL: &str = "https://registry.npmjs.org/openplanet-lsp/latest";
const CARGO_GIT_URL: &str = "https://github.com/clankercode/lsp-openplanet";
const STATUS_FILE_NAME: &str = "update-status.json";
const DEFAULT_CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// How this binary appears to have been installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallMethod {
    /// `npm install -g openplanet-lsp` (binary under a global node_modules tree).
    NpmGlobal,
    /// Project-local `node_modules` dependency.
    NpmLocal { package_root: PathBuf },
    /// `~/.cargo/bin/openplanet-lsp` (typically `cargo install --git …`).
    Cargo,
    /// Built from a checkout (`target/debug` or `target/release`).
    Development,
    /// Unpacked release archive or other fixed path.
    Standalone { exe_path: PathBuf },
    /// Could not classify.
    Unknown { exe_path: PathBuf },
}

impl InstallMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            InstallMethod::NpmGlobal => "npm-global",
            InstallMethod::NpmLocal { .. } => "npm-local",
            InstallMethod::Cargo => "cargo",
            InstallMethod::Development => "development",
            InstallMethod::Standalone { .. } => "standalone",
            InstallMethod::Unknown { .. } => "unknown",
        }
    }

    /// Shell command the user (or `update`) should run to upgrade, if any.
    pub fn update_command(&self) -> Option<String> {
        match self {
            InstallMethod::NpmGlobal => Some(format!("npm install -g {PACKAGE_NAME}@latest")),
            InstallMethod::NpmLocal { package_root } => Some(format!(
                "npm install {PACKAGE_NAME}@latest  # in {}",
                package_root.display()
            )),
            InstallMethod::Cargo => Some(format!(
                "cargo install --git {CARGO_GIT_URL} --locked --force"
            )),
            InstallMethod::Development => None,
            InstallMethod::Standalone { .. } | InstallMethod::Unknown { .. } => Some(format!(
                "npm install -g {PACKAGE_NAME}@latest  # or re-download from GitHub Releases"
            )),
        }
    }

    pub fn can_auto_apply(&self) -> bool {
        matches!(
            self,
            InstallMethod::NpmGlobal | InstallMethod::NpmLocal { .. } | InstallMethod::Cargo
        )
    }
}

/// Persisted update probe result under the user config directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateStatus {
    /// Unix epoch seconds when the check ran.
    pub checked_at: u64,
    /// RFC3339-ish UTC timestamp for humans.
    pub checked_at_rfc3339: String,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub install_method: String,
    pub exe_path: String,
    pub update_command: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum UpdateError {
    Message(String),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::Message(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for UpdateError {}

impl UpdateError {
    fn msg(s: impl Into<String>) -> Self {
        UpdateError::Message(s.into())
    }
}

/// Options for the `update` CLI command.
#[derive(Debug, Clone, Default)]
pub struct UpdateOptions {
    /// Only check and write the status file; do not install.
    pub check_only: bool,
    /// Print last saved status without hitting the network.
    pub status_only: bool,
    /// Re-check even if the status file is fresh.
    pub force_check: bool,
    /// Reinstall even when already on the latest reported version.
    pub force_install: bool,
}

// ── public entry points ────────────────────────────────────────────────────

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Resolve the user config directory (`~/.config/openplanet-lsp` by default).
pub fn config_dir() -> Result<PathBuf, UpdateError> {
    if let Ok(dir) = std::env::var("OPENPLANET_LSP_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Ok(home) = std::env::var("HOME") {
        return Ok(PathBuf::from(home).join(".config").join(PACKAGE_NAME));
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return Ok(PathBuf::from(profile).join(".config").join(PACKAGE_NAME));
    }
    Err(UpdateError::msg(
        "cannot resolve config dir: set HOME, USERPROFILE, or OPENPLANET_LSP_CONFIG_DIR",
    ))
}

pub fn status_path() -> Result<PathBuf, UpdateError> {
    Ok(config_dir()?.join(STATUS_FILE_NAME))
}

pub fn load_status() -> Result<Option<UpdateStatus>, UpdateError> {
    let path = status_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)
        .map_err(|e| UpdateError::msg(format!("failed to read {}: {e}", path.display())))?;
    let status = serde_json::from_str(&text)
        .map_err(|e| UpdateError::msg(format!("failed to parse {}: {e}", path.display())))?;
    Ok(Some(status))
}

pub fn save_status(status: &UpdateStatus) -> Result<PathBuf, UpdateError> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir)
        .map_err(|e| UpdateError::msg(format!("failed to create {}: {e}", dir.display())))?;
    let path = dir.join(STATUS_FILE_NAME);
    let text = serde_json::to_string_pretty(status)
        .map_err(|e| UpdateError::msg(format!("failed to serialize update status: {e}")))?;
    fs::write(&path, text + "\n")
        .map_err(|e| UpdateError::msg(format!("failed to write {}: {e}", path.display())))?;
    Ok(path)
}

/// Detect install method from the running executable path.
pub fn detect_install_method() -> InstallMethod {
    let exe = match resolve_exe_path() {
        Ok(p) => p,
        Err(_) => {
            return InstallMethod::Unknown {
                exe_path: PathBuf::from("unknown"),
            }
        }
    };
    detect_install_method_from_path(&exe)
}

pub fn detect_install_method_from_path(exe: &Path) -> InstallMethod {
    let components = path_components_lossy(exe);

    // npm optionalDep layout:
    //   .../node_modules/openplanet-lsp-<plat>/bin/openplanet-lsp
    //   .../node_modules/openplanet-lsp/node_modules/openplanet-lsp-<plat>/bin/...
    if let Some(nm_idx) = components.iter().rposition(|c| c == "node_modules") {
        let package_root = infer_npm_package_root(exe, nm_idx, &components);
        if is_npm_global_path(exe) {
            return InstallMethod::NpmGlobal;
        }
        return InstallMethod::NpmLocal {
            package_root: package_root.unwrap_or_else(|| {
                exe.parent()
                    .and_then(|p| p.parent())
                    .unwrap_or(exe)
                    .to_path_buf()
            }),
        };
    }

    // cargo install destination
    if components
        .windows(2)
        .any(|w| w[0] == ".cargo" && w[1] == "bin")
    {
        return InstallMethod::Cargo;
    }

    // local cargo build
    if components
        .windows(2)
        .any(|w| w[0] == "target" && (w[1] == "debug" || w[1] == "release"))
    {
        return InstallMethod::Development;
    }

    InstallMethod::Standalone {
        exe_path: exe.to_path_buf(),
    }
}

/// Split a path on `/` and `\` so Windows layouts still classify when the host
/// OS uses a different separator (and for mixed-separator paths).
fn path_components_lossy(path: &Path) -> Vec<String> {
    path.to_string_lossy()
        .split(['/', '\\'])
        .filter(|p| !p.is_empty() && *p != ".")
        .map(|p| p.to_string())
        .collect()
}

/// Compare dotted numeric versions (`1.2.3`). Returns `Ordering` when both parse.
pub fn cmp_versions(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let pa = parse_version(a)?;
    let pb = parse_version(b)?;
    Some(pa.cmp(&pb))
}

pub fn is_update_available(current: &str, latest: &str) -> bool {
    matches!(
        cmp_versions(current, latest),
        Some(std::cmp::Ordering::Less)
    )
}

/// Fetch the latest published version from the npm registry.
pub fn fetch_latest_version() -> Result<String, UpdateError> {
    if let Ok(v) = fetch_latest_via_curl() {
        return Ok(v);
    }
    if let Ok(v) = fetch_latest_via_npm_view() {
        return Ok(v);
    }
    Err(UpdateError::msg(
        "failed to query npm for latest version (tried curl registry.npmjs.org and `npm view`)",
    ))
}

/// Run a version check, always writing the status file.
pub fn check_for_update() -> Result<UpdateStatus, UpdateError> {
    let current = current_version().to_string();
    let method = detect_install_method();
    let exe = resolve_exe_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".into());

    let (latest, error) = match fetch_latest_version() {
        Ok(v) => (Some(v), None),
        Err(e) => (None, Some(e.to_string())),
    };

    let update_available = match &latest {
        Some(latest) => is_update_available(&current, latest),
        None => false,
    };

    let now = now_epoch_secs();
    let status = UpdateStatus {
        checked_at: now,
        checked_at_rfc3339: epoch_to_rfc3339(now),
        current_version: current,
        latest_version: latest,
        update_available,
        install_method: method.as_str().to_string(),
        exe_path: exe,
        update_command: method.update_command(),
        error,
    };
    save_status(&status)?;
    Ok(status)
}

/// Whether an automatic (LSP startup) check should run given the last status.
pub fn should_auto_check(last: Option<&UpdateStatus>, interval: Duration) -> bool {
    let Some(last) = last else {
        return true;
    };
    let now = now_epoch_secs();
    now.saturating_sub(last.checked_at) >= interval.as_secs()
}

pub fn default_check_interval() -> Duration {
    Duration::from_secs(DEFAULT_CHECK_INTERVAL_SECS)
}

/// Apply an update using the detected install method.
pub fn apply_update(force: bool) -> Result<UpdateStatus, UpdateError> {
    let method = detect_install_method();
    let status = check_for_update()?;
    apply_update_with_status(&method, &status, force)
}

/// Apply using a status already obtained from [`check_for_update`].
pub fn apply_update_with_status(
    method: &InstallMethod,
    status: &UpdateStatus,
    force: bool,
) -> Result<UpdateStatus, UpdateError> {
    if !method.can_auto_apply() {
        let hint = method
            .update_command()
            .unwrap_or_else(|| "rebuild from source or install via npm".into());
        return Err(UpdateError::msg(format!(
            "cannot auto-update install method `{}`; try: {hint}",
            method.as_str()
        )));
    }

    if let Some(err) = &status.error {
        return Err(UpdateError::msg(err.clone()));
    }
    if !status.update_available && !force {
        return Ok(status.clone());
    }

    match method {
        InstallMethod::NpmGlobal => {
            run_command(
                "npm",
                &["install", "-g", &format!("{PACKAGE_NAME}@latest")],
                None,
            )?;
        }
        InstallMethod::NpmLocal { package_root } => {
            run_command(
                "npm",
                &["install", &format!("{PACKAGE_NAME}@latest")],
                Some(package_root),
            )?;
        }
        InstallMethod::Cargo => {
            run_command(
                "cargo",
                &["install", "--git", CARGO_GIT_URL, "--locked", "--force"],
                None,
            )?;
        }
        _ => unreachable!("can_auto_apply guards apply arms"),
    }

    // Refresh status after install. The running binary version is unchanged
    // until restart; record the latest registry view.
    let mut after = check_for_update()?;
    after.error = None;
    save_status(&after)?;
    Ok(after)
}

/// Format a human-readable status report.
pub fn format_status(status: &UpdateStatus) -> String {
    let mut out = String::new();
    out.push_str(&format!("current:  {}\n", status.current_version));
    match &status.latest_version {
        Some(v) => out.push_str(&format!("latest:   {v} (npm)\n")),
        None => out.push_str("latest:   (unavailable)\n"),
    }
    out.push_str(&format!("method:   {}\n", status.install_method));
    out.push_str(&format!("exe:      {}\n", status.exe_path));
    out.push_str(&format!("checked:  {}\n", status.checked_at_rfc3339));
    if status.update_available {
        out.push_str("status:   update available\n");
        if let Some(cmd) = &status.update_command {
            out.push_str(&format!("update:   {cmd}\n"));
        }
    } else if status.error.is_some() {
        out.push_str("status:   check failed\n");
    } else {
        out.push_str("status:   up to date\n");
    }
    if let Some(err) = &status.error {
        out.push_str(&format!("error:    {err}\n"));
    }
    if let Ok(path) = status_path() {
        out.push_str(&format!("file:     {}\n", path.display()));
    }
    out
}

/// CLI entry: parse is done by main; this runs the chosen action.
pub fn run_update(options: &UpdateOptions) -> Result<String, UpdateError> {
    if options.status_only {
        return match load_status()? {
            Some(status) => Ok(format_status(&status)),
            None => Ok("no update status saved yet; run `openplanet-lsp update --check`\n".into()),
        };
    }

    if options.check_only {
        let status = check_for_update()?;
        return Ok(format_status(&status));
    }

    // Full update path
    let method = detect_install_method();
    let before = check_for_update()?;
    if let Some(err) = &before.error {
        return Err(UpdateError::msg(err.clone()));
    }
    if !before.update_available && !options.force_install {
        return Ok(format_status(&before));
    }

    if !method.can_auto_apply() {
        let hint = method
            .update_command()
            .unwrap_or_else(|| "rebuild from source or install via npm".into());
        let reason = if before.update_available {
            format!(
                "Update available but install method `{}` cannot be auto-updated.",
                method.as_str()
            )
        } else {
            format!(
                "Install method `{}` cannot be auto-updated.",
                method.as_str()
            )
        };
        let mut report = format!("{reason}\nManual step: {hint}\n\n");
        report.push_str(&format_status(&before));
        return Ok(report);
    }

    let after = apply_update_with_status(&method, &before, options.force_install)?;
    let mut report = String::new();
    report.push_str("Update command finished.\n");
    report.push_str(
        "Restart openplanet-lsp (and your editor language client) to use the new binary.\n\n",
    );
    report.push_str(&format_status(&after));
    Ok(report)
}

// ── internals ──────────────────────────────────────────────────────────────

fn resolve_exe_path() -> Result<PathBuf, UpdateError> {
    if let Ok(p) = std::env::var("OPENPLANET_LSP_EXE") {
        let pb = PathBuf::from(p);
        return Ok(pb.canonicalize().unwrap_or(pb));
    }
    let exe = std::env::current_exe()
        .map_err(|e| UpdateError::msg(format!("cannot resolve current exe: {e}")))?;
    Ok(exe.canonicalize().unwrap_or(exe))
}

fn infer_npm_package_root(exe: &Path, nm_idx: usize, components: &[String]) -> Option<PathBuf> {
    // Prefer the directory that owns the top-level node_modules (project root
    // or npm prefix). For nested optionalDeps, use the outermost node_modules.
    let outer_nm = components.iter().position(|c| c == "node_modules")?;
    let root_parts: Vec<&str> = components[..outer_nm].iter().map(String::as_str).collect();
    if root_parts.is_empty() {
        // path started at node_modules — fall back to host-native parents
        let fallback: PathBuf = exe.components().take(nm_idx).collect();
        if fallback.as_os_str().is_empty() {
            return None;
        }
        return Some(fallback);
    }

    // Rebuild with `/` join; on Windows absolute paths keep a drive prefix like `C:`.
    let mut root = PathBuf::new();
    for (i, part) in root_parts.iter().enumerate() {
        if i == 0 && part.ends_with(':') {
            // Drive letter — PathBuf::push("C:") alone is fine on Windows; on
            // Unix keep it as a prefix component for stable test expectations.
            root.push(part);
        } else if i == 0 && !part.starts_with(':') {
            // Absolute POSIX path
            root.push(format!("/{part}"));
        } else {
            root.push(part);
        }
    }
    Some(root)
}

fn is_npm_global_path(exe: &Path) -> bool {
    // Heuristics: global prefix layouts and `npm root -g` match.
    let s = exe.to_string_lossy();
    let global_markers = [
        "/lib/node_modules/",
        "\\lib\\node_modules\\",
        "/lib64/node_modules/",
        "/share/node_modules/",
        "AppData\\Roaming\\npm\\node_modules\\",
        "AppData/Roaming/npm/node_modules/",
        "/npm/lib/node_modules/",
        "/.nvm/versions/",
        "/.fnm/",
        "/.volta/tools/image/packages/",
        "/.local/share/pnpm/global/",
        "/.local/lib/node_modules/",
    ];
    if global_markers.iter().any(|m| s.contains(m)) {
        return true;
    }

    if let Some(global_root) = npm_global_root() {
        if s.contains(&*global_root.to_string_lossy()) {
            return true;
        }
    }
    false
}

fn npm_global_root() -> Option<PathBuf> {
    let output = Command::new("npm").args(["root", "-g"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() {
        None
    } else {
        Some(PathBuf::from(root))
    }
}

fn parse_version(raw: &str) -> Option<(u64, u64, u64)> {
    let s = raw.trim().trim_start_matches('v');
    // strip pre-release / build metadata
    let core = s.split(['-', '+']).next().unwrap_or(s);
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

fn fetch_latest_via_curl() -> Result<String, UpdateError> {
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "15",
            "-H",
            "Accept: application/json",
            NPM_LATEST_URL,
        ])
        .output()
        .map_err(|e| UpdateError::msg(format!("curl failed: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(UpdateError::msg(format!(
            "curl exited {}: {stderr}",
            output.status
        )));
    }
    parse_npm_latest_json(&output.stdout)
}

fn fetch_latest_via_npm_view() -> Result<String, UpdateError> {
    let output = Command::new("npm")
        .args(["view", PACKAGE_NAME, "version"])
        .output()
        .map_err(|e| UpdateError::msg(format!("npm view failed: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(UpdateError::msg(format!(
            "npm view exited {}: {stderr}",
            output.status
        )));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if version.is_empty() || parse_version(&version).is_none() {
        return Err(UpdateError::msg(format!(
            "npm view returned unusable version: {version:?}"
        )));
    }
    Ok(version)
}

fn parse_npm_latest_json(bytes: &[u8]) -> Result<String, UpdateError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| UpdateError::msg(format!("invalid npm registry JSON: {e}")))?;
    let version = value
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| UpdateError::msg("npm registry JSON missing string \"version\""))?;
    if parse_version(version).is_none() {
        return Err(UpdateError::msg(format!(
            "npm registry version not semver-like: {version}"
        )));
    }
    Ok(version.to_string())
}

fn run_command(program: &str, args: &[&str], cwd: Option<&Path>) -> Result<(), UpdateError> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let output = cmd
        .output()
        .map_err(|e| UpdateError::msg(format!("failed to spawn `{program}`: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(UpdateError::msg(format!(
            "`{program} {}` failed ({}):\n{stdout}{stderr}",
            args.join(" "),
            output.status
        )));
    }
    Ok(())
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn epoch_to_rfc3339(secs: u64) -> String {
    // Keep this dependency-free: format as UTC approximate timestamp.
    // Good enough for a status file; not a full chrono implementation.
    const DAY: u64 = 24 * 3600;
    let days = secs / DAY;
    let rem = secs % DAY;
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    let sec = rem % 60;

    // Civil date from Unix day count (Howard Hinnant algorithm).
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize tests that touch process-global env vars.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn version_compare_basic() {
        assert_eq!(
            cmp_versions("0.2.3", "0.2.4"),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            cmp_versions("1.0.0", "1.0.0"),
            Some(std::cmp::Ordering::Equal)
        );
        assert_eq!(
            cmp_versions("2.0.0", "1.9.9"),
            Some(std::cmp::Ordering::Greater)
        );
        assert!(is_update_available("0.2.0", "0.2.4"));
        assert!(!is_update_available("0.2.4", "0.2.4"));
        assert!(!is_update_available("0.3.0", "0.2.9"));
        assert_eq!(
            cmp_versions("v1.2.3", "1.2.3"),
            Some(std::cmp::Ordering::Equal)
        );
    }

    #[test]
    fn parse_npm_latest_json_extracts_version() {
        let json = br#"{"name":"openplanet-lsp","version":"0.2.0"}"#;
        assert_eq!(parse_npm_latest_json(json).unwrap(), "0.2.0");
    }

    #[test]
    fn detect_npm_global_linux_layout() {
        let path = PathBuf::from(
            "/home/user/.nvm/versions/node/v20.0.0/lib/node_modules/openplanet-lsp-linux-x64/bin/openplanet-lsp",
        );
        assert_eq!(
            detect_install_method_from_path(&path),
            InstallMethod::NpmGlobal
        );
    }

    #[test]
    fn detect_npm_local_layout() {
        let path = PathBuf::from(
            "/work/my-plugin/node_modules/openplanet-lsp-linux-x64/bin/openplanet-lsp",
        );
        match detect_install_method_from_path(&path) {
            InstallMethod::NpmLocal { package_root } => {
                assert_eq!(package_root, PathBuf::from("/work/my-plugin"));
            }
            other => panic!("expected NpmLocal, got {other:?}"),
        }
    }

    #[test]
    fn detect_cargo_bin() {
        let path = PathBuf::from("/home/user/.cargo/bin/openplanet-lsp");
        assert_eq!(detect_install_method_from_path(&path), InstallMethod::Cargo);
    }

    #[test]
    fn detect_development_target() {
        let path = PathBuf::from("/home/user/src/lsp-openplanet/target/release/openplanet-lsp");
        assert_eq!(
            detect_install_method_from_path(&path),
            InstallMethod::Development
        );
    }

    #[test]
    fn detect_standalone() {
        let path = PathBuf::from("/opt/openplanet-lsp/openplanet-lsp");
        match detect_install_method_from_path(&path) {
            InstallMethod::Standalone { exe_path } => {
                assert_eq!(exe_path, path);
            }
            other => panic!("expected Standalone, got {other:?}"),
        }
    }

    #[test]
    fn update_command_for_methods() {
        assert!(InstallMethod::NpmGlobal
            .update_command()
            .unwrap()
            .contains("npm install -g"));
        assert!(InstallMethod::Cargo
            .update_command()
            .unwrap()
            .contains("cargo install --git"));
        assert!(InstallMethod::Development.update_command().is_none());
        assert!(InstallMethod::NpmGlobal.can_auto_apply());
        assert!(!InstallMethod::Development.can_auto_apply());
    }

    #[test]
    fn status_roundtrip_in_temp_config_dir() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "openplanet-lsp-update-test-{}-{}",
            std::process::id(),
            now_epoch_secs()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("OPENPLANET_LSP_CONFIG_DIR", &dir);

        let status = UpdateStatus {
            checked_at: 1_700_000_000,
            checked_at_rfc3339: epoch_to_rfc3339(1_700_000_000),
            current_version: "0.2.4".into(),
            latest_version: Some("0.3.0".into()),
            update_available: true,
            install_method: "npm-global".into(),
            exe_path: "/bin/openplanet-lsp".into(),
            update_command: Some("npm install -g openplanet-lsp@latest".into()),
            error: None,
        };
        let path = save_status(&status).unwrap();
        assert!(path.ends_with(STATUS_FILE_NAME));
        let loaded = load_status().unwrap().unwrap();
        assert_eq!(loaded, status);

        std::env::remove_var("OPENPLANET_LSP_CONFIG_DIR");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn should_auto_check_respects_interval() {
        let fresh = UpdateStatus {
            checked_at: now_epoch_secs(),
            checked_at_rfc3339: "now".into(),
            current_version: "0.2.4".into(),
            latest_version: Some("0.2.4".into()),
            update_available: false,
            install_method: "npm-global".into(),
            exe_path: "/x".into(),
            update_command: None,
            error: None,
        };
        assert!(!should_auto_check(
            Some(&fresh),
            Duration::from_secs(24 * 3600)
        ));
        assert!(should_auto_check(None, Duration::from_secs(24 * 3600)));

        let stale = UpdateStatus {
            checked_at: now_epoch_secs().saturating_sub(48 * 3600),
            ..fresh.clone()
        };
        assert!(should_auto_check(
            Some(&stale),
            Duration::from_secs(24 * 3600)
        ));
    }

    #[test]
    fn epoch_to_rfc3339_known_instant() {
        // 2024-01-01T00:00:00Z
        assert_eq!(epoch_to_rfc3339(1_704_067_200), "2024-01-01T00:00:00Z");
    }

    #[test]
    fn detect_npm_nested_optional_dep() {
        let path = PathBuf::from(
            "/work/app/node_modules/openplanet-lsp/node_modules/openplanet-lsp-linux-x64/bin/openplanet-lsp",
        );
        match detect_install_method_from_path(&path) {
            InstallMethod::NpmLocal { package_root } => {
                assert_eq!(package_root, PathBuf::from("/work/app"));
            }
            other => panic!("expected NpmLocal, got {other:?}"),
        }
    }

    #[test]
    fn detect_npm_global_windows_roaming() {
        let path = PathBuf::from(
            r"C:\Users\me\AppData\Roaming\npm\node_modules\openplanet-lsp-win32-x64\bin\openplanet-lsp.exe",
        );
        assert_eq!(
            detect_install_method_from_path(&path),
            InstallMethod::NpmGlobal
        );
    }

    #[test]
    fn format_status_mentions_update() {
        let status = UpdateStatus {
            checked_at: 0,
            checked_at_rfc3339: "1970-01-01T00:00:00Z".into(),
            current_version: "0.2.0".into(),
            latest_version: Some("0.2.4".into()),
            update_available: true,
            install_method: "npm-global".into(),
            exe_path: "/x".into(),
            update_command: Some("npm install -g openplanet-lsp@latest".into()),
            error: None,
        };
        let text = format_status(&status);
        assert!(text.contains("update available"));
        assert!(text.contains("0.2.4"));
    }
}
