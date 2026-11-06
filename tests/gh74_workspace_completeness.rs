use std::fs;
use std::path::PathBuf;

use futures_util::StreamExt;
use openplanet_lsp::server::Backend;
use tower::{Service, ServiceExt};
use tower_lsp::jsonrpc::Request;
use tower_lsp::lsp_types::*;
use tower_lsp::{LanguageServer, LspService};

use openplanet_lsp::cli::{parse_check_args, run_check};
use openplanet_lsp::testing::TempDir;

struct Fixture {
    _root: TempDir,
    plugin: PathBuf,
    plugins: PathBuf,
    dependency: PathBuf,
}

fn missing_export_fixture(name: &str) -> Fixture {
    let root = TempDir::new(name);
    let plugin = root.join("consumer");
    let plugins = root.join("plugins");
    let dependency = plugins.join("Editor");
    fs::create_dir_all(plugin.join("src")).unwrap();
    fs::create_dir_all(dependency.join("src")).unwrap();
    fs::write(
        plugin.join("info.toml"),
        "[meta]\nname = \"Consumer\"\nversion = \"0.1.0\"\n[script]\ndependencies = [\"Editor\"]\n",
    )
    .unwrap();
    fs::write(
        dependency.join("info.toml"),
        "[meta]\nname = \"Editor\"\nversion = \"0.1.0\"\n[script]\nmodule = \"Editor\"\nexports = [\"Missing.as\"]\n",
    )
    .unwrap();
    fs::write(
        plugin.join("src/Main.as"),
        "void Main() { Editor::NotExported(); }\n",
    )
    .unwrap();
    Fixture {
        _root: root,
        plugin,
        plugins,
        dependency,
    }
}

fn check(fixture: &Fixture, extra: &[&str]) -> openplanet_lsp::cli::CheckReport {
    let mut args: Vec<String> = extra.iter().map(|s| (*s).to_string()).collect();
    args.extend([
        "--no-typedb".into(),
        "--unused=ignore".into(),
        "--plugins-dir".into(),
        fixture.plugins.display().to_string(),
        fixture.plugin.display().to_string(),
    ]);
    run_check(&parse_check_args(&args).unwrap()).unwrap()
}

#[test]
fn missing_dependency_export_warns_that_cross_plugin_symbols_cannot_be_verified() {
    let fixture = missing_export_fixture("gh74-missing-export");
    let report = check(&fixture, &[]);
    let warnings: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|d| d.diagnostic.severity == Some(DiagnosticSeverity::WARNING))
        .collect();
    assert_eq!(warnings.len(), 1, "{:#?}", report.diagnostics);
    let warning = warnings[0];
    assert_eq!(warning.path, fixture.plugin.join("info.toml"));
    assert!(warning.diagnostic.message.contains("Editor"));
    assert!(warning.diagnostic.message.contains("Missing.as"));
    assert!(warning.diagnostic.message.contains("cannot be verified"));
    assert!(report
        .diagnostics
        .iter()
        .all(|d| d.diagnostic.severity != Some(DiagnosticSeverity::ERROR)));

    fs::write(
        fixture.dependency.join("src/Missing.as"),
        "namespace Editor { void Available() {} }\n",
    )
    .unwrap();
    let repaired = check(&fixture, &[]);
    assert!(!repaired
        .diagnostics
        .iter()
        .any(|d| d.diagnostic.message.contains("cannot be verified")));
    assert!(repaired
        .diagnostics
        .iter()
        .any(|d| d.diagnostic.severity == Some(DiagnosticSeverity::ERROR)
            && d.diagnostic.message.contains("Editor::NotExported")));
    fs::write(
        fixture.plugin.join("src/Main.as"),
        "void Main() { Editor::Available(); }\n",
    )
    .unwrap();
    assert!(check(&fixture, &[]).diagnostics.is_empty());
}

async fn lsp_manifest_diagnostics(fixture: &Fixture) -> Vec<Diagnostic> {
    let manifest_path = fixture.plugin.join("info.toml");
    let uri = Url::from_file_path(&manifest_path).unwrap();
    let (mut service, mut socket) = LspService::new(Backend::new);
    #[allow(deprecated)]
    let params = InitializeParams {
        root_uri: Some(Url::from_directory_path(&fixture.plugin).unwrap()),
        workspace_folders: Some(vec![WorkspaceFolder {
            uri: Url::from_directory_path(&fixture.plugin).unwrap(),
            name: "consumer".into(),
        }]),
        initialization_options: Some(serde_json::json!({
            "plugins_dir": fixture.plugins,
            "unused": "ignore",
        })),
        ..Default::default()
    };
    let request = Request::build("initialize")
        .id(1)
        .params(serde_json::to_value(params).unwrap())
        .finish();
    let response = service.ready().await.unwrap().call(request).await.unwrap();
    assert!(response.unwrap().error().is_none());
    let event = service.inner().did_open(DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: uri.clone(),
            language_id: "toml".into(),
            version: 1,
            text: fs::read_to_string(&manifest_path).unwrap(),
        },
    });
    let receive = async {
        loop {
            let request = socket.next().await.unwrap();
            if request.method() == "textDocument/publishDiagnostics" {
                let params: PublishDiagnosticsParams =
                    serde_json::from_value(request.params().unwrap().clone()).unwrap();
                if params.uri == uri {
                    return params.diagnostics;
                }
            }
        }
    };
    let (_, diagnostics) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::join!(event, receive)
    })
    .await
    .expect("manifest diagnostics must be published");
    diagnostics
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_publishes_incomplete_dependency_warning_on_consumer_manifest() {
    let fixture = missing_export_fixture("gh74-lsp-missing-export");
    let diagnostics = lsp_manifest_diagnostics(&fixture).await;
    assert!(
        diagnostics.iter().any(|d| {
            d.severity == Some(DiagnosticSeverity::WARNING)
                && d.message.contains("Editor")
                && d.message.contains("Missing.as")
                && d.message.contains("cannot be verified")
        }),
        "{diagnostics:#?}"
    );
}

#[test]
fn configured_source_filters_explain_why_cross_plugin_checks_are_incomplete() {
    for setting in ["source_paths", "ignore_paths"] {
        let fixture = missing_export_fixture(&format!("gh74-{setting}"));
        fs::write(
            fixture.dependency.join("src/Missing.as"),
            "namespace Editor { void Available() {} }\n",
        )
        .unwrap();
        fs::write(
            fixture.plugin.join(".openplanet-lsp.toml"),
            format!("{setting} = [\"src\"]\n"),
        )
        .unwrap();
        let report = check(&fixture, &[]);
        assert!(
            report.diagnostics.iter().any(|d| {
                d.diagnostic.severity == Some(DiagnosticSeverity::WARNING)
                    && d.diagnostic.message.contains(setting)
                    && d.diagnostic.message.contains("cannot be verified")
            }),
            "{setting}: {:#?}",
            report.diagnostics
        );
    }
}

#[test]
fn gitignored_sources_warn_until_the_full_compile_surface_is_checked() {
    let fixture = missing_export_fixture("gh74-gitignored");
    fs::write(
        fixture.dependency.join("src/Missing.as"),
        "namespace Editor { void Available() {} }\n",
    )
    .unwrap();
    fs::create_dir_all(fixture.plugin.join("generated")).unwrap();
    fs::write(fixture.plugin.join(".gitignore"), "generated/\n").unwrap();
    fs::write(
        fixture.plugin.join("generated/New.as"),
        "void Generated() { UnknownFunction(); }\n",
    )
    .unwrap();
    let report = check(&fixture, &[]);
    assert!(
        report.diagnostics.iter().any(|d| {
            d.diagnostic.severity == Some(DiagnosticSeverity::WARNING)
                && d.diagnostic.message.contains(".gitignore")
                && d.diagnostic.message.contains("cannot be verified")
        }),
        "{:#?}",
        report.diagnostics
    );
    assert!(report
        .diagnostics
        .iter()
        .all(|d| d.diagnostic.severity != Some(DiagnosticSeverity::ERROR)));
    let unfiltered = check(&fixture, &["--no-gitignore"]);
    assert!(!unfiltered
        .diagnostics
        .iter()
        .any(|d| d.diagnostic.message.contains("cannot be verified")));
    assert!(unfiltered
        .diagnostics
        .iter()
        .any(|d| d.diagnostic.message.contains("UnknownFunction")));
}

#[test]
fn unresolved_transitive_export_dependency_is_visible_to_the_consumer() {
    let fixture = missing_export_fixture("gh74-transitive");
    fs::write(
        fixture.dependency.join("src/Missing.as"),
        "namespace Editor { void Available() {} }\n",
    )
    .unwrap();
    fs::write(fixture.dependency.join("info.toml"),
        "[meta]\nname = \"Editor\"\nversion = \"0.1.0\"\n[script]\nmodule = \"Editor\"\nexports = [\"Missing.as\"]\nexport_dependencies = [\"NotInstalled\"]\n").unwrap();
    let report = check(&fixture, &[]);
    assert!(
        report.diagnostics.iter().any(|d| {
            d.diagnostic.severity == Some(DiagnosticSeverity::WARNING)
                && d.diagnostic.message.contains("Editor")
                && d.diagnostic.message.contains("NotInstalled")
                && d.diagnostic.message.contains("cannot be verified")
        }),
        "{:#?}",
        report.diagnostics
    );
    assert!(report
        .diagnostics
        .iter()
        .all(|d| d.diagnostic.severity != Some(DiagnosticSeverity::ERROR)));
}

#[test]
fn absent_optional_dependency_warns_only_when_its_define_is_forced() {
    let fixture = missing_export_fixture("gh74-forced-optional");
    fs::write(fixture.plugin.join("info.toml"),
        "[meta]\nname = \"Consumer\"\nversion = \"0.1.0\"\n[script]\noptional_dependencies = [\"NotInstalled\"]\n").unwrap();
    fs::write(
        fixture.plugin.join("src/Main.as"),
        "void Main() {\n#if DEPENDENCY_NOTINSTALLED\n    NotInstalled::Missing();\n#endif\n}\n",
    )
    .unwrap();
    assert!(check(&fixture, &[]).diagnostics.is_empty());
    let forced = check(&fixture, &["--define", "DEPENDENCY_NOTINSTALLED"]);
    assert!(
        forced.diagnostics.iter().any(|d| {
            d.diagnostic.severity == Some(DiagnosticSeverity::WARNING)
                && d.diagnostic.message.contains("NotInstalled")
                && d.diagnostic.message.contains("DEPENDENCY_NOTINSTALLED")
                && d.diagnostic.message.contains("cannot be verified")
        }),
        "{:#?}",
        forced.diagnostics
    );
    assert!(forced
        .diagnostics
        .iter()
        .all(|d| d.diagnostic.severity != Some(DiagnosticSeverity::ERROR)));
}

#[tokio::test(flavor = "current_thread")]
async fn lsp_explains_when_unreadable_sources_force_open_document_only_analysis() {
    let fixture = missing_export_fixture("gh74-lsp-source-load-failure");
    fs::write(
        fixture.dependency.join("src/Missing.as"),
        "namespace Editor { void Available() {} }\n",
    )
    .unwrap();
    fs::write(fixture.plugin.join("src/Unreadable.as"), [0xff]).unwrap();
    let diagnostics = lsp_manifest_diagnostics(&fixture).await;
    assert!(
        diagnostics.iter().any(|d| {
            d.severity == Some(DiagnosticSeverity::WARNING)
                && d.message.contains("workspace")
                && d.message.contains("cannot be verified")
        }),
        "{diagnostics:#?}"
    );
}

#[test]
fn missing_and_unreadable_exports_are_reported_for_directories_and_archives() {
    use std::io::Write;
    for key in ["exports", "shared_exports"] {
        for archive in [false, true] {
            for unreadable in [false, true] {
                let fixture = missing_export_fixture(&format!("gh74-{key}-{archive}-{unreadable}"));
                let manifest = format!("[meta]\nname = \"Editor\"\nversion = \"0.1.0\"\n[script]\nmodule = \"Editor\"\n{key} = [\"Missing.as\"]\n");
                if archive {
                    let file = fs::File::create(fixture.plugins.join("Editor.op")).unwrap();
                    let mut zip = zip::ZipWriter::new(file);
                    let options = zip::write::SimpleFileOptions::default();
                    zip.start_file("info.toml", options).unwrap();
                    zip.write_all(manifest.as_bytes()).unwrap();
                    if unreadable {
                        zip.start_file("Missing.as", options).unwrap();
                        zip.write_all(&[0xff]).unwrap();
                    }
                    zip.finish().unwrap();
                    fs::remove_dir_all(&fixture.dependency).unwrap();
                } else {
                    fs::write(fixture.dependency.join("info.toml"), &manifest).unwrap();
                    if unreadable {
                        fs::write(fixture.dependency.join("src/Missing.as"), [0xff]).unwrap();
                    }
                }
                let report = check(&fixture, &[]);
                let warnings: Vec<_> = report
                    .diagnostics
                    .iter()
                    .filter(|d| {
                        d.diagnostic.severity == Some(DiagnosticSeverity::WARNING)
                            && d.diagnostic.message.contains("Editor")
                            && d.diagnostic.message.contains("Missing.as")
                            && d.diagnostic.message.contains("cannot be verified")
                    })
                    .collect();
                assert_eq!(
                    warnings.len(),
                    1,
                    "{key}, archive={archive}, unreadable={unreadable}: {:#?}",
                    report.diagnostics
                );
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn one_missing_export_listed_twice_produces_one_editor_warning() {
    let fixture = missing_export_fixture("gh74-duplicate-export");
    fs::write(fixture.dependency.join("info.toml"),
        "[meta]\nname = \"Editor\"\nversion = \"0.1.0\"\n[script]\nmodule = \"Editor\"\nexports = [\"Missing.as\"]\nshared_exports = [\"Missing.as\"]\n").unwrap();
    let diagnostics = lsp_manifest_diagnostics(&fixture).await;
    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert!(diagnostics[0].message.contains("Missing.as"));
}
