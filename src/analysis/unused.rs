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
use crate::symbols::SymbolTable;
use crate::typecheck::checker::Checker;
use crate::typecheck::GlobalScope;

/// Openplanet engine entry points / callbacks. Free functions with these
/// exact names are invoked by the game / Openplanet runtime by name — a
/// `Main()` no plugin calls is still the plugin's entry point.
pub const OPENPLANET_CALLBACKS: &[&str] = &[
    "Main",
    "Render",
    "RenderMenu",
    "RenderInterface",
    "OnKeyPress",
    "OnSettingsChanged",
    "OnDestroyed",
];

/// True for free functions the Openplanet runtime invokes by name.
fn is_openplanet_callback(simple_name: &str) -> bool {
    OPENPLANET_CALLBACKS.contains(&simple_name)
}

/// Union of used workspace-qualified names across every file in a
/// snapshot, keyed by the pooled symbol-table name convention.
#[derive(Debug, Default)]
pub struct WorkspaceUsage {
    used: HashSet<String>,
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
        for file in snapshot.files() {
            let mut checker =
                Checker::new(file.analysis.masked_source(), &scope).with_usage_recording();
            checker.check_file(&file.analysis.file);
            used.extend(checker.take_used_names());
        }
        Self { used }
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
/// Locals and parameters are deliberately out of scope (deferred — see
/// the tracking issue); this covers file-level decls and class members.
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
    let mut out = Vec::new();
    let symbols = snapshot.symbols();

    for (file_id, file) in snapshot.files().iter().enumerate() {
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
            if let Some(unused) = evaluate_symbol(sym, usage, source, symbols) {
                out.push(unused);
            }
        }
    }
    out
}

/// Decide whether one pooled symbol is unused, applying per-kind policy.
/// `source` is the declaring file's masked source (comment suppression
/// and `_`-prefix checks read it).
fn evaluate_symbol(
    sym: &Symbol,
    usage: &WorkspaceUsage,
    source: &str,
    symbols: &SymbolTable,
) -> Option<UnusedDeclaration> {
    let (kind, simple_name) = match &sym.kind {
        SymbolKind::Class { .. } => ("class", tail_name(&sym.name)),
        SymbolKind::Interface { .. } => ("interface", tail_name(&sym.name)),
        SymbolKind::Enum { .. } => ("enum", tail_name(&sym.name)),
        SymbolKind::Namespace => return None, // namespaces are open; skip
        SymbolKind::Funcdef { .. } => ("funcdef", tail_name(&sym.name)),
        SymbolKind::EnumValue { .. } => ("enum value", tail_name(&sym.name)),
        SymbolKind::Variable { .. } => ("global", tail_name(&sym.name)),
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

    // `override` methods are virtual dispatch: if ANY class in the parent
    // chain declares a member with the same name, this decl is an
    // override and its "use" is the base's (engine- or framework-invoked)
    // — never warn. Parent names are short-form in the pooled table; the
    // tail match mirrors `GlobalScope::workspace_class_member`'s walk.
    if let SymbolKind::Function { .. } = &sym.kind {
        if let Some((owner, member)) = sym.name.rsplit_once("::") {
            if class_chain_declares(symbols, owner, member) {
                return None;
            }
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
    if is_free_function && is_openplanet_callback(simple_name) {
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

/// True when `class_name` or any ancestor (via the pooled table's parent
/// lists) declares a member named `member` — i.e. `Class::member` is an
/// override. Cycle-guarded, depth-capped.
fn class_chain_declares(symbols: &SymbolTable, class_name: &str, member: &str) -> bool {
    let mut visited = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::from([class_name.to_string()]);
    let mut hops = 0usize;
    while let Some(class) = queue.pop_front() {
        if !visited.insert(class.clone()) {
            continue;
        }
        hops += 1;
        if hops > 64 {
            break;
        }
        for sym in symbols.lookup(&class) {
            if let SymbolKind::Class { parents, .. } = &sym.kind {
                for parent in parents {
                    // Parents are stored as written (often short-form);
                    // try exact then tail match.
                    if !symbols.lookup(parent).is_empty() {
                        queue.push_back(parent.clone());
                    } else if let Some(tail) =
                        symbols.lookup_tail(parent).first().map(|s| s.name.clone())
                    {
                        queue.push_back(tail);
                    }
                }
            }
        }
        let qualified = format!("{}::{}", class, member);
        if !symbols.lookup(&qualified).is_empty() {
            // Declared on THIS class — only counts as an override when an
            // ANCESTOR also declares it (sym itself is the decl under
            // evaluation, at the front of the walk).
            if class != class_name {
                return true;
            }
        } else if class != class_name
            && !symbols.lookup_tail(member).is_empty()
            && symbols
                .lookup_members(&class)
                .iter()
                .any(|s| s.name.ends_with(&format!("::{}", member)))
        {
            return true;
        }
    }
    false
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis_snapshot::AnalysisSnapshot;
    use crate::config::LspConfig;
    use std::collections::HashSet as StdHashSet;
    use std::path::PathBuf;

    fn snap_of(files: &[(&str, &str)]) -> AnalysisSnapshot {
        let inputs: Vec<(PathBuf, String)> = files
            .iter()
            .map(|(p, s)| (PathBuf::from(p), s.to_string()))
            .collect();
        AnalysisSnapshot::from_files(&inputs, &LspConfig::default())
    }

    fn names(report: &[UnusedDeclaration]) -> Vec<String> {
        let mut v: Vec<String> = report.iter().map(|u| u.qualified_name.clone()).collect();
        v.sort();
        v
    }

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
            ("b.as", "void Main() { int x = g_HelpVal; }\n"),
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
