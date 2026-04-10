use serde::Deserialize;
use std::path::Path;

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
    pub key_path: String, // e.g. "meta.version"
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
            key_path: String::new(),
        })?;
        Self::parse(&contents)
    }

    pub fn parse(contents: &str) -> Result<Self, ManifestDiagnostic> {
        toml::from_str(contents).map_err(|e| ManifestDiagnostic {
            message: format!("TOML parse error: {}", e),
            severity: DiagSeverity::Error,
            key_path: String::new(),
        })
    }

    /// Validate the manifest and return all diagnostics.
    pub fn validate(&self, workspace_root: &Path) -> Vec<ManifestDiagnostic> {
        let mut diags = Vec::new();

        // Required: meta.version
        if self.meta.version.is_none() {
            diags.push(ManifestDiagnostic {
                message: "Missing required field 'version'".to_string(),
                severity: DiagSeverity::Error,
                key_path: "meta.version".to_string(),
            });
        }

        // Validate export files exist
        if let Some(script) = &self.script {
            for export in &script.exports {
                let export_path = workspace_root.join(export);
                if !export_path.exists() {
                    diags.push(ManifestDiagnostic {
                        message: format!("Export file not found: {}", export),
                        severity: DiagSeverity::Error,
                        key_path: "script.exports".to_string(),
                    });
                }
            }
            for export in &script.shared_exports {
                let export_path = workspace_root.join(export);
                if !export_path.exists() {
                    diags.push(ManifestDiagnostic {
                        message: format!("Shared export file not found: {}", export),
                        severity: DiagSeverity::Error,
                        key_path: "script.shared_exports".to_string(),
                    });
                }
            }
        }

        // Warn about deprecated fields
        if self.meta.perms.is_some() {
            diags.push(ManifestDiagnostic {
                message: "'perms' is deprecated".to_string(),
                severity: DiagSeverity::Warning,
                key_path: "meta.perms".to_string(),
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
        let diags = manifest.validate(Path::new("/tmp"));
        assert!(diags.iter().any(|d| d.key_path == "meta.version"));
    }

    #[test]
    fn test_malformed_toml() {
        let result = Manifest::parse("this is not toml [[[");
        assert!(result.is_err());
    }
}
