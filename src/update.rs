//! Self-update check and apply helpers.
//!
//! Latest version is resolved from the **npm registry** (not the GitHub API).
//! Install method is inferred from the running binary path; update applies the
//! matching package-manager command when one is known (npm / pnpm / yarn / bun,
//! cargo) or downloads the GitHub Release archive for **standalone** installs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const PACKAGE_NAME: &str = "openplanet-lsp";
const NPM_LATEST_URL: &str = "https://registry.npmjs.org/openplanet-lsp/latest";
const CARGO_GIT_URL: &str = "https://github.com/clankercode/lsp-openplanet";
/// GitHub repo used for standalone release-asset downloads.
const GITHUB_REPO: &str = "clankercode/lsp-openplanet";
const STATUS_FILE_NAME: &str = "update-status.json";
const DEFAULT_CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// JavaScript package manager used for a node_modules-based install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsPackageManager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

impl JsPackageManager {
    pub fn as_str(self) -> &'static str {
        match self {
            JsPackageManager::Npm => "npm",
            JsPackageManager::Pnpm => "pnpm",
            JsPackageManager::Yarn => "yarn",
            JsPackageManager::Bun => "bun",
        }
    }

    pub fn program(self) -> &'static str {
        self.as_str()
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "npm" => Some(Self::Npm),
            "pnpm" => Some(Self::Pnpm),
            "yarn" | "yarnpkg" => Some(Self::Yarn),
            "bun" => Some(Self::Bun),
            _ => None,
        }
    }
}

/// How this binary appears to have been installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallMethod {
    /// Global JS package manager install (`npm/pnpm/yarn/bun … -g`).
    NodeGlobal { pm: JsPackageManager },
    /// Project-local dependency under a package root.
    NodeLocal {
        pm: JsPackageManager,
        package_root: PathBuf,
    },
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
    pub fn as_str(&self) -> String {
        match self {
            InstallMethod::NodeGlobal { pm } => format!("{}-global", pm.as_str()),
            InstallMethod::NodeLocal { pm, .. } => format!("{}-local", pm.as_str()),
            InstallMethod::Cargo => "cargo".into(),
            InstallMethod::Development => "development".into(),
            InstallMethod::Standalone { .. } => "standalone".into(),
            InstallMethod::Unknown { .. } => "unknown".into(),
        }
    }

    /// Shell command the user (or `update`) should run to upgrade, if any.
    pub fn update_command(&self) -> Option<String> {
        let specs = update_package_specs().join(" ");
        match self {
            InstallMethod::NodeGlobal { pm } => Some(js_global_update_cmdline(*pm, &specs)),
            InstallMethod::NodeLocal { pm, package_root } => Some(format!(
                "{}  # in {}",
                js_local_update_cmdline(*pm, &specs),
                package_root.display()
            )),
            // Prefer crates.io once published; git always works today.
            InstallMethod::Cargo => Some(format!("cargo install --git {CARGO_GIT_URL} --force")),
            InstallMethod::Development => None,
            InstallMethod::Standalone { exe_path } => Some(format!(
                "openplanet-lsp update  # replace {}",
                exe_path.display()
            )),
            InstallMethod::Unknown { .. } => Some(format!(
                "npm install -g {specs}  # or openplanet-lsp update if standalone"
            )),
        }
    }

    pub fn can_auto_apply(&self) -> bool {
        matches!(
            self,
            InstallMethod::NodeGlobal { .. }
                | InstallMethod::NodeLocal { .. }
                | InstallMethod::Cargo
                | InstallMethod::Standalone { .. }
        )
    }
}

fn js_global_update_cmdline(pm: JsPackageManager, specs: &str) -> String {
    match pm {
        JsPackageManager::Npm => format!("npm install -g {specs}"),
        JsPackageManager::Pnpm => format!("pnpm add -g {specs}"),
        JsPackageManager::Yarn => format!("yarn global add {specs}"),
        JsPackageManager::Bun => format!("bun add -g {specs}"),
    }
}

fn js_local_update_cmdline(pm: JsPackageManager, specs: &str) -> String {
    match pm {
        JsPackageManager::Npm => format!("npm install {specs}"),
        JsPackageManager::Pnpm => format!("pnpm add {specs}"),
        JsPackageManager::Yarn => format!("yarn add {specs}"),
        JsPackageManager::Bun => format!("bun add {specs}"),
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
    /// Install finished; this process still runs the old binary until restart.
    #[serde(default)]
    pub pending_restart: bool,
    /// Version that was installed on disk (when known).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    /// Where `latest_version` was resolved (`npm`, `crate`, `github`).
    #[serde(default = "default_version_source_label")]
    pub version_source: String,
}

fn default_version_source_label() -> String {
    VersionSource::Npm.as_str().to_string()
}

/// Which channel to query for the latest published version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VersionSource {
    /// npm registry (`registry.npmjs.org/openplanet-lsp/latest`). Default.
    #[default]
    Npm,
    /// crates.io crate max version.
    Crate,
    /// Latest GitHub Release tag for this repo.
    Github,
}

impl VersionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            VersionSource::Npm => "npm",
            VersionSource::Crate => "crate",
            VersionSource::Github => "github",
        }
    }

    /// Parse CLI / config labels: `npm`, `crate`/`crates`/`cargo`, `github`/`gh`.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "npm" => Ok(VersionSource::Npm),
            "crate" | "crates" | "crates.io" | "cargo" => Ok(VersionSource::Crate),
            "github" | "gh" | "git" => Ok(VersionSource::Github),
            other => Err(format!(
                "unknown version source: {other:?} (expected npm, crate, or github)"
            )),
        }
    }
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
    /// Print last saved status without contacting the network.
    pub status_only: bool,
    /// Reinstall even when already on the latest reported version.
    pub force_install: bool,
    /// Channel used to resolve the latest version (default: npm).
    pub version_source: VersionSource,
}

/// CLI/run outcome: text plus process exit code.
#[derive(Debug, Clone)]
pub struct UpdateReport {
    pub text: String,
    /// 0 = ok / up-to-date / applied; 3 = update available but not applied.
    pub exit_code: i32,
}

// ── public entry points ────────────────────────────────────────────────────

/// Effective package version used for update comparisons.
///
/// Override with `OPENPLANET_LSP_VERSION` (CI/dev: pretend to be an older build).
/// `--version` still prints the real `CARGO_PKG_VERSION`.
pub fn current_version() -> String {
    env_nonempty("OPENPLANET_LSP_VERSION").unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

/// npm install target(s) for apply. Default: `openplanet-lsp@latest`.
///
/// Override with `OPENPLANET_LSP_UPDATE_PACKAGE` (whitespace-separated specs), e.g.
/// a local `.tgz` path for smoke tests.
pub fn update_package_specs() -> Vec<String> {
    match env_nonempty("OPENPLANET_LSP_UPDATE_PACKAGE") {
        Some(raw) => raw
            .split_whitespace()
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .collect(),
        None => vec![format!("{PACKAGE_NAME}@latest")],
    }
}

/// Backward-compatible alias.
pub fn npm_update_package_specs() -> Vec<String> {
    update_package_specs()
}

/// Resolve the user config directory.
///
/// Order: `OPENPLANET_LSP_CONFIG_DIR` → `XDG_CONFIG_HOME` → `%APPDATA%` (Windows)
/// → `~/.config` via `HOME` / `USERPROFILE`.
pub fn config_dir() -> Result<PathBuf, UpdateError> {
    if let Ok(dir) = std::env::var("OPENPLANET_LSP_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join(PACKAGE_NAME));
        }
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        if !appdata.is_empty() {
            return Ok(PathBuf::from(appdata).join(PACKAGE_NAME));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return Ok(PathBuf::from(home).join(".config").join(PACKAGE_NAME));
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return Ok(PathBuf::from(profile).join(".config").join(PACKAGE_NAME));
    }
    Err(UpdateError::msg(
        "cannot resolve config dir: set OPENPLANET_LSP_CONFIG_DIR, XDG_CONFIG_HOME, APPDATA, HOME, or USERPROFILE",
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
    let tmp = dir.join(format!("{STATUS_FILE_NAME}.tmp"));
    let text = serde_json::to_string_pretty(status)
        .map_err(|e| UpdateError::msg(format!("failed to serialize update status: {e}")))?;
    fs::write(&tmp, text + "\n")
        .map_err(|e| UpdateError::msg(format!("failed to write {}: {e}", tmp.display())))?;
    // Replace atomically when the OS allows; fall back to remove+rename on Windows.
    if let Err(e) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&path);
        fs::rename(&tmp, &path).map_err(|e2| {
            UpdateError::msg(format!(
                "failed to finalize {}: {e}; retry: {e2}",
                path.display()
            ))
        })?;
    }
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
    let path_str = exe.to_string_lossy();

    // Global JS PM shims / stores (before generic node_modules handling).
    if let Some(pm) = detect_global_js_pm_from_path(&path_str, &components) {
        return InstallMethod::NodeGlobal { pm };
    }

    // JS package layout:
    //   .../node_modules/openplanet-lsp-<plat>/bin/openplanet-lsp
    //   .../node_modules/openplanet-lsp/node_modules/openplanet-lsp-<plat>/bin/...
    //   pnpm virtual store: .../node_modules/.pnpm/.../node_modules/openplanet-lsp-*/
    if let Some(nm_idx) = components.iter().rposition(|c| c == "node_modules") {
        let package_root = infer_npm_package_root(exe, nm_idx, &components);
        let package_root = package_root.unwrap_or_else(|| {
            exe.parent()
                .and_then(|p| p.parent())
                .unwrap_or(exe)
                .to_path_buf()
        });
        if is_node_global_path(exe) {
            let pm = detect_js_pm(exe, Some(&package_root), true);
            return InstallMethod::NodeGlobal { pm };
        }
        let pm = detect_js_pm(exe, Some(&package_root), false);
        return InstallMethod::NodeLocal { pm, package_root };
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

/// Fetch the latest published version from the chosen channel.
///
/// Override with `OPENPLANET_LSP_LATEST_VERSION` to skip the network (CI/dev).
pub fn fetch_latest_version() -> Result<String, UpdateError> {
    fetch_latest_version_from(VersionSource::Npm)
}

/// Fetch latest version from `source` (`npm` / `crate` / `github`).
pub fn fetch_latest_version_from(source: VersionSource) -> Result<String, UpdateError> {
    if let Some(v) = env_nonempty("OPENPLANET_LSP_LATEST_VERSION") {
        if parse_version(&v).is_none() {
            return Err(UpdateError::msg(format!(
                "OPENPLANET_LSP_LATEST_VERSION is not semver-like: {v}"
            )));
        }
        return Ok(v);
    }
    match source {
        VersionSource::Npm => fetch_latest_npm(),
        VersionSource::Crate => fetch_latest_crate(),
        VersionSource::Github => fetch_latest_github(),
    }
}

fn fetch_latest_npm() -> Result<String, UpdateError> {
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

fn fetch_latest_crate() -> Result<String, UpdateError> {
    let url = format!("https://crates.io/api/v1/crates/{PACKAGE_NAME}");
    let body = curl_json_get(&url)?;
    let value: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| UpdateError::msg(format!("invalid crates.io JSON: {e}")))?;
    let version = value
        .pointer("/crate/max_version")
        .and_then(|v| v.as_str())
        .or_else(|| {
            value
                .pointer("/crate/newest_version")
                .and_then(|v| v.as_str())
        })
        .ok_or_else(|| UpdateError::msg("crates.io JSON missing crate.max_version"))?;
    if parse_version(version).is_none() {
        return Err(UpdateError::msg(format!(
            "crates.io version not semver-like: {version}"
        )));
    }
    Ok(version.to_string())
}

fn fetch_latest_github() -> Result<String, UpdateError> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let body = curl_json_get(&url)?;
    let value: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| UpdateError::msg(format!("invalid GitHub releases JSON: {e}")))?;
    let tag = value
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| UpdateError::msg("GitHub releases JSON missing tag_name"))?;
    let version = tag.trim().trim_start_matches('v');
    if parse_version(version).is_none() {
        return Err(UpdateError::msg(format!(
            "GitHub release tag not semver-like: {tag}"
        )));
    }
    Ok(version.to_string())
}

fn curl_json_get(url: &str) -> Result<Vec<u8>, UpdateError> {
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "15",
            "-H",
            "Accept: application/json",
            "-A",
            "openplanet-lsp-update/1.0 (https://github.com/clankercode/lsp-openplanet)",
            url,
        ])
        .output()
        .map_err(|e| UpdateError::msg(format!("curl failed: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(UpdateError::msg(format!(
            "curl exited {} for {url}: {stderr}",
            output.status
        )));
    }
    Ok(output.stdout)
}

/// Run a version check, always writing the status file (npm channel).
pub fn check_for_update() -> Result<UpdateStatus, UpdateError> {
    check_for_update_from(VersionSource::Npm)
}

/// Run a version check against `source`, always writing the status file.
pub fn check_for_update_from(source: VersionSource) -> Result<UpdateStatus, UpdateError> {
    let current = current_version();
    let method = detect_install_method();
    let exe = resolve_exe_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".into());

    let (latest, error) = match fetch_latest_version_from(source) {
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
        install_method: method.as_str(),
        exe_path: exe,
        update_command: method.update_command(),
        error,
        pending_restart: false,
        installed_version: None,
        version_source: source.as_str().to_string(),
    };
    save_status(&status)?;
    Ok(status)
}

/// Whether an automatic (LSP startup) check should run given the last status.
pub fn should_auto_check(last: Option<&UpdateStatus>, interval: Duration) -> bool {
    let Some(last) = last else {
        return true;
    };
    let running = current_version();
    // New binary after upgrade/restart — refresh status immediately.
    if last.current_version != running {
        return true;
    }
    // Install finished earlier; this process is now the installed version.
    if last.pending_restart && last.installed_version.as_deref() == Some(running.as_str()) {
        return true;
    }
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
        InstallMethod::NodeGlobal { pm } => {
            apply_js_pm(*pm, true, None).map_err(annotate_replace_failure)?;
        }
        InstallMethod::NodeLocal { pm, package_root } => {
            apply_js_pm(*pm, false, Some(package_root)).map_err(annotate_replace_failure)?;
        }
        InstallMethod::Cargo => {
            run_command(
                "cargo",
                &["install", "--git", CARGO_GIT_URL, "--force"],
                None,
            )
            .map_err(annotate_replace_failure)?;
        }
        InstallMethod::Standalone { exe_path } => {
            let version = status.latest_version.as_deref().ok_or_else(|| {
                UpdateError::msg("no latest version available for standalone update")
            })?;
            apply_standalone(exe_path, version).map_err(annotate_replace_failure)?;
        }
        _ => unreachable!("can_auto_apply guards apply arms"),
    }

    // Running process still has the old binary mapped. Record install outcome
    // explicitly instead of re-deriving "update available" from CARGO_PKG_VERSION.
    let installed = status
        .latest_version
        .clone()
        .unwrap_or_else(current_version);
    let now = now_epoch_secs();
    let after = UpdateStatus {
        checked_at: now,
        checked_at_rfc3339: epoch_to_rfc3339(now),
        current_version: current_version(),
        latest_version: status.latest_version.clone(),
        update_available: false,
        install_method: method.as_str(),
        exe_path: status.exe_path.clone(),
        update_command: method.update_command(),
        error: None,
        pending_restart: true,
        installed_version: Some(installed),
        version_source: status.version_source.clone(),
    };
    save_status(&after)?;
    Ok(after)
}

/// Format a human-readable status report.
pub fn format_status(status: &UpdateStatus) -> String {
    format_status_with(status, crate::term::color_stdout())
}

/// Format status; `color` forces ANSI on/off (tests / screenshots).
pub fn format_status_with(status: &UpdateStatus, color: bool) -> String {
    use crate::term;

    let mut out = String::new();
    out.push_str(&format!(
        "current:  {} {}\n",
        term::bold(color, &status.current_version),
        term::info(color, format!("(install type: {})", status.install_method))
    ));
    match &status.latest_version {
        Some(v) => {
            let src = if status.version_source.is_empty() {
                "npm"
            } else {
                status.version_source.as_str()
            };
            out.push_str(&format!(
                "latest:   {} {}\n",
                term::bold(color, v),
                term::dim(color, format!("(source checked: {src})"))
            ));
        }
        None => out.push_str(&format!(
            "latest:   {}\n",
            term::warning(color, "(unavailable)")
        )),
    }
    if let Some(inst) = &status.installed_version {
        out.push_str(&format!("installed: {}\n", term::ok(color, inst)));
    }
    out.push_str(&format!(
        "method:   {}\n",
        term::info(color, &status.install_method)
    ));
    out.push_str(&format!(
        "exe:      {}\n",
        term::path(color, &status.exe_path)
    ));
    out.push_str(&format!(
        "checked:  {}\n",
        term::dim(color, &status.checked_at_rfc3339)
    ));
    if status.pending_restart {
        out.push_str(&format!(
            "status:   {}\n",
            term::warning(color, "installed — restart required")
        ));
    } else if status.update_available {
        out.push_str(&format!(
            "status:   {}\n",
            term::warning(color, "update available")
        ));
        if let Some(cmd) = &status.update_command {
            out.push_str(&format!("update:   {}\n", term::ok(color, cmd)));
        }
    } else if status.error.is_some() {
        out.push_str(&format!(
            "status:   {}\n",
            term::error(color, "check failed")
        ));
    } else {
        out.push_str(&format!("status:   {}\n", term::ok(color, "up to date")));
    }
    if let Some(err) = &status.error {
        out.push_str(&format!("error:    {}\n", term::error(color, err)));
    }
    if let Ok(path) = status_path() {
        out.push_str(&format!(
            "file:     {}\n",
            term::dim(color, path.display().to_string())
        ));
    }
    out
}

/// CLI entry: parse is done by main; this runs the chosen action.
pub fn run_update(options: &UpdateOptions) -> Result<UpdateReport, UpdateError> {
    if options.status_only {
        return match load_status()? {
            Some(status) => Ok(UpdateReport {
                text: format_status(&status),
                exit_code: 0,
            }),
            None => Ok(UpdateReport {
                text: "no update status saved yet; run `openplanet-lsp update --check`\n".into(),
                exit_code: 0,
            }),
        };
    }

    if options.check_only {
        let status = check_for_update_from(options.version_source)?;
        return Ok(UpdateReport {
            text: format_status(&status),
            exit_code: 0,
        });
    }

    // Full update path
    let method = detect_install_method();
    let before = check_for_update_from(options.version_source)?;
    if let Some(err) = &before.error {
        return Err(UpdateError::msg(err.clone()));
    }
    if !before.update_available && !options.force_install {
        return Ok(UpdateReport {
            text: format_status(&before),
            exit_code: 0,
        });
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
        return Ok(UpdateReport {
            text: report,
            // Scripts can distinguish "needs manual action" from success.
            exit_code: if before.update_available { 3 } else { 0 },
        });
    }

    let after = apply_update_with_status(&method, &before, options.force_install)?;
    let mut report = String::new();
    report.push_str("Update command finished.\n");
    report.push_str(
        "Restart openplanet-lsp (and your editor language client) to use the new binary.\n",
    );
    report.push_str(
        "If install failed on Windows because the file was locked, stop the language server first.\n\n",
    );
    report.push_str(&format_status(&after));
    Ok(UpdateReport {
        text: report,
        exit_code: 0,
    })
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

fn is_node_global_path(exe: &Path) -> bool {
    // Heuristics: global prefix layouts and package-manager `root -g` matches.
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
        "/.local/share/pnpm/",
        "/.local/lib/node_modules/",
        // Default Windows Node installer layout
        "\\nodejs\\node_modules\\",
        "/nodejs/node_modules/",
        "Program Files\\nodejs\\",
        "Program Files/nodejs/",
        "Program Files (x86)\\nodejs\\",
        "Program Files (x86)/nodejs/",
        // yarn classic global
        "/.config/yarn/global/",
        "\\.config\\yarn\\global\\",
        "/.yarn/global/",
        "\\.yarn\\global\\",
        "AppData\\Local\\Yarn\\Data\\global\\",
        "AppData/Local/Yarn/Data/global/",
        // bun global
        "/.bun/install/global/",
        "\\.bun\\install\\global\\",
    ];
    if global_markers.iter().any(|m| s.contains(m)) {
        return true;
    }

    for root in [
        npm_global_root(),
        pnpm_global_root(),
        yarn_global_root(),
        bun_global_root(),
    ]
    .into_iter()
    .flatten()
    {
        if s.contains(&*root.to_string_lossy()) {
            return true;
        }
    }
    false
}

/// Detect PM from well-known global shim/store paths (no node_modules required).
fn detect_global_js_pm_from_path(
    path_str: &str,
    components: &[String],
) -> Option<JsPackageManager> {
    if path_str.contains("/.bun/") || path_str.contains("\\.bun\\") {
        return Some(JsPackageManager::Bun);
    }
    if path_str.contains("/.local/share/pnpm/")
        || path_str.contains("\\.local\\share\\pnpm\\")
        || path_str.contains("/pnpm/global/")
        || path_str.contains("\\pnpm\\global\\")
    {
        return Some(JsPackageManager::Pnpm);
    }
    if path_str.contains("/.config/yarn/global/")
        || path_str.contains("\\.config\\yarn\\global\\")
        || path_str.contains("/.yarn/global/")
        || path_str.contains("\\.yarn\\global\\")
        || path_str.contains("Yarn\\Data\\global")
        || path_str.contains("Yarn/Data/global")
    {
        return Some(JsPackageManager::Yarn);
    }
    // bun/pnpm often put user-facing shims in */.bun/bin or */.local/share/pnpm
    if components
        .windows(2)
        .any(|w| w[0] == ".bun" && w[1] == "bin")
    {
        return Some(JsPackageManager::Bun);
    }
    if components
        .windows(3)
        .any(|w| w[0] == ".local" && w[1] == "share" && w[2] == "pnpm")
    {
        return Some(JsPackageManager::Pnpm);
    }
    None
}

/// Choose npm/pnpm/yarn/bun for a node install.
///
/// Order: `OPENPLANET_LSP_PACKAGE_MANAGER` override → path heuristics →
/// lockfiles / node_modules layout at package root → global root match → npm.
fn detect_js_pm(exe: &Path, package_root: Option<&Path>, global: bool) -> JsPackageManager {
    if let Some(raw) = env_nonempty("OPENPLANET_LSP_PACKAGE_MANAGER") {
        if let Some(pm) = JsPackageManager::parse(&raw) {
            return pm;
        }
    }

    let s = exe.to_string_lossy();
    if let Some(pm) = detect_global_js_pm_from_path(&s, &path_components_lossy(exe)) {
        return pm;
    }
    if s.contains("/.pnpm/") || s.contains("\\.pnpm\\") || s.contains("node_modules/.pnpm") {
        return JsPackageManager::Pnpm;
    }
    if s.contains("/.yarn/") || s.contains("\\.yarn\\") {
        return JsPackageManager::Yarn;
    }

    if let Some(root) = package_root {
        if let Some(pm) = detect_js_pm_from_package_root(root) {
            return pm;
        }
    }

    if global {
        if let Some(root) = pnpm_global_root() {
            if s.contains(&*root.to_string_lossy()) {
                return JsPackageManager::Pnpm;
            }
        }
        if let Some(root) = yarn_global_root() {
            if s.contains(&*root.to_string_lossy()) {
                return JsPackageManager::Yarn;
            }
        }
        if let Some(root) = bun_global_root() {
            if s.contains(&*root.to_string_lossy()) {
                return JsPackageManager::Bun;
            }
        }
        if let Some(root) = npm_global_root() {
            if s.contains(&*root.to_string_lossy()) {
                return JsPackageManager::Npm;
            }
        }
    }
    JsPackageManager::Npm
}

fn detect_js_pm_from_package_root(root: &Path) -> Option<JsPackageManager> {
    // Lockfiles (most reliable for project-local installs).
    if root.join("bun.lockb").is_file() || root.join("bun.lock").is_file() {
        return Some(JsPackageManager::Bun);
    }
    if root.join("pnpm-lock.yaml").is_file() {
        return Some(JsPackageManager::Pnpm);
    }
    if root.join("yarn.lock").is_file() {
        return Some(JsPackageManager::Yarn);
    }
    if root.join("package-lock.json").is_file() || root.join("npm-shrinkwrap.json").is_file() {
        return Some(JsPackageManager::Npm);
    }
    // Layout hints when lockfile is absent / gitignored.
    if root.join("node_modules/.pnpm").is_dir() {
        return Some(JsPackageManager::Pnpm);
    }
    if root.join(".yarn").is_dir() || root.join(".pnp.cjs").is_file() {
        return Some(JsPackageManager::Yarn);
    }
    None
}

/// Download (or open a local archive) the GitHub Release asset for this host
/// and atomically replace `exe_path`.
///
/// Best-practice notes:
/// - Fetch into a temp dir; never stream directly over the live binary.
/// - Extract the platform binary from the release layout (root of tar.gz/zip).
/// - Write to `exe_path.new`, fsync when possible, then rename into place so a
///   crash mid-write cannot leave a truncated executable.
/// - On Windows, rename the running binary aside first (cannot overwrite a
///   mapped PE image).
///
/// Dev/CI: set `OPENPLANET_LSP_RELEASE_ARCHIVE` to a local `.tar.gz` / `.zip`
/// to skip the network.
fn apply_standalone(exe_path: &Path, version: &str) -> Result<(), UpdateError> {
    let version = version.trim().trim_start_matches('v');
    if parse_version(version).is_none() {
        return Err(UpdateError::msg(format!(
            "invalid version for standalone update: {version:?}"
        )));
    }

    let tmp = std::env::temp_dir().join(format!(
        "openplanet-lsp-update-{}-{}",
        version,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)
        .map_err(|e| UpdateError::msg(format!("create temp dir {}: {e}", tmp.display())))?;

    let result = (|| {
        let (archive_path, cleanup_download) = resolve_release_archive(version, &tmp)?;
        let extract_dir = tmp.join("extract");
        fs::create_dir_all(&extract_dir).map_err(|e| {
            UpdateError::msg(format!("create extract dir {}: {e}", extract_dir.display()))
        })?;
        extract_release_archive(&archive_path, &extract_dir)?;
        if cleanup_download {
            let _ = fs::remove_file(&archive_path);
        }
        let new_bin = find_binary_in_extract(&extract_dir)?;
        replace_executable(exe_path, &new_bin)
    })();

    let _ = fs::remove_dir_all(&tmp);
    result
}

/// Returns `(archive_path, delete_after)`.
fn resolve_release_archive(version: &str, tmp: &Path) -> Result<(PathBuf, bool), UpdateError> {
    if let Some(local) = env_nonempty("OPENPLANET_LSP_RELEASE_ARCHIVE") {
        let p = PathBuf::from(local);
        if !p.is_file() {
            return Err(UpdateError::msg(format!(
                "OPENPLANET_LSP_RELEASE_ARCHIVE is not a file: {}",
                p.display()
            )));
        }
        return Ok((p, false));
    }

    let (target, ext) = host_release_target()?;
    let asset = format!("openplanet-lsp-v{version}-{target}.{ext}");
    let url = format!("https://github.com/{GITHUB_REPO}/releases/download/v{version}/{asset}");
    let dest = tmp.join(&asset);
    download_file(&url, &dest)?;
    Ok((dest, true))
}

/// Rust target triple + archive extension for the running host.
fn host_release_target() -> Result<(&'static str, &'static str), UpdateError> {
    // Keep in lockstep with .github/workflows/release.yml matrix targets.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Ok(("x86_64-unknown-linux-gnu", "tar.gz"));
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        return Ok(("aarch64-unknown-linux-gnu", "tar.gz"));
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        return Ok(("x86_64-apple-darwin", "tar.gz"));
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Ok(("aarch64-apple-darwin", "tar.gz"));
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Ok(("x86_64-pc-windows-msvc", "zip"));
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        return Ok(("aarch64-pc-windows-msvc", "zip"));
    }
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
    )))]
    {
        Err(UpdateError::msg(format!(
            "standalone self-update is not supported on this host ({}-{})",
            std::env::consts::OS,
            std::env::consts::ARCH
        )))
    }
}

fn download_file(url: &str, dest: &Path) -> Result<(), UpdateError> {
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "120",
            "-o",
            dest.to_str().ok_or_else(|| {
                UpdateError::msg(format!("non-utf8 download path: {}", dest.display()))
            })?,
            url,
        ])
        .output()
        .map_err(|e| UpdateError::msg(format!("curl download failed: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(UpdateError::msg(format!(
            "curl download exited {} for {url}: {stderr}",
            output.status
        )));
    }
    if !dest.is_file() {
        return Err(UpdateError::msg(format!(
            "download produced no file at {}",
            dest.display()
        )));
    }
    Ok(())
}

fn extract_release_archive(archive: &Path, dest_dir: &Path) -> Result<(), UpdateError> {
    let name = archive
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let dest = dest_dir
        .to_str()
        .ok_or_else(|| UpdateError::msg("non-utf8 extract dir"))?;
    let arch = archive
        .to_str()
        .ok_or_else(|| UpdateError::msg("non-utf8 archive path"))?;

    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let output = Command::new("tar")
            .args(["-xzf", arch, "-C", dest])
            .output()
            .map_err(|e| UpdateError::msg(format!("tar extract failed: {e}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(UpdateError::msg(format!(
                "tar exited {}: {stderr}",
                output.status
            )));
        }
        return Ok(());
    }

    if name.ends_with(".zip") {
        // Prefer `unzip` when present; fall back to PowerShell on Windows.
        if Command::new("unzip").arg("-h").output().is_ok() {
            let output = Command::new("unzip")
                .args(["-o", arch, "-d", dest])
                .output()
                .map_err(|e| UpdateError::msg(format!("unzip failed: {e}")))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(UpdateError::msg(format!(
                    "unzip exited {}: {stderr}",
                    output.status
                )));
            }
            return Ok(());
        }
        #[cfg(windows)]
        {
            let ps = format!(
                "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
                arch.replace('\'', "''"),
                dest.replace('\'', "''")
            );
            let output = Command::new("powershell")
                .args(["-NoProfile", "-Command", &ps])
                .output()
                .map_err(|e| UpdateError::msg(format!("powershell Expand-Archive failed: {e}")))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(UpdateError::msg(format!(
                    "Expand-Archive exited {}: {stderr}",
                    output.status
                )));
            }
            return Ok(());
        }
        #[cfg(not(windows))]
        {
            return Err(UpdateError::msg(
                "cannot extract .zip: `unzip` not found on PATH",
            ));
        }
    }

    Err(UpdateError::msg(format!(
        "unsupported release archive format: {}",
        archive.display()
    )))
}

fn find_binary_in_extract(dir: &Path) -> Result<PathBuf, UpdateError> {
    #[cfg(windows)]
    const WANT: &str = "openplanet-lsp.exe";
    #[cfg(not(windows))]
    const WANT: &str = "openplanet-lsp";

    // Prefer top-level match (release archives place the binary at root).
    let direct = dir.join(WANT);
    if direct.is_file() {
        return Ok(direct);
    }

    // Fall back to a shallow walk for nested layouts.
    let mut found = Vec::new();
    fn walk(dir: &Path, want: &str, out: &mut Vec<PathBuf>, depth: usize) {
        if depth > 4 {
            return;
        }
        let Ok(rd) = fs::read_dir(dir) else {
            return;
        };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                walk(&p, want, out, depth + 1);
            } else if p.file_name().and_then(|s| s.to_str()) == Some(want) {
                out.push(p);
            }
        }
    }
    walk(dir, WANT, &mut found, 0);
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(UpdateError::msg(format!(
            "release archive did not contain `{WANT}`"
        ))),
        many => Err(UpdateError::msg(format!(
            "release archive contained multiple `{WANT}` files ({})",
            many.len()
        ))),
    }
}

/// Atomically replace `target` with the contents of `new_bin`.
fn replace_executable(target: &Path, new_bin: &Path) -> Result<(), UpdateError> {
    if !new_bin.is_file() {
        return Err(UpdateError::msg(format!(
            "new binary missing: {}",
            new_bin.display()
        )));
    }
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let staging = parent.join(format!(
        "{}.new.{}",
        target
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("openplanet-lsp"),
        std::process::id()
    ));

    fs::copy(new_bin, &staging)
        .map_err(|e| UpdateError::msg(format!("copy new binary to {}: {e}", staging.display())))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&staging)
            .map_err(|e| UpdateError::msg(format!("stat staging: {e}")))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&staging, perms)
            .map_err(|e| UpdateError::msg(format!("chmod staging: {e}")))?;
    }

    #[cfg(windows)]
    {
        // Running PE cannot be overwritten in-place. Move it aside first.
        if target.exists() {
            let bak = parent.join(format!(
                "{}.old.{}",
                target
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("openplanet-lsp"),
                std::process::id()
            ));
            let _ = fs::remove_file(&bak);
            fs::rename(target, &bak).map_err(|e| {
                UpdateError::msg(format!(
                    "could not move running binary aside ({} → {}): {e}",
                    target.display(),
                    bak.display()
                ))
            })?;
            // Best-effort cleanup; may fail while this process still holds the mapping.
            let _ = fs::remove_file(&bak);
        }
    }

    fs::rename(&staging, target).map_err(|e| {
        // Try to leave staging behind rather than delete on failure.
        UpdateError::msg(format!(
            "could not install new binary at {}: {e}",
            target.display()
        ))
    })?;

    Ok(())
}

fn apply_js_pm(
    pm: JsPackageManager,
    global: bool,
    package_root: Option<&Path>,
) -> Result<(), UpdateError> {
    let specs = update_package_specs();
    if specs.is_empty() {
        return Err(UpdateError::msg("no package specs to install"));
    }
    let mut args: Vec<String> = Vec::new();
    match pm {
        JsPackageManager::Npm => {
            args.push("install".into());
            if global {
                args.push("-g".into());
            }
            args.extend(specs);
            run_command_owned("npm", &args, package_root)
        }
        JsPackageManager::Pnpm => {
            args.push("add".into());
            if global {
                args.push("-g".into());
            }
            args.extend(specs);
            run_command_owned("pnpm", &args, package_root)
        }
        JsPackageManager::Yarn => {
            if global {
                args.push("global".into());
                args.push("add".into());
                args.extend(specs);
                run_command_owned("yarn", &args, None)
            } else {
                args.push("add".into());
                args.extend(specs);
                run_command_owned("yarn", &args, package_root)
            }
        }
        JsPackageManager::Bun => {
            args.push("add".into());
            if global {
                args.push("-g".into());
            }
            args.extend(specs);
            run_command_owned("bun", &args, package_root)
        }
    }
}

fn npm_global_root() -> Option<PathBuf> {
    cached_cmd_path("npm-root-g", || {
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
    })
}

fn pnpm_global_root() -> Option<PathBuf> {
    cached_cmd_path("pnpm-root-g", || {
        let output = Command::new("pnpm").args(["root", "-g"]).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if root.is_empty() {
            None
        } else {
            Some(PathBuf::from(root))
        }
    })
}

fn yarn_global_root() -> Option<PathBuf> {
    cached_cmd_path("yarn-global-folder", || {
        // Yarn classic: `yarn global dir` → …/global ; packages under node_modules there.
        // Yarn berry may fail — ignore.
        let output = Command::new("yarn").args(["global", "dir"]).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if root.is_empty() {
            None
        } else {
            let p = PathBuf::from(root);
            let nm = p.join("node_modules");
            Some(if nm.is_dir() { nm } else { p })
        }
    })
}

fn bun_global_root() -> Option<PathBuf> {
    cached_cmd_path("bun-global", || {
        if let Ok(home) = std::env::var("HOME") {
            let p = PathBuf::from(&home).join(".bun/install/global/node_modules");
            if p.is_dir() {
                return Some(p);
            }
        }
        if let Ok(profile) = std::env::var("USERPROFILE") {
            let p = PathBuf::from(&profile).join(".bun/install/global/node_modules");
            if p.is_dir() {
                return Some(p);
            }
        }
        None
    })
}

fn cached_cmd_path(
    key: &'static str,
    compute: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<&'static str, Option<PathBuf>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().ok()?;
    if let Some(v) = guard.get(key) {
        return v.clone();
    }
    let v = compute();
    guard.insert(key, v.clone());
    v
}

fn annotate_replace_failure(err: UpdateError) -> UpdateError {
    UpdateError::msg(format!(
        "{err}\nHint: if the package manager could not replace the running binary \
         (common on Windows), stop openplanet-lsp / your editor language client and retry."
    ))
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
    let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    run_command_owned(program, &owned, cwd)
}

fn run_command_owned(
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
) -> Result<(), UpdateError> {
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

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().and_then(|v| {
        let t = v.trim().to_string();
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    })
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
            InstallMethod::NodeGlobal {
                pm: JsPackageManager::Npm
            }
        );
    }

    #[test]
    fn detect_npm_local_layout() {
        let path = PathBuf::from(
            "/work/my-plugin/node_modules/openplanet-lsp-linux-x64/bin/openplanet-lsp",
        );
        match detect_install_method_from_path(&path) {
            InstallMethod::NodeLocal {
                pm: JsPackageManager::Npm,
                package_root,
            } => {
                assert_eq!(package_root, PathBuf::from("/work/my-plugin"));
            }
            other => panic!("expected NodeLocal npm, got {other:?}"),
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
        assert!(InstallMethod::NodeGlobal {
            pm: JsPackageManager::Npm
        }
        .update_command()
        .unwrap()
        .contains("npm install -g"));
        assert!(InstallMethod::NodeGlobal {
            pm: JsPackageManager::Pnpm
        }
        .update_command()
        .unwrap()
        .contains("pnpm add -g"));
        assert!(InstallMethod::NodeGlobal {
            pm: JsPackageManager::Yarn
        }
        .update_command()
        .unwrap()
        .contains("yarn global add"));
        assert!(InstallMethod::NodeGlobal {
            pm: JsPackageManager::Bun
        }
        .update_command()
        .unwrap()
        .contains("bun add -g"));
        assert!(InstallMethod::NodeLocal {
            pm: JsPackageManager::Pnpm,
            package_root: PathBuf::from("/app"),
        }
        .update_command()
        .unwrap()
        .contains("pnpm add "));
        assert!(InstallMethod::Cargo
            .update_command()
            .unwrap()
            .contains("cargo install --git"));
        assert!(!InstallMethod::Cargo
            .update_command()
            .unwrap()
            .contains("--locked"));
        assert!(InstallMethod::Development.update_command().is_none());
        assert!(InstallMethod::NodeGlobal {
            pm: JsPackageManager::Yarn
        }
        .can_auto_apply());
        assert!(InstallMethod::Standalone {
            exe_path: PathBuf::from("/opt/openplanet-lsp")
        }
        .can_auto_apply());
        assert!(InstallMethod::Standalone {
            exe_path: PathBuf::from("/opt/openplanet-lsp")
        }
        .update_command()
        .unwrap()
        .contains("openplanet-lsp update"));
        assert!(!InstallMethod::Development.can_auto_apply());
    }

    #[test]
    fn detect_pnpm_bun_yarn_global_paths() {
        let pnpm = PathBuf::from(
            "/home/u/.local/share/pnpm/global/5/node_modules/openplanet-lsp-linux-x64/bin/openplanet-lsp",
        );
        assert_eq!(
            detect_install_method_from_path(&pnpm),
            InstallMethod::NodeGlobal {
                pm: JsPackageManager::Pnpm
            }
        );
        let bun = PathBuf::from(
            "/home/u/.bun/install/global/node_modules/openplanet-lsp-linux-x64/bin/openplanet-lsp",
        );
        assert_eq!(
            detect_install_method_from_path(&bun),
            InstallMethod::NodeGlobal {
                pm: JsPackageManager::Bun
            }
        );
        let yarn = PathBuf::from(
            "/home/u/.config/yarn/global/node_modules/openplanet-lsp-linux-x64/bin/openplanet-lsp",
        );
        assert_eq!(
            detect_install_method_from_path(&yarn),
            InstallMethod::NodeGlobal {
                pm: JsPackageManager::Yarn
            }
        );
        let bun_shim = PathBuf::from("/home/u/.bun/bin/openplanet-lsp");
        assert_eq!(
            detect_install_method_from_path(&bun_shim),
            InstallMethod::NodeGlobal {
                pm: JsPackageManager::Bun
            }
        );
    }

    #[test]
    fn detect_local_pm_from_lockfile() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("OPENPLANET_LSP_PACKAGE_MANAGER");
        let dir = std::env::temp_dir().join(format!(
            "oplsp-pm-{}-{}",
            std::process::id(),
            now_epoch_secs()
        ));
        let _ = fs::remove_dir_all(&dir);
        let bin_dir = dir.join("node_modules/openplanet-lsp-linux-x64/bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let exe = bin_dir.join("openplanet-lsp");
        fs::write(&exe, b"x").unwrap();

        fs::write(dir.join("pnpm-lock.yaml"), "lockfileVersion: '9'\n").unwrap();
        match detect_install_method_from_path(&exe) {
            InstallMethod::NodeLocal {
                pm: JsPackageManager::Pnpm,
                package_root,
            } => assert_eq!(package_root, dir),
            other => panic!("expected pnpm-local, got {other:?}"),
        }
        fs::remove_file(dir.join("pnpm-lock.yaml")).unwrap();

        fs::write(dir.join("yarn.lock"), "# yarn\n").unwrap();
        match detect_install_method_from_path(&exe) {
            InstallMethod::NodeLocal {
                pm: JsPackageManager::Yarn,
                ..
            } => {}
            other => panic!("expected yarn-local, got {other:?}"),
        }
        fs::remove_file(dir.join("yarn.lock")).unwrap();

        fs::write(dir.join("bun.lock"), "{}\n").unwrap();
        match detect_install_method_from_path(&exe) {
            InstallMethod::NodeLocal {
                pm: JsPackageManager::Bun,
                ..
            } => {}
            other => panic!("expected bun-local, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn package_manager_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("OPENPLANET_LSP_PACKAGE_MANAGER", "pnpm");
        let path =
            PathBuf::from("/work/app/node_modules/openplanet-lsp-linux-x64/bin/openplanet-lsp");
        match detect_install_method_from_path(&path) {
            InstallMethod::NodeLocal {
                pm: JsPackageManager::Pnpm,
                ..
            } => {}
            other => panic!("expected pnpm via env, got {other:?}"),
        }
        std::env::remove_var("OPENPLANET_LSP_PACKAGE_MANAGER");
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
            pending_restart: false,
            installed_version: None,
            version_source: "npm".into(),
        };
        let path = save_status(&status).unwrap();
        assert!(path.ends_with(STATUS_FILE_NAME));
        let loaded = load_status().unwrap().unwrap();
        assert_eq!(loaded, status);

        std::env::remove_var("OPENPLANET_LSP_CONFIG_DIR");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn should_auto_check_on_version_skew() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("OPENPLANET_LSP_VERSION");
        let last = UpdateStatus {
            checked_at: now_epoch_secs(),
            checked_at_rfc3339: "now".into(),
            current_version: "0.1.0".into(),
            latest_version: Some("0.2.0".into()),
            update_available: true,
            install_method: "npm-global".into(),
            exe_path: "/x".into(),
            update_command: None,
            error: None,
            pending_restart: false,
            installed_version: None,
            version_source: "npm".into(),
        };
        // Running binary is newer than last recorded current → recheck.
        assert_ne!(current_version(), "0.1.0");
        assert!(should_auto_check(
            Some(&last),
            Duration::from_secs(24 * 3600)
        ));
    }

    #[test]
    fn should_auto_check_respects_interval() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("OPENPLANET_LSP_VERSION");
        let fresh = UpdateStatus {
            checked_at: now_epoch_secs(),
            checked_at_rfc3339: "now".into(),
            current_version: current_version(),
            latest_version: Some(current_version()),
            update_available: false,
            install_method: "npm-global".into(),
            exe_path: "/x".into(),
            update_command: None,
            error: None,
            pending_restart: false,
            installed_version: None,
            version_source: "npm".into(),
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
    fn format_status_pending_restart() {
        let status = UpdateStatus {
            checked_at: 0,
            checked_at_rfc3339: "1970-01-01T00:00:00Z".into(),
            current_version: "0.2.0".into(),
            latest_version: Some("0.2.6".into()),
            update_available: false,
            install_method: "npm-global".into(),
            exe_path: "/x".into(),
            update_command: None,
            error: None,
            pending_restart: true,
            installed_version: Some("0.2.6".into()),
            version_source: "npm".into(),
        };
        let text = format_status(&status);
        assert!(text.contains("installed — restart required"));
        assert!(text.contains("installed: 0.2.6"));
        assert!(!text.contains("update available"));
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
            InstallMethod::NodeLocal {
                pm: JsPackageManager::Npm,
                package_root,
            } => {
                assert_eq!(package_root, PathBuf::from("/work/app"));
            }
            other => panic!("expected NodeLocal npm, got {other:?}"),
        }
    }

    #[test]
    fn detect_npm_global_windows_roaming() {
        let path = PathBuf::from(
            r"C:\Users\me\AppData\Roaming\npm\node_modules\openplanet-lsp-win32-x64\bin\openplanet-lsp.exe",
        );
        assert_eq!(
            detect_install_method_from_path(&path),
            InstallMethod::NodeGlobal {
                pm: JsPackageManager::Npm
            }
        );
    }

    #[test]
    fn detect_npm_global_windows_program_files_nodejs() {
        let path = PathBuf::from(
            r"C:\Program Files\nodejs\node_modules\openplanet-lsp-win32-x64\bin\openplanet-lsp.exe",
        );
        assert_eq!(
            detect_install_method_from_path(&path),
            InstallMethod::NodeGlobal {
                pm: JsPackageManager::Npm
            }
        );
    }

    #[test]
    fn env_overrides_version_and_latest() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("OPENPLANET_LSP_VERSION", "0.1.0");
        std::env::set_var("OPENPLANET_LSP_LATEST_VERSION", "9.9.9");
        std::env::set_var("OPENPLANET_LSP_UPDATE_PACKAGE", "./old.tgz ./new.tgz");
        assert_eq!(current_version(), "0.1.0");
        assert_eq!(fetch_latest_version().unwrap(), "9.9.9");
        assert!(is_update_available(
            &current_version(),
            &fetch_latest_version().unwrap()
        ));
        assert_eq!(
            npm_update_package_specs(),
            vec!["./old.tgz".to_string(), "./new.tgz".to_string()]
        );
        let cmd = InstallMethod::NodeGlobal {
            pm: JsPackageManager::Npm,
        }
        .update_command()
        .unwrap();
        assert!(cmd.contains("./old.tgz"));
        assert!(cmd.contains("-g"));
        std::env::remove_var("OPENPLANET_LSP_VERSION");
        std::env::remove_var("OPENPLANET_LSP_LATEST_VERSION");
        std::env::remove_var("OPENPLANET_LSP_UPDATE_PACKAGE");
        std::env::remove_var("OPENPLANET_LSP_PACKAGE_MANAGER");
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
            pending_restart: false,
            installed_version: None,
            version_source: "npm".into(),
        };
        let text = format_status(&status);
        assert!(text.contains("update available"));
        assert!(text.contains("0.2.4"));
        assert!(text.contains("current:  0.2.0 (install type: npm-global)"));
        assert!(text.contains("latest:   0.2.4 (source checked: npm)"));
    }

    #[test]
    fn version_source_parse_aliases() {
        assert_eq!(VersionSource::parse("npm").unwrap(), VersionSource::Npm);
        assert_eq!(VersionSource::parse("crate").unwrap(), VersionSource::Crate);
        assert_eq!(
            VersionSource::parse("crates").unwrap(),
            VersionSource::Crate
        );
        assert_eq!(
            VersionSource::parse("github").unwrap(),
            VersionSource::Github
        );
        assert_eq!(VersionSource::parse("gh").unwrap(), VersionSource::Github);
        assert!(VersionSource::parse("ftp").is_err());
    }

    #[test]
    fn format_status_up_to_date_shows_install_type() {
        let status = UpdateStatus {
            checked_at: 0,
            checked_at_rfc3339: "1970-01-01T00:00:00Z".into(),
            current_version: "0.2.9".into(),
            latest_version: Some("0.2.9".into()),
            update_available: false,
            install_method: "standalone".into(),
            exe_path: "/home/x/.local/bin/openplanet-lsp".into(),
            update_command: Some("openplanet-lsp update".into()),
            error: None,
            pending_restart: false,
            installed_version: None,
            version_source: "github".into(),
        };
        let text = format_status(&status);
        assert!(text.contains("current:  0.2.9 (install type: standalone)"));
        assert!(text.contains("latest:   0.2.9 (source checked: github)"));
        assert!(text.contains("status:   up to date"));
    }

    #[test]
    fn standalone_apply_from_local_archive_replaces_binary() {
        let dir = std::env::temp_dir().join(format!(
            "openplanet-lsp-standalone-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let old_exe = dir.join("openplanet-lsp");
        fs::write(&old_exe, b"OLD_BINARY_CONTENT").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = fs::metadata(&old_exe).unwrap().permissions();
            p.set_mode(0o755);
            fs::set_permissions(&old_exe, p).unwrap();
        }

        // Build a release-shaped tar.gz with a new binary at archive root.
        let stage = dir.join("stage");
        fs::create_dir_all(&stage).unwrap();
        let new_bin = stage.join("openplanet-lsp");
        fs::write(&new_bin, b"NEW_BINARY_CONTENT_V999").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = fs::metadata(&new_bin).unwrap().permissions();
            p.set_mode(0o755);
            fs::set_permissions(&new_bin, p).unwrap();
        }
        // BUILD_INFO.txt like CI archives
        fs::write(stage.join("BUILD_INFO.txt"), "test\n").unwrap();

        let archive = dir.join("openplanet-lsp-v9.9.9-test.tar.gz");
        let status = std::process::Command::new("tar")
            .args([
                "-czf",
                archive.to_str().unwrap(),
                "-C",
                stage.to_str().unwrap(),
                ".",
            ])
            .status()
            .expect("tar available");
        assert!(status.success(), "tar create failed");

        std::env::set_var("OPENPLANET_LSP_RELEASE_ARCHIVE", &archive);
        apply_standalone(&old_exe, "9.9.9").expect("standalone apply");
        std::env::remove_var("OPENPLANET_LSP_RELEASE_ARCHIVE");

        let got = fs::read(&old_exe).unwrap();
        assert_eq!(got, b"NEW_BINARY_CONTENT_V999");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn host_release_target_is_known() {
        // Smoke: current CI host must map to a release asset.
        let (triple, ext) = host_release_target().expect("host supported");
        assert!(!triple.is_empty());
        assert!(ext == "tar.gz" || ext == "zip");
    }
}
