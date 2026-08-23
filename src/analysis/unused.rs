//! Unused-declaration analysis: workspace-wide usage collection joined
//! against the pooled symbol table.
//!
//! Two phases, mirroring the checker's resolution ladder:
//!
//! 1. **Collect** — run `Checker::with_usage_recording` over every file in
//!    the snapshot against the pooled workspace symbols. The checker
//!    records the workspace-qualified name of everything that *resolved*
//!    (ident reads, calls, member access on typed receivers, implicit-`this`
//!    member use, type-position references, `import ... from` aliases).
//!    A single union set makes sibling-file usage visible to every file —
//!    "exports count as usage" without parsing exports specially.
//!
//! 2. **Join** — walk the pooled symbols; a declaration is unused when no
//!    recorded name matches it *as declared* (qualified names must match
//!    exactly — see [`matches_declared_name`]). Exemptions (Openplanet
//!    callbacks, public-API export files, `_`-prefix, trailing
//!    `// lsp: ignore unused` comment) are applied at this phase so the
//!    checker stays policy-free.
//!
//! False positives are the failure mode that matters (see
//! `todo-false-positives.txt` culture): every heuristic here errs toward
//! over-marking usage. A missed warning is acceptable; a bogus one is not.

use std::collections::HashSet;
use std::path::Path;

use crate::analysis_snapshot::AnalysisSnapshot;
use crate::symbols::scope::{Symbol, SymbolKind};
use crate::typecheck::checker::Checker;
use crate::typecheck::GlobalScope;

pub mod locals;

/// Openplanet engine entry points / callbacks. Free functions with these
/// exact names are invoked by the game / Openplanet runtime by name — a
/// `Main()` no plugin calls is still the plugin's entry point.
///
/// Source: the Openplanet scripting documentation (plugin lifecycle
/// callbacks + `Meta::Plugin`-invoked hooks). Deliberately exhaustive —
/// false positives here are harmless (no warning), false negatives
/// produce wrong "unused" squiggles on real engine callbacks.
pub const OPENPLANET_CALLBACKS: &[&str] = &[
    "Main",
    "Render",
    "RenderEarly",
    "RenderInterface",
    "RenderMenu",
    "RenderMenuMain",
    "Update",
    "OnDestroyed",
    "OnDisabled",
    "OnEnabled",
    "OnError",
    "OnKeyPress",
    "OnKeyRelease",
    "OnMouseButton",
    "OnMouseMove",
    "OnSettingsChanged",
    "OnSettingsLoad",
    "OnSettingsSave",
];

/// True for free functions the Openplanet runtime invokes by name.
fn is_openplanet_callback(simple_name: &str) -> bool {
    OPENPLANET_CALLBACKS.contains(&simple_name)
}

/// True when `name` is exempt as a runtime-invoked callback: a built-in
/// engine callback, or a match against the project's `unused_callbacks`
/// patterns (GH #56). `extra` only ever ADDS to the built-ins — dropping
/// engine callbacks from the list would only manufacture false positives,
/// so there is no switch to disable them.
fn is_exempt_callback(name: &str, extra: &[String]) -> bool {
    is_openplanet_callback(name) || extra.iter().any(|p| matches_callback_pattern(p, name))
}

/// Match one `unused_callbacks` entry against an unqualified name.
///
/// Deliberately not a glob and not a regex — the smallest syntax that
/// covers reflective-dispatch naming, with no new dependency: a literal
/// name, or a single `*` standing for any run of characters. That covers
/// `OnCounterUpdate`, `On*`, `*Update`, `On*Update`, and bare `*`. An
/// entry with two or more `*` is compared literally (unsupported syntax
/// must not silently match everything).
fn matches_callback_pattern(pattern: &str, name: &str) -> bool {
    let Some((prefix, suffix)) = pattern.split_once('*') else {
        return pattern == name;
    };
    if suffix.contains('*') {
        return pattern == name;
    }
    name.len() >= prefix.len() + suffix.len() && name.starts_with(prefix) && name.ends_with(suffix)
}

/// Union of used workspace-qualified names across every file in a
/// snapshot, keyed by the pooled symbol-table name convention, plus the
/// local/parameter bindings the same pass walked (GH #55).
#[derive(Debug, Default)]
pub struct WorkspaceUsage {
    used: HashSet<String>,
    /// Snapshot-wide inputs to the locals/params join (GH #55), built in
    /// the same pass as `used` so the workspace is walked once — the join
    /// itself runs once per reported file.
    locals: locals::LocalsInputs,
}

impl WorkspaceUsage {
    /// Run usage-recording check passes over the whole snapshot. Requires
    /// the external typedb when the caller has one (same resolution view
    /// the diagnostics pass sees).
    pub fn collect(
        snapshot: &AnalysisSnapshot,
        external: Option<&crate::typedb::TypeIndex>,
    ) -> Self {
        let symbols = snapshot.symbols();
        let scope = GlobalScope::new(symbols, external);
        let mut used = HashSet::new();
        let mut local_decls = Vec::with_capacity(snapshot.files().len());
        for file in snapshot.files() {
            let mut checker = Checker::new(file.analysis.masked_source(), &scope)
                .with_usage_recording()
                .with_local_decl_recording();
            checker.check_file(&file.analysis.file);
            used.extend(checker.take_used_names());
            local_decls.push(checker.take_local_decls());
        }
        let locals = locals::LocalsInputs::new(snapshot, external, local_decls);
        Self { used, locals }
    }

    /// True when `qualified` (pooled-table name form) was used somewhere.
    ///
    /// Matching is exact-first, then by *qualified suffix*: a reference
    /// written inside `namespace Repro` to `StatusKind::B` records that
    /// shorter form while the declaration pools as
    /// `Repro::StatusKind::B`. Suffix matches must keep at least TWO
    /// `::`-segments — a bare tail (`B`) is too ambiguous to trust.
    pub fn is_used(&self, qualified: &str) -> bool {
        if self.used.contains(qualified) {
            return true;
        }
        let mut start = 0;
        while let Some(off) = qualified[start..].find("::") {
            let suffix = &qualified[start + off + 2..];
            if suffix.contains("::") && self.used.contains(suffix) {
                return true;
            }
            start += off + 2;
        }
        false
    }

    /// Raw recorded names (test / diagnostic surface).
    pub fn used_names(&self) -> &HashSet<String> {
        &self.used
    }
}

/// One unused-declaration finding, in symbol-table shape.
#[derive(Debug, Clone)]
pub struct UnusedDeclaration {
    /// Pooled symbol-table qualified name (`Ns::Class::method` form).
    pub qualified_name: String,
    /// Simple (unqualified) name as written at the declaration.
    pub simple_name: String,
    pub kind: &'static str,
    /// Span of the declaration (masked-source offsets, file-local).
    pub span: crate::lexer::Span,
    /// file_id into the snapshot (== index into `AnalysisSnapshot::files`).
    pub file_id: usize,
}

/// Compute unused declarations for a snapshot: declarations that no file
/// in the workspace references. `export_paths` are the plugin's own
/// info.toml `[script] exports` + shared_exports files (canonicalized) —
/// declarations in those files are public API and exempt.
///
/// Covers file-level declarations, class members, and — via
/// [`locals`] — function-body locals and parameters (GH #55).
pub fn unused_declarations(
    snapshot: &AnalysisSnapshot,
    external: Option<&crate::typedb::TypeIndex>,
    export_paths: &HashSet<std::path::PathBuf>,
) -> Vec<UnusedDeclaration> {
    let usage = WorkspaceUsage::collect(snapshot, external);
    unused_declarations_with_usage(snapshot, &usage, export_paths)
}

/// Like [`unused_declarations`] but with the usage set supplied by the
/// caller — the shape multi-file callers (CLI `check`) need so the
/// collect pass runs once, not once per file.
pub fn unused_declarations_with_usage(
    snapshot: &AnalysisSnapshot,
    usage: &WorkspaceUsage,
    export_paths: &HashSet<std::path::PathBuf>,
) -> Vec<UnusedDeclaration> {
    unused_declarations_filtered(snapshot, usage, export_paths, None)
}

/// Findings for ONE file. The per-file diagnostics path (LSP publish,
/// CLI `check`) asks per file, so joining the whole workspace and
/// filtering afterwards costs O(files²).
///
/// Sound because every input is either workspace-wide and precomputed
/// (`usage`) or file-local: a binding's enclosing function — and the
/// class that owns it — are declared in the same file as the binding,
/// so the no-double-warn gate only needs this file's findings.
pub fn unused_declarations_for_file(
    snapshot: &AnalysisSnapshot,
    usage: &WorkspaceUsage,
    export_paths: &HashSet<std::path::PathBuf>,
    file_id: usize,
) -> Vec<UnusedDeclaration> {
    unused_declarations_filtered(snapshot, usage, export_paths, Some(file_id))
}

/// Shared body: `only` limits the walk to one file_id.
fn unused_declarations_filtered(
    snapshot: &AnalysisSnapshot,
    usage: &WorkspaceUsage,
    export_paths: &HashSet<std::path::PathBuf>,
    only: Option<usize>,
) -> Vec<UnusedDeclaration> {
    let mut out = Vec::new();
    let symbols = snapshot.symbols();
    // GH #56: the project's extra callback patterns, shared by both
    // consumers — the declaration gate below and the parameter gate in
    // `locals`.
    let extra = snapshot.unused_callbacks();
    let exempt = |name: &str| is_exempt_callback(name, extra);

    for (file_id, file) in snapshot.files().iter().enumerate() {
        if only.is_some_and(|want| want != file_id) {
            continue;
        }
        // Dependency export files contribute symbols and usage but are not
        // this plugin's code — never warn on them (report_diagnostics
        // already gates the diagnostics pipeline; same gate here).
        if !file.report_diagnostics {
            continue;
        }
        // The plugin's own export files are public API.
        if export_paths.contains(&file.path) {
            continue;
        }
        let source = file.analysis.masked_source();
        for sym in symbols.file_symbols(file_id) {
            if let Some(unused) = evaluate_symbol(sym, usage, source, &exempt) {
                out.push(unused);
            }
        }
    }

    // Locals/parameters run second: the declaration-level findings are the
    // no-double-warn gate for the bindings inside them (GH #55).
    let reported: HashSet<String> = out.iter().map(|u| u.qualified_name.clone()).collect();
    locals::collect_unused_locals(
        snapshot,
        &usage.locals,
        export_paths,
        &reported,
        &exempt,
        only,
        &mut out,
    );
    out
}

/// Decide whether one pooled symbol is unused, applying per-kind policy.
/// `source` is the declaring file's masked source (comment suppression
/// and `_`-prefix checks read it).
fn evaluate_symbol(
    sym: &Symbol,
    usage: &WorkspaceUsage,
    source: &str,
    exempt_callback: &dyn Fn(&str) -> bool,
) -> Option<UnusedDeclaration> {
    let (kind, simple_name) = match &sym.kind {
        SymbolKind::Class { .. } => ("class", tail_name(&sym.name)),
        SymbolKind::Interface { .. } => ("interface", tail_name(&sym.name)),
        SymbolKind::Enum { .. } => ("enum", tail_name(&sym.name)),
        SymbolKind::Namespace => return None, // namespaces are open; skip
        SymbolKind::Funcdef { .. } => ("funcdef", tail_name(&sym.name)),
        SymbolKind::EnumValue { .. } => ("enum value", tail_name(&sym.name)),
        SymbolKind::Variable { .. } => {
            // `Class::field` in the pooled table = class member; a bare
            // name = file-level global.
            if sym.name.contains("::") {
                ("field", tail_name(&sym.name))
            } else {
                ("global", tail_name(&sym.name))
            }
        }
        SymbolKind::Function { imported, .. } => {
            if *imported {
                // `import ... from` alias: usage is recorded when the
                // alias name is referenced anywhere in the workspace
                // (checker records the qualified name on Item::Import).
                ("function (imported)", tail_name(&sym.name))
            } else {
                ("function", tail_name(&sym.name))
            }
        }
    };
    // Constructors and destructors are invoked through the class name /
    // lifetime syntax, which records the CLASS, not `Class::Ctor` — and
    // overload-level usage can't be distinguished. Never warn on them.
    if let SymbolKind::Function { .. } = &sym.kind {
        if let Some((owner, member)) = sym.name.rsplit_once("::") {
            let class = owner.rsplit("::").next().unwrap_or(owner);
            if member == class || member.starts_with('~') {
                return None;
            }
        }
    }

    // Operator overloads (`opEquals`, `opAssign`, `opImplConv`, ...) are
    // invoked through operator / conversion syntax, never by name — the
    // checker's expression walker types `a == b` without recording the
    // method. Never warn on `op*` members.
    if simple_name.starts_with("op") && sym.name.contains("::") {
        return None;
    }

    // Inherited methods are virtual dispatch: their "use" is the base's
    // (engine- or framework-invoked), so never warn. The locals input owns
    // the same canonical source/external ancestry graph used for parameter
    // exemptions; do not independently tail-resolve parent names here.
    if let SymbolKind::Function { .. } = &sym.kind {
        if usage.locals.function_has_inherited_contract(&sym.name) {
            return None;
        }
    }

    // `_`-prefixed declarations opt out (same convention as Rust/Python).
    if simple_name.starts_with('_') {
        return None;
    }

    // Enum values: a value of a *used* enum warns individually when never
    // referenced; a value of an *unused* enum is skipped — the type-level
    // warning already covers the whole declaration.
    if let SymbolKind::EnumValue { enum_name, .. } = &sym.kind {
        if !usage.is_used(enum_name) {
            return None;
        }
    }

    // Trailing `// lsp: ignore unused` on the declaration line.
    if has_ignore_comment(source, sym.span.start as usize) {
        return None;
    }

    // Openplanet callbacks: free functions the runtime invokes by name.
    // (Class members and namespaced functions are never callbacks.)
    let is_free_function =
        matches!(sym.kind, SymbolKind::Function { .. }) && !sym.name.contains("::");
    if is_free_function && exempt_callback(simple_name) {
        return None;
    }

    if usage.is_used(&sym.name) {
        return None;
    }

    Some(UnusedDeclaration {
        qualified_name: sym.name.clone(),
        simple_name: simple_name.to_string(),
        kind,
        span: sym.span,
        file_id: sym.file_id,
    })
}

/// Last `::`-segment of a pooled-table qualified name.
fn tail_name(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

/// True when the line containing `offset` ends with an ignore marker
/// comment (`// lsp: ignore unused`, case-insensitive, `lsp-ignore-unused`
/// also accepted).
fn has_ignore_comment(source: &str, offset: usize) -> bool {
    let offset = offset.min(source.len());
    let line_start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line_end = source[offset..]
        .find('\n')
        .map_or(source.len(), |i| offset + i);
    let line = &source[line_start..line_end];
    let normalized: String = line.to_ascii_lowercase().replace([' ', '-', '\t'], "");
    normalized.contains("lsp:ignoreunused") || normalized.contains("lspignoreunused")
}

/// File path of a snapshot file, when needed by callers (CLI labels).
#[allow(dead_code)]
fn _file_path(snapshot: &AnalysisSnapshot, file_id: usize) -> Option<&Path> {
    snapshot.files().get(file_id).map(|f| f.path.as_path())
}

/// Test-only snapshot/report helpers, shared by this module's tests and
/// [`locals`]'s.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::analysis_snapshot::AnalysisSnapshot;
    use crate::config::LspConfig;
    use std::path::PathBuf;

    /// In-memory snapshot from `(path, source)` pairs.
    pub fn snap_of(files: &[(&str, &str)]) -> AnalysisSnapshot {
        let inputs: Vec<(PathBuf, String)> = files
            .iter()
            .map(|(p, s)| (PathBuf::from(p), s.to_string()))
            .collect();
        AnalysisSnapshot::from_files(&inputs, &LspConfig::default())
    }

    /// In-memory snapshot with GH #56 `unused_callbacks` patterns set.
    pub fn snap_of_with_callbacks(files: &[(&str, &str)], callbacks: &[&str]) -> AnalysisSnapshot {
        let inputs: Vec<(PathBuf, String)> = files
            .iter()
            .map(|(p, s)| (PathBuf::from(p), s.to_string()))
            .collect();
        let config = LspConfig {
            unused_callbacks: callbacks.iter().map(|c| c.to_string()).collect(),
            ..LspConfig::default()
        };
        AnalysisSnapshot::from_files(&inputs, &config)
    }

    /// In-memory snapshot with own-module sources the checked set OMITTED
    /// (GH #61): `files` are checked and reported, `usage_only` stands for
    /// the gitignored / `source_paths`-or-`ignore_paths`-filtered files the
    /// game still compiles — they contribute usage and nothing else.
    pub fn snap_with_usage_only(
        files: &[(&str, &str)],
        usage_only: &[(&str, &str)],
    ) -> AnalysisSnapshot {
        let inputs = |v: &[(&str, &str)]| -> Vec<(PathBuf, String)> {
            v.iter()
                .map(|(p, s)| (PathBuf::from(p), s.to_string()))
                .collect()
        };
        AnalysisSnapshot::from_files_with_usage_only(
            &inputs(files),
            &inputs(usage_only),
            &LspConfig::default(),
        )
    }

    /// Sorted qualified names of an unused report.
    pub fn names(report: &[super::UnusedDeclaration]) -> Vec<String> {
        let mut v: Vec<String> = report.iter().map(|u| u.qualified_name.clone()).collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{names, snap_of};
    use super::*;
    use crate::analysis_snapshot::AnalysisSnapshot;
    use crate::config::LspConfig;
    use std::collections::HashSet as StdHashSet;
    use std::path::PathBuf;

    #[test]
    fn unused_free_function_warns() {
        let snap = snap_of(&[(
            "a.as",
            "void Used() {}\nvoid Dead() {}\nvoid Main() { Used(); }\n",
        )]);
        let report = unused_declarations(&snap, None, &StdHashSet::new());
        assert_eq!(names(&report), vec!["Dead"]);
    }

    #[test]
    fn cross_file_usage_suppresses() {
        let snap = snap_of(&[
            ("a.as", "int g_HelpVal = 1;\nvoid Dead() {}\n"),
            (
                "b.as",
                "void Main() { int x = g_HelpVal; print(\"\" + x); }\n",
            ),
        ]);
        let report = unused_declarations(&snap, None, &StdHashSet::new());
        assert_eq!(names(&report), vec!["Dead"]);
    }

    #[test]
    fn openplanet_callbacks_exempt() {
        let snap = snap_of(&[(
            "a.as",
            "void Main() {}\nvoid RenderMenu() {}\nvoid OnKeyPress(VirtualKey k) {}\n",
        )]);
        let report = unused_declarations(&snap, None, &StdHashSet::new());
        assert!(report.is_empty(), "got {:?}", names(&report));
    }

    #[test]
    fn full_callback_surface_exempt() {
        // Every documented engine callback: dips++ uses OnDisabled /
        // OnMouseMove / OnMouseButton / RenderEarly / RenderMenuMain and
        // they must never be flagged (GH: user report, Aug 2026).
        let src = "void OnDisabled() {}\nvoid OnEnabled() {}\nvoid OnDestroyed() {}\nvoid OnError(const string &in msg) {}\nvoid OnMouseButton(bool down, int button) {}\nvoid OnMouseMove(int x, int y) {}\nvoid OnKeyRelease(VirtualKey k) {}\nvoid OnSettingsChanged() {}\nvoid OnSettingsLoad() {}\nvoid OnSettingsSave() {}\nvoid Render() {}\nvoid RenderEarly() {}\nvoid RenderInterface() {}\nvoid RenderMenuMain() {}\nvoid Update(float dt) {}\n";
        let snap = snap_of(&[("a.as", src)]);
        let report = unused_declarations(&snap, None, &StdHashSet::new());
        assert!(report.is_empty(), "got {:?}", names(&report));
    }

    #[test]
    fn underscore_prefix_suppresses() {
        let snap = snap_of(&[(
            "a.as",
            "void _UnusedHelper() {}\nvoid Main() {}\nint _g_Cache = 0;\n",
        )]);
        let report = unused_declarations(&snap, None, &StdHashSet::new());
        assert!(report.is_empty(), "got {:?}", names(&report));
    }

    #[test]
    fn trailing_comment_suppresses() {
        let snap = snap_of(&[(
            "a.as",
            "void KeepMe() {} // lsp: ignore unused\nvoid Main() {}\n",
        )]);
        let report = unused_declarations(&snap, None, &StdHashSet::new());
        assert!(report.is_empty(), "got {:?}", names(&report));
    }

    #[test]
    fn unused_class_members_warn() {
        let src = "class C {\n    int usedField;\n    int deadField;\n    void UsedMethod() {}\n    void DeadMethod() {}\n    void Main() {\n        usedField = 1;\n        UsedMethod();\n    }\n}\n";
        // NOTE: `Main` here is a method, not a free function — expect a
        // warning for it (callbacks are free functions only).
        let snap = snap_of(&[("a.as", src)]);
        let report = named(&snap);
        assert!(
            report.contains(&"C::deadField".to_string()),
            "got {:?}",
            report
        );
        assert!(
            report.contains(&"C::DeadMethod".to_string()),
            "got {:?}",
            report
        );
        assert!(
            !report.contains(&"C::usedField".to_string()),
            "got {:?}",
            report
        );
        assert!(
            !report.contains(&"C::UsedMethod".to_string()),
            "got {:?}",
            report
        );
    }

    fn named(snap: &AnalysisSnapshot) -> Vec<String> {
        names(&unused_declarations(snap, None, &StdHashSet::new()))
    }

    #[test]
    fn exported_files_exempt() {
        let snap = snap_of(&[
            ("Exports.as", "void PubApi() {}\n"),
            ("Main.as", "void Main() {}\n"),
        ]);
        let exports: StdHashSet<PathBuf> = [PathBuf::from("Exports.as")].into_iter().collect();
        let report = unused_declarations(&snap, None, &exports);
        assert_eq!(names(&report), Vec::<String>::new());
    }

    #[test]
    fn enum_type_and_values() {
        let snap = snap_of(&[(
            "a.as",
            "enum Used { A, B }\nenum Dead { X, Y }\nvoid Main() { Used u = Used::A; }\n",
        )]);
        let report = named(&snap);
        // `Dead` (type) unused → single warning on the type only. `Used`
        // is used via `Used u = Used::A`; `Used::A` referenced → no
        // warning; `Used::B` never referenced → warns.
        assert!(report.contains(&"Dead".to_string()), "got {:?}", report);
        assert!(!report.contains(&"Dead::X".to_string()), "got {:?}", report);
        assert!(!report.contains(&"Used".to_string()), "got {:?}", report);
        assert!(!report.contains(&"Used::A".to_string()), "got {:?}", report);
        assert!(report.contains(&"Used::B".to_string()), "got {:?}", report);
    }

    #[test]
    fn import_alias_usage_counts() {
        let snap = snap_of(&[
            (
                "a.as",
                "import void ExtFn() from \"Other\";\nvoid Main() { ExtFn(); }\n",
            ),
            (
                "b.as",
                "import void DeadAlias() from \"Other2\";\nvoid Main2() {}\n",
            ),
        ]);
        let report = named(&snap);
        // ExtFn's alias is referenced in a.as → no warning for it; the
        // alias declaration itself marks nothing used (only *references*
        // to the alias name do). DeadAlias never referenced → warns.
        // Main2 isn't a callback and nothing references it → warns.
        assert!(
            !report.contains(&"ExtFn".to_string()),
            "used alias must not warn: {:?}",
            report
        );
        assert!(
            report.contains(&"DeadAlias".to_string()),
            "got {:?}",
            report
        );
        assert!(report.contains(&"Main2".to_string()), "got {:?}", report);
    }

    // ── GH #56: configurable callback exemptions ────────────────────────

    /// Report for a one-file snapshot with `unused_callbacks` set.
    fn report_with_callbacks(src: &str, callbacks: &[&str]) -> Vec<String> {
        let snap = super::test_support::snap_of_with_callbacks(&[("a.as", src)], callbacks);
        names(&unused_declarations(&snap, None, &StdHashSet::new()))
    }

    #[test]
    fn configured_callback_exempts_the_function_and_its_params() {
        // The issue's own case: MLHook dispatches `OnCounterUpdate` by
        // name reflection, so nothing in the plugin calls it.
        let src = "void OnCounterUpdate(const string &in name, int val) {}\nvoid Main() {}\n";
        assert_eq!(
            report_with_callbacks(src, &[]),
            vec!["OnCounterUpdate"],
            "control: unexempt, the function warns"
        );
        assert_eq!(
            report_with_callbacks(src, &["OnCounterUpdate"]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn callback_patterns_cover_every_supported_wildcard_form() {
        let src = "void OnCounterUpdate(int a) {}\nvoid Main() {}\n";
        for pattern in [
            "OnCounterUpdate", // literal
            "On*",             // prefix
            "*Update",         // suffix
            "On*Update",       // both
            "*",               // everything
        ] {
            assert_eq!(
                report_with_callbacks(src, &[pattern]),
                Vec::<String>::new(),
                "pattern {pattern:?} must exempt OnCounterUpdate"
            );
        }
    }

    #[test]
    fn non_matching_callback_patterns_still_warn() {
        let src = "void OnCounterUpdate(int a) {}\nvoid Main() {}\n";
        for pattern in [
            "OnCounter",       // literal prefix is not a wildcard
            "Update",          // literal suffix is not a wildcard
            "On*Updated",      // suffix does not match
            "Off*",            // prefix does not match
            "On**Update",      // two `*` are unsupported → literal
            "oncounterupdate", // case-sensitive
        ] {
            assert_eq!(
                report_with_callbacks(src, &[pattern]),
                vec!["OnCounterUpdate"],
                "pattern {pattern:?} must NOT exempt OnCounterUpdate"
            );
        }
    }

    #[test]
    fn wildcard_needs_room_for_prefix_and_suffix() {
        // `On*Update` must not match "OnUpdate" by overlapping the two
        // literal halves.
        let src = "void OnUpdate(int a) {}\nvoid Main() {}\n";
        assert_eq!(
            report_with_callbacks(src, &["On*Update"]),
            Vec::<String>::new(),
            "prefix+suffix exactly spanning the name is a match"
        );
        let src = "void OnUpdat(int a) {}\nvoid Main() {}\n";
        assert_eq!(
            report_with_callbacks(src, &["On*Update"]),
            vec!["OnUpdat"],
            "a name shorter than prefix+suffix must not match"
        );
    }

    #[test]
    fn configured_callbacks_are_additive_to_the_builtins() {
        // Setting the key must not replace `OPENPLANET_CALLBACKS`.
        let src = "void Main() {}\nvoid RenderMenu() {}\nvoid OnCounterUpdate(int a) {}\n";
        assert_eq!(
            report_with_callbacks(src, &["OnCounterUpdate"]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_empty_callback_list_is_a_no_op() {
        let src = "void Dead(int a) {}\nvoid Main() {}\n";
        assert_eq!(report_with_callbacks(src, &[]), vec!["Dead"]);
    }

    #[test]
    fn configured_callbacks_do_not_exempt_class_methods() {
        // Decision 5: the patterns match free functions at file top level
        // — the same population the built-in list applies to. MLHook
        // dispatches free functions.
        let src = "class C {\n    int keep;\n    void OnCounterUpdate(int a) {}\n}\nvoid Main() { C c; c.keep = 1; }\n";
        let report = report_with_callbacks(src, &["OnCounterUpdate", "*"]);
        assert!(
            report.contains(&"C::OnCounterUpdate".to_string()),
            "class methods stay in scope: {report:?}"
        );
    }

    #[test]
    fn configured_callbacks_do_not_exempt_namespaced_functions() {
        let src = "namespace N {\n    void OnCounterUpdate(int a) {}\n}\nvoid Main() {}\n";
        let report = report_with_callbacks(src, &["OnCounterUpdate", "*"]);
        assert!(
            report.contains(&"N::OnCounterUpdate".to_string()),
            "namespaced functions stay in scope: {report:?}"
        );
    }

    #[test]
    fn dependency_export_files_never_warn() {
        // report_diagnostics=false files are excluded entirely.
        let snap = AnalysisSnapshot::from_files_with_flags(
            &[
                (PathBuf::from("own.as"), "void Main() {}".to_string(), true),
                (
                    PathBuf::from("dep.as"),
                    "void DepOnly() {}".to_string(),
                    false,
                ),
            ],
            &LspConfig::default(),
        );
        let report = unused_declarations(&snap, None, &StdHashSet::new());
        assert!(
            !names(&report).contains(&"DepOnly".to_string()),
            "dep export decls must never warn: {:?}",
            names(&report)
        );
    }
}

/// GH #61: usage from own-module files the checked set OMITTED.
///
/// Gitignore honoring (GH #60) and `source_paths`/`ignore_paths` both drop
/// files the GAME still compiles, so a declaration whose only callers live
/// there was reported "never used". These files contribute usage only —
/// never declarations, never diagnostics — which is why every case below
/// asserts on the CHECKED file's report.
#[cfg(test)]
mod omitted_own_module_usage {
    use super::test_support::{names, snap_with_usage_only};
    use super::*;
    use std::collections::HashSet as StdHashSet;

    /// Names reported for a checked set plus an omitted (usage-only) set.
    fn report(files: &[(&str, &str)], usage_only: &[(&str, &str)]) -> Vec<String> {
        let snap = snap_with_usage_only(files, usage_only);
        names(&unused_declarations(&snap, None, &StdHashSet::new()))
    }

    /// The GH #61 repro: `generated/` is gitignored, and its only caller of
    /// `UsedOnlyByGenerated` is invisible to the usage join.
    #[test]
    fn a_call_from_an_omitted_file_counts_as_usage() {
        assert_eq!(
            report(
                &[(
                    "src/Main.as",
                    "void UsedOnlyByGenerated() {}\nvoid Main() {}\n"
                )],
                &[(
                    "generated/Gen.as",
                    "void GenCaller() { UsedOnlyByGenerated(); }\n"
                )],
            ),
            Vec::<String>::new()
        );
    }

    /// Variant: a global variable READ (not a call).
    #[test]
    fn a_global_read_from_an_omitted_file_counts_as_usage() {
        assert_eq!(
            report(
                &[("src/Main.as", "int g_Val = 1;\nvoid Main() {}\n")],
                &[(
                    "generated/Gen.as",
                    "void GenCaller() { print(\"\" + g_Val); }\n"
                )],
            ),
            Vec::<String>::new()
        );
    }

    /// Variant: a TYPE-POSITION reference, no call and no member access.
    #[test]
    fn a_type_position_reference_from_an_omitted_file_counts_as_usage() {
        assert_eq!(
            report(
                &[("src/Main.as", "class Helper {}\nvoid Main() {}\n")],
                &[("generated/Gen.as", "void GenCaller() { Helper h; }\n")],
            ),
            Vec::<String>::new()
        );
    }

    /// Variant: MEMBER ACCESS on a typed receiver reaches class members —
    /// the population that motivated the class-member half of the pass.
    #[test]
    fn member_access_from_an_omitted_file_counts_as_usage() {
        assert_eq!(
            report(
                &[(
                    "src/Main.as",
                    "class C {\n    int f;\n    void M() {}\n}\nvoid Main() {}\n"
                )],
                &[(
                    "generated/Gen.as",
                    "void GenCaller() { C c; c.f = 1; c.M(); }\n"
                )],
            ),
            Vec::<String>::new()
        );
    }

    /// Variant: an ENUM VALUE. The unused value stays reported — the fix
    /// must credit exactly what the omitted file touches.
    #[test]
    fn enum_value_usage_from_an_omitted_file_is_credited_per_value() {
        assert_eq!(
            report(
                &[("src/Main.as", "enum E { A, B }\nvoid Main() {}\n")],
                &[("generated/Gen.as", "void GenCaller() { E e = E::A; }\n")],
            ),
            vec!["E::B"]
        );
    }

    /// Variant: a NAMESPACED declaration, reached by its qualified name.
    #[test]
    fn namespaced_usage_from_an_omitted_file_counts_as_usage() {
        assert_eq!(
            report(
                &[(
                    "src/Main.as",
                    "namespace N {\n    void F() {}\n}\nvoid Main() {}\n"
                )],
                &[("generated/Gen.as", "void GenCaller() { N::F(); }\n")],
            ),
            Vec::<String>::new()
        );
    }

    /// Variant: an `import ... from` ALIAS declaration, whose only caller
    /// is omitted. Aliases are reportable declarations of their own
    /// (`import_alias_usage_counts`), so they need the same credit.
    #[test]
    fn import_alias_used_only_by_an_omitted_file_is_not_reported() {
        assert_eq!(
            report(
                &[(
                    "src/Main.as",
                    "import void ExtFn() from \"Other\";\nvoid Main() {}\n"
                )],
                &[("generated/Gen.as", "void GenCaller() { ExtFn(); }\n")],
            ),
            Vec::<String>::new()
        );
    }

    /// Variant: a CLASS METHOD called through implicit `this` in a subclass
    /// declared in the omitted file — usage that only resolves because the
    /// omitted file is checked against the pooled table.
    #[test]
    fn subclass_in_an_omitted_file_uses_the_base_member() {
        assert_eq!(
            report(
                &[(
                    "src/Main.as",
                    "class Base {\n    void Hook() {}\n}\nvoid Main() {}\n"
                )],
                &[(
                    "generated/Gen.as",
                    "class Derived : Base {\n    void Go() { Hook(); }\n}\n"
                )],
            ),
            Vec::<String>::new()
        );
    }

    /// NON-VACUITY: the pass must still warn. A genuinely unused
    /// declaration with omitted files present is the case that proves the
    /// fix did not simply disarm the rule (refuted alternative 2).
    #[test]
    fn a_genuinely_unused_declaration_still_warns_with_omitted_files_present() {
        assert_eq!(
            report(
                &[("src/Main.as", "void Dead() {}\nvoid Main() {}\n")],
                &[("generated/Gen.as", "void GenCaller() { print(\"hi\"); }\n")],
            ),
            vec!["Dead"]
        );
    }

    /// Omitted files declare nothing we can see, so their own
    /// declarations are never reported — the GH #60 goal (no squiggles in
    /// a file the author believes is excluded) must survive.
    #[test]
    fn declarations_inside_an_omitted_file_are_never_reported() {
        assert_eq!(
            report(
                &[("src/Main.as", "void Main() {}\n")],
                &[(
                    "generated/Gen.as",
                    "void GenDead() {}\nclass GenClass { int f; }\n"
                )],
            ),
            Vec::<String>::new()
        );
    }

    /// A gitignored WHOLE-TREE COPY (`.worktrees/`, scratch probes) is the
    /// common shape in the real fleet. A stale copy that still calls a
    /// now-dead function MASKS the warning. Accepted, and asserted so the
    /// direction is deliberate: a missed warning is acceptable, a bogus one
    /// is not.
    #[test]
    fn a_stale_omitted_copy_masks_a_real_unused_warning() {
        assert_eq!(
            report(
                &[("src/Main.as", "void Dead() {}\nvoid Main() {}\n")],
                &[(
                    ".worktrees/copy/src/Main.as",
                    "void Dead() {}\nvoid Main() { Dead(); }\n"
                )],
            ),
            Vec::<String>::new()
        );
    }

    /// The copy above must not DOUBLE-report either: its duplicate
    /// declarations never reach the pooled table, which is what refuted
    /// alternative 1 (parse omitted files fully) got wrong.
    #[test]
    fn an_omitted_copy_never_double_reports_its_duplicate_declarations() {
        let out = report(
            &[("src/Main.as", "void Dead() {}\nvoid Main() {}\n")],
            &[(
                ".worktrees/copy/src/Main.as",
                "void Dead() {}\nvoid Main() {}\n",
            )],
        );
        assert_eq!(
            out,
            vec!["Dead"],
            "exactly one finding, from the checked tree"
        );
    }

    /// No omitted files at all: the overwhelmingly common tree must be
    /// bit-for-bit unchanged.
    #[test]
    fn an_empty_omitted_set_changes_nothing() {
        assert_eq!(
            report(&[("src/Main.as", "void Dead() {}\nvoid Main() {}\n")], &[]),
            vec!["Dead"]
        );
    }

    /// Locals and parameters are out of scope (B011 scope note): a local's
    /// usage is bounded by its own function body, so no omitted file can
    /// affect it — and an omitted mention must NOT silence it.
    #[test]
    fn omitted_files_do_not_credit_locals_or_parameters() {
        let out = report(
            &[(
                "src/Main.as",
                "void Helper(int p) {\n    int dead = 1;\n    print(\"x\");\n}\nvoid Main() { Helper(1); }\n",
            )],
            &[(
                "generated/Gen.as",
                "void GenCaller() { int dead = 2; int p = 3; print(\"\" + dead + p); }\n",
            )],
        );
        assert!(
            out.contains(&"Helper::dead".to_string()),
            "a local must stay reported: {out:?}"
        );
        assert!(
            out.contains(&"Helper::p".to_string()),
            "a parameter must stay reported: {out:?}"
        );
    }
}
