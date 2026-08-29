use serde::Deserialize;
use std::ops::Range;
use std::path::{Path, PathBuf};

/// Parsed info.toml manifest
#[derive(Debug, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub meta: ManifestMeta,
    #[serde(default)]
    pub game: Option<ManifestGame>,
    #[serde(default)]
    pub script: Option<ManifestScript>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ManifestMeta {
    pub name: Option<String>,
    pub version: Option<String>,
    pub author: Option<String>,
    pub category: Option<String>,
    #[serde(default)]
    pub blocks: Vec<String>,
    pub perms: Option<String>,
    pub siteid: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct ManifestGame {
    pub min_version: Option<String>,
    pub max_version: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ManifestScript {
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(default)]
    pub imports: Vec<String>,
    #[serde(default)]
    pub exports: Vec<String>,
    #[serde(default)]
    pub shared_exports: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub optional_dependencies: Vec<String>,
    #[serde(default)]
    pub export_dependencies: Vec<String>,
    #[serde(default)]
    pub defines: Vec<String>,
    #[serde(default)]
    pub module: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ManifestDiagnostic {
    pub message: String,
    pub severity: DiagSeverity,
    /// Where in info.toml the problem lives. Resolved to a byte range by
    /// [`crate::workspace::manifest_span::anchor_range`].
    pub anchor: ManifestAnchor,
}

impl ManifestDiagnostic {
    /// The dotted key path this diagnostic points at, when it points at a
    /// key at all (a TOML parse error carries a raw span instead).
    pub fn key_path(&self) -> Option<&str> {
        match &self.anchor {
            ManifestAnchor::Key { path, .. } => Some(path),
            ManifestAnchor::Span(_) => None,
        }
    }
}

/// Where a [`ManifestDiagnostic`] belongs in the info.toml source.
///
/// `validate` works on the deserialized manifest, which serde strips of all
/// position information, so rules name a key path and the span lookup
/// re-derives the range from the original text.
#[derive(Debug, Clone, PartialEq)]
pub enum ManifestAnchor {
    /// A byte range already known — TOML parse errors report one directly.
    Span(Range<usize>),
    /// A dotted key path (`meta.version`). `index` narrows the anchor to one
    /// element of the array that path holds, when only that entry is at
    /// fault (`script.exports[2]`).
    Key { path: String, index: Option<usize> },
}

impl ManifestAnchor {
    /// Anchor on a whole key.
    pub fn key(path: &str) -> Self {
        Self::Key {
            path: path.to_string(),
            index: None,
        }
    }

    /// Anchor on one element of the array at `path`.
    pub fn element(path: &str, index: usize) -> Self {
        Self::Key {
            path: path.to_string(),
            index: Some(index),
        }
    }
}

/// The filesystem context the `exports` rules need. `root` is the plugin
/// root; `plugin_files_search_paths` is the SAME list the workspace loader
/// resolves plugin files with (`src/` by default) — resolving exports any
/// other way would report a missing file for every plugin that keeps its
/// sources in a subdirectory, which is most of them.
pub struct ManifestWorkspace<'a> {
    pub root: &'a Path,
    pub plugin_files_search_paths: &'a [PathBuf],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DiagSeverity {
    Error,
    Warning,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self, ManifestDiagnostic> {
        let contents = std::fs::read_to_string(path).map_err(|e| ManifestDiagnostic {
            message: format!("Failed to read info.toml: {}", e),
            severity: DiagSeverity::Error,
            anchor: ManifestAnchor::Span(0..0),
        })?;
        Self::parse(&contents)
    }

    pub fn parse(contents: &str) -> Result<Self, ManifestDiagnostic> {
        toml::from_str(contents).map_err(|e| ManifestDiagnostic {
            message: format!("TOML parse error: {}", e),
            severity: DiagSeverity::Error,
            // `toml` reports the offending byte range; keep it so the squiggle
            // lands on the broken line instead of the top of the file.
            anchor: ManifestAnchor::Span(e.span().unwrap_or(0..0)),
        })
    }

    /// Validate the manifest and return all diagnostics.
    ///
    /// `workspace` is `None` when the manifest did not come from a directory
    /// we can stat (a `.op` archive, an unsaved buffer with no path). The
    /// rules that need the filesystem are then skipped rather than guessed
    /// at — reporting "export file not found" for a file we never looked for
    /// would be a false positive on every archived plugin.
    pub fn validate(&self, workspace: Option<&ManifestWorkspace<'_>>) -> Vec<ManifestDiagnostic> {
        let mut diags = Vec::new();

        // Required: meta.version
        if self.meta.version.is_none() {
            diags.push(ManifestDiagnostic {
                message: "Missing required field: [meta].version".to_string(),
                severity: DiagSeverity::Error,
                anchor: ManifestAnchor::key("meta.version"),
            });
        }

        // Validate export files exist
        if let (Some(script), Some(workspace)) = (&self.script, workspace) {
            let missing = |file: &str| {
                crate::workspace::deps::resolve_plugin_file(
                    workspace.root,
                    file,
                    workspace.plugin_files_search_paths,
                )
                .is_none()
            };
            for (index, export) in script.exports.iter().enumerate() {
                if missing(export) {
                    diags.push(ManifestDiagnostic {
                        message: format!("Export file not found: {}", export),
                        severity: DiagSeverity::Error,
                        anchor: ManifestAnchor::element("script.exports", index),
                    });
                }
            }
            for (index, export) in script.shared_exports.iter().enumerate() {
                if missing(export) {
                    diags.push(ManifestDiagnostic {
                        message: format!("Shared export file not found: {}", export),
                        severity: DiagSeverity::Error,
                        anchor: ManifestAnchor::element("script.shared_exports", index),
                    });
                }
            }
        }

        // Warn about deprecated fields
        if self.meta.perms.is_some() {
            diags.push(ManifestDiagnostic {
                message: "'perms' is deprecated".to_string(),
                severity: DiagSeverity::Warning,
                anchor: ManifestAnchor::key("meta.perms"),
            });
        }

        diags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_counter_info_toml() {
        let toml_str = r#"
[meta]
name     = "Counter"
author   = "XertroV"
category = "Utilities"
version  = "0.2.2"
"#;
        let manifest = Manifest::parse(toml_str).unwrap();
        assert_eq!(manifest.meta.name.as_deref(), Some("Counter"));
        assert_eq!(manifest.meta.version.as_deref(), Some("0.2.2"));
    }

    #[test]
    fn test_parse_dashboard_info_toml() {
        let toml_str = r#"
[meta]
name = "Dashboard"
author = "Miss"
category = "Overlay"
version = "1.9.6"
blocks = [ "Plugin_Dashboard" ]

[script]
dependencies = [ "VehicleState" ]
timeout = 0
exports = ["Source/Exports.as"]
"#;
        let manifest = Manifest::parse(toml_str).unwrap();
        let script = manifest.script.as_ref().unwrap();
        assert_eq!(script.dependencies, vec!["VehicleState"]);
        assert_eq!(script.exports, vec!["Source/Exports.as"]);
    }

    #[test]
    fn test_validate_missing_version() {
        let toml_str = "[meta]\nname = \"Test\"";
        let manifest = Manifest::parse(toml_str).unwrap();
        let diags = manifest.validate(None);
        assert!(diags.iter().any(|d| d.key_path() == Some("meta.version")));
    }

    #[test]
    fn test_malformed_toml() {
        let result = Manifest::parse("this is not toml [[[");
        assert!(result.is_err());
    }

    /// A TOML syntax error carries the offending byte range, so the squiggle
    /// lands on the broken line rather than at the top of the file.
    #[test]
    fn parse_error_carries_the_offending_span() {
        let source = "[meta]\nname = \"Test\"\nversion = = \"1\"\n";
        let err = Manifest::parse(source).unwrap_err();
        let ManifestAnchor::Span(span) = &err.anchor else {
            panic!("parse errors anchor on a span, got {:?}", err.anchor);
        };
        assert!(span.end <= source.len(), "{span:?} past end");
        assert!(
            span.start >= source.find("version").unwrap(),
            "{span:?} should point at the broken line, not the file start"
        );
    }

    /// Fixture plugin: a fresh root with the listed files created under
    /// whichever subdirectory `at` names (`""` for the root itself).
    fn workspace_with(name: &str, files: &[&str], at: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ols-manifest-validate-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for file in files {
            let path = root.join(at).join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "// export\n").unwrap();
        }
        root
    }

    fn search_paths() -> Vec<PathBuf> {
        vec![PathBuf::from("src")]
    }

    fn validate_in(dir: &Path, toml_str: &str) -> Vec<ManifestDiagnostic> {
        let manifest = Manifest::parse(toml_str).unwrap();
        let paths = search_paths();
        manifest.validate(Some(&ManifestWorkspace {
            root: dir,
            plugin_files_search_paths: &paths,
        }))
    }

    const EXPORTING: &str = "[meta]\nversion = \"1\"\n\n[script]\nexports = [\"Ex/Fns.as\"]\nshared_exports = [\"Ex/Shared.as\"]\n";

    /// Exports resolve the way the LOADER resolves them: root-relative first,
    /// then the plugin-file search paths. Most plugins keep sources in
    /// `src/`, so a root-join-only check would report every one of them.
    #[test]
    fn exports_resolve_through_the_plugin_file_search_paths() {
        for at in ["", "src"] {
            let dir = workspace_with(&format!("search-{at}"), &["Ex/Fns.as", "Ex/Shared.as"], at);
            let diags = validate_in(&dir, EXPORTING);
            assert!(
                diags.is_empty(),
                "exports under {at:?} must resolve, got {:?}",
                diags.iter().map(|d| &d.message).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn missing_export_and_shared_export_are_errors_naming_the_entry() {
        let dir = workspace_with("missing-exports", &[], "");
        let diags = validate_in(&dir, EXPORTING);
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert_eq!(diags[0].message, "Export file not found: Ex/Fns.as");
        assert_eq!(diags[0].severity, DiagSeverity::Error);
        assert_eq!(
            diags[0].anchor,
            ManifestAnchor::element("script.exports", 0)
        );
        assert_eq!(
            diags[1].message,
            "Shared export file not found: Ex/Shared.as"
        );
        assert_eq!(diags[1].severity, DiagSeverity::Error);
        assert_eq!(
            diags[1].anchor,
            ManifestAnchor::element("script.shared_exports", 0)
        );
    }

    /// Only the missing entries are reported, and each anchors on its own
    /// array index — not on the first one, and not on the whole array.
    #[test]
    fn each_missing_entry_anchors_on_its_own_index() {
        let dir = workspace_with("own-index", &["A.as", "C.as"], "src");
        let diags = validate_in(
            &dir,
            "[meta]\nversion = \"1\"\n[script]\nexports = [\"A.as\", \"B.as\", \"C.as\", \"D.as\"]\n",
        );
        let anchors: Vec<_> = diags.iter().map(|d| d.anchor.clone()).collect();
        assert_eq!(
            anchors,
            vec![
                ManifestAnchor::element("script.exports", 1),
                ManifestAnchor::element("script.exports", 3),
            ],
            "{diags:?}"
        );
    }

    /// Without a workspace we cannot look for the files, so the export rules
    /// are skipped instead of guessed at (`.op` archives, unsaved buffers).
    #[test]
    fn export_rules_are_skipped_when_there_is_no_workspace() {
        let manifest = Manifest::parse(EXPORTING).unwrap();
        assert!(manifest.validate(None).is_empty());
    }

    #[test]
    fn deprecated_perms_is_a_warning_anchored_on_the_key() {
        let dir = workspace_with("perms", &[], "");
        let diags = validate_in(&dir, "[meta]\nversion = \"1\"\nperms = \"full\"\n");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, DiagSeverity::Warning);
        assert_eq!(diags[0].anchor, ManifestAnchor::key("meta.perms"));
    }

    /// Every rule fires at once — the first problem does not shadow the rest.
    #[test]
    fn all_rules_report_together() {
        let dir = workspace_with("all-rules", &[], "");
        let diags = validate_in(
            &dir,
            "[meta]\nname = \"T\"\nperms = \"full\"\n\n[script]\nexports = [\"E.as\"]\nshared_exports = [\"S.as\"]\n",
        );
        let anchors: Vec<_> = diags.iter().map(|d| d.anchor.clone()).collect();
        assert_eq!(
            anchors,
            vec![
                ManifestAnchor::key("meta.version"),
                ManifestAnchor::element("script.exports", 0),
                ManifestAnchor::element("script.shared_exports", 0),
                ManifestAnchor::key("meta.perms"),
            ],
            "{diags:?}"
        );
    }

    #[test]
    fn a_complete_manifest_reports_nothing() {
        let dir = workspace_with("complete", &["Ex/Fns.as", "Ex/Shared.as"], "src");
        assert!(validate_in(&dir, EXPORTING).is_empty());
    }
}
