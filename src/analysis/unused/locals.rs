//! Unused **locals and parameters** (GH #55) — the function-body half of
//! the unused-declaration analysis.
//!
//! The checker records every local/parameter binding it walks
//! ([`Checker::with_local_decl_recording`]); this module decides which of
//! them warn. Two halves:
//!
//! * **Usage** is *syntactic*: a binding is used when its identifier is
//!   mentioned again anywhere after its declaration, up to the end of the
//!   enclosing function body. A write with no read (`x = 1;`) therefore
//!   counts as usage — dead-store analysis needs real flow analysis and
//!   would misfire on `&out` parameters, loop accumulators and `cast<>`
//!   temporaries. The scan reads the ORIGINAL source, not the masked one,
//!   so a mention inside an inactive `#if` branch still counts.
//! * **Exemptions** cover bindings whose shape the author did not choose:
//!   parameters of Openplanet callbacks, operator overloads, virtual
//!   overrides / interface implementations, public-API export files, and
//!   lambdas (never recorded). Parameters and locals of a declaration
//!   that is *itself* reported unused are suppressed too — the
//!   declaration-level warning already covers them.
//!
//! Every heuristic errs toward marking a binding used: a missed warning
//! is acceptable, a bogus one is not (`todo-false-positives.txt`).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use super::{class_chain_declares, has_ignore_comment, tail_name, UnusedDeclaration};
use crate::analysis_snapshot::AnalysisSnapshot;
use crate::lexer::{self, TokenKind};
use crate::parser::ast::{Item, TypeExpr, TypeExprKind};
use crate::symbols::scope::SymbolKind;
use crate::symbols::SymbolTable;
use crate::typecheck::checker::{LocalDeclKind, LocalDeclRecord};

/// One file's identifier tokens, indexed for the two syntactic questions
/// this module asks.
///
/// Built from the ORIGINAL source: identifiers inside an inactive `#if`
/// branch are blanked in the masked source, and treating those as absent
/// would report a binding that a different define set uses. Comments and
/// string literals are single tokens, so their contents never register.
#[derive(Debug)]
struct FileScan {
    /// name → ascending token start offsets.
    by_name: HashMap<String, Vec<u32>>,
    /// Names that occur at least once *not* immediately followed by `(`
    /// — i.e. mentioned as a value rather than called. A function handed
    /// to a funcdef (`MsgHandler(AuthFailHandler)`,
    /// `CoroutineFunc(MyCoro)`) takes the funcdef's signature, so its
    /// parameters are not the author's to fix. Deliberately blunt: it
    /// over-marks (a member access `x.Foo` counts), which only ever
    /// removes warnings.
    value_refs: HashSet<String>,
}

impl FileScan {
    fn of(source: &str) -> Self {
        let mut by_name: HashMap<String, Vec<u32>> = HashMap::new();
        let mut value_refs = HashSet::new();
        let tokens = lexer::tokenize(source);
        let mut idents = tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == TokenKind::Ident);
        for (i, tok) in &mut idents {
            let text = tok.span.text(source);
            by_name
                .entry(text.to_string())
                .or_default()
                .push(tok.span.start);
            let called = tokens[i + 1..]
                .iter()
                .find(|t| !t.kind.is_trivia())
                .is_some_and(|t| t.kind == TokenKind::LParen);
            if !called {
                value_refs.insert(text.to_string());
            }
        }
        Self {
            by_name,
            value_refs,
        }
    }

    /// True when `name` appears as an identifier strictly after `after`
    /// and at or before `upto` — "mentioned again in the enclosing
    /// function body".
    fn mentioned_after(&self, name: &str, after: u32, upto: u32) -> bool {
        self.by_name
            .get(name)
            .is_some_and(|offsets| offsets.iter().any(|&o| o > after && o <= upto))
    }
}

/// `(class, member)` pairs a *subclass* re-declares — the base half of a
/// virtual pair. [`class_chain_declares`] covers the derived half; an
/// abstract base stub (`void Render(Vis@ v) { throw("not implemented"); }`)
/// needs this one, because the descendants fix its signature.
fn members_overridden_by_subclasses(symbols: &SymbolTable) -> HashSet<(String, String)> {
    let mut out = HashSet::new();
    for sym in symbols.all_symbols() {
        if !matches!(sym.kind, SymbolKind::Class { .. }) {
            continue;
        }
        let ancestors = ancestors_of(symbols, &sym.name);
        if ancestors.is_empty() {
            continue;
        }
        // `SymbolKind::Class::members` is not populated by the extractor;
        // the pooled `Class::member` entries are the member list.
        for member_sym in symbols.lookup_members(&sym.name) {
            let member = tail_name(&member_sym.name);
            for ancestor in &ancestors {
                if !symbols.lookup(&format!("{ancestor}::{member}")).is_empty() {
                    out.insert((ancestor.clone(), member.to_string()));
                }
            }
        }
    }
    out
}

/// Ancestor class/interface names of `class_name`, exclusive. Parents are
/// stored as written (often short-form), so an exact lookup is tried
/// first and a tail match second — the same resolution
/// [`class_chain_declares`] uses. Cycle-guarded, depth-capped.
fn ancestors_of(symbols: &SymbolTable, class_name: &str) -> Vec<String> {
    let mut visited = HashSet::from([class_name.to_string()]);
    let mut queue = std::collections::VecDeque::from([class_name.to_string()]);
    let mut out = Vec::new();
    while let Some(class) = queue.pop_front() {
        if out.len() > 64 {
            break;
        }
        for sym in symbols.lookup(&class) {
            let SymbolKind::Class { parents, .. } = &sym.kind else {
                continue;
            };
            for parent in parents {
                let resolved = if symbols.lookup(parent).is_empty() {
                    symbols.lookup_tail(parent).first().map(|s| s.name.clone())
                } else {
                    Some(parent.clone())
                };
                let Some(resolved) = resolved else { continue };
                if visited.insert(resolved.clone()) {
                    out.push(resolved.clone());
                    queue.push_back(resolved);
                }
            }
        }
    }
    out
}

#[derive(Debug)]
struct SourceType {
    is_interface: bool,
    parents: Vec<ScopedTypeName>,
    methods: HashSet<String>,
}

#[derive(Debug)]
struct ScopedTypeName {
    written: String,
    namespace: String,
}

/// `(class, member)` pairs whose signature is imposed by a workspace
/// interface. Interface bases and methods are not retained in the pooled
/// symbol table, so collect this deliberately narrow view from the AST.
fn interface_implementations(snapshot: &AnalysisSnapshot) -> HashSet<(String, String)> {
    let mut types = HashMap::new();
    for file in snapshot.files() {
        let source = file.analysis.masked_source();
        collect_source_types(source, &file.analysis.file.items, "", &mut types);
    }

    let class_names: Vec<String> = types
        .iter()
        .filter(|(_, ty)| !ty.is_interface)
        .map(|(name, _)| name.clone())
        .collect();
    let mut out = HashSet::new();
    for class_name in class_names {
        let Some(class) = types.get(&class_name) else {
            continue;
        };
        let mut visited = HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        for parent in &class.parents {
            if let Some(parent) = resolve_source_type(&types, parent) {
                queue.push_back(parent);
            }
        }
        while let Some(ancestor_name) = queue.pop_front() {
            if !visited.insert(ancestor_name.clone()) {
                continue;
            }
            if visited.len() > 64 {
                break;
            }
            let Some(ancestor) = types.get(&ancestor_name) else {
                continue;
            };
            if ancestor.is_interface {
                out.extend(
                    ancestor
                        .methods
                        .iter()
                        .map(|member| (class_name.clone(), member.clone())),
                );
            }
            for parent in &ancestor.parents {
                if let Some(parent) = resolve_source_type(&types, parent) {
                    queue.push_back(parent);
                }
            }
        }
    }
    out
}

fn collect_source_types(
    source: &str,
    items: &[Item],
    namespace: &str,
    out: &mut HashMap<String, SourceType>,
) {
    let qualify = |name: &str| {
        if namespace.is_empty() {
            name.to_string()
        } else {
            format!("{namespace}::{name}")
        }
    };
    for item in items {
        match item {
            Item::Class(class) => {
                let name = qualify(class.name.text(source));
                let entry = out.entry(name).or_insert_with(|| SourceType {
                    is_interface: false,
                    parents: Vec::new(),
                    methods: HashSet::new(),
                });
                entry
                    .parents
                    .extend(class.base_classes.iter().filter_map(|base| {
                        source_type_name(source, base).map(|written| ScopedTypeName {
                            written,
                            namespace: namespace.to_string(),
                        })
                    }));
            }
            Item::Interface(interface) => {
                let name = qualify(interface.name.text(source));
                let entry = out.entry(name).or_insert_with(|| SourceType {
                    is_interface: true,
                    parents: Vec::new(),
                    methods: HashSet::new(),
                });
                entry.is_interface = true;
                entry
                    .parents
                    .extend(interface.bases.iter().filter_map(|base| {
                        source_type_name(source, base).map(|written| ScopedTypeName {
                            written,
                            namespace: namespace.to_string(),
                        })
                    }));
                entry.methods.extend(
                    interface
                        .methods
                        .iter()
                        .map(|method| method.name.text(source).to_string()),
                );
            }
            Item::Namespace(ns) => {
                let nested = qualify(ns.name.text(source));
                collect_source_types(source, &ns.items, &nested, out);
            }
            _ => {}
        }
    }
}

fn source_type_name(source: &str, ty: &TypeExpr) -> Option<String> {
    match &ty.kind {
        TypeExprKind::Named(name) => Some(name.to_string(source)),
        TypeExprKind::Handle(inner)
        | TypeExprKind::Reference(inner, _)
        | TypeExprKind::Const(inner) => source_type_name(source, inner),
        TypeExprKind::Template(name, _) => Some(name.to_string(source)),
        _ => None,
    }
}

/// Resolve like a lexical type name, without a tail-name fallback. The
/// latter would conflate `Api::Parent` with `Impl::Parent` and suppress a
/// real warning merely because the short names collide.
fn resolve_source_type(
    types: &HashMap<String, SourceType>,
    name: &ScopedTypeName,
) -> Option<String> {
    let mut namespace = name.namespace.as_str();
    loop {
        let candidate = if namespace.is_empty() {
            name.written.clone()
        } else {
            format!("{namespace}::{}", name.written)
        };
        if types.contains_key(&candidate) {
            return Some(candidate);
        }
        let Some((outer, _)) = namespace.rsplit_once("::") else {
            break;
        };
        namespace = outer;
    }
    types
        .contains_key(&name.written)
        .then(|| name.written.clone())
}

/// Snapshot-wide inputs to the locals/params join, built ONCE per
/// [`super::WorkspaceUsage`].
///
/// The join itself runs once per reported file (see
/// `unused_declaration_diagnostics`), so everything that scans the whole
/// workspace has to live here or the pass costs O(files²).
#[derive(Debug, Default)]
pub(super) struct LocalsInputs {
    /// Recorded bindings per file_id.
    decls: Vec<Vec<LocalDeclRecord>>,
    /// Identifier index per file_id.
    scans: Vec<FileScan>,
    /// Workspace-wide value-reference set (see [`FileScan::value_refs`]).
    /// Workspace-wide because a handler may be registered in a file that
    /// declares no bindings of its own.
    value_refs: HashSet<String>,
    /// Workspace-wide base-of-a-virtual-pair set.
    overridden: HashSet<(String, String)>,
    /// Workspace interface contracts recovered from the source AST.
    interface_implementations: HashSet<(String, String)>,
}

impl LocalsInputs {
    /// `decls` come from the same check pass that collects usage.
    pub(super) fn new(snapshot: &AnalysisSnapshot, decls: Vec<Vec<LocalDeclRecord>>) -> Self {
        let scans: Vec<FileScan> = snapshot
            .files()
            .iter()
            .map(|f| FileScan::of(&f.analysis.source))
            .collect();
        let value_refs = scans
            .iter()
            .flat_map(|s| s.value_refs.iter().cloned())
            .collect();
        Self {
            decls,
            scans,
            value_refs,
            overridden: members_overridden_by_subclasses(snapshot.symbols()),
            interface_implementations: interface_implementations(snapshot),
        }
    }
}

/// Append unused locals/parameters for the whole snapshot to `out`.
///
/// `unused_symbol_names` are the qualified names the declaration-level
/// pass already reported — the no-double-warn gate.
pub(super) fn collect_unused_locals(
    snapshot: &AnalysisSnapshot,
    inputs: &LocalsInputs,
    export_paths: &HashSet<PathBuf>,
    unused_symbol_names: &HashSet<String>,
    exempt_callbacks: &dyn Fn(&str) -> bool,
    only: Option<usize>,
    out: &mut Vec<UnusedDeclaration>,
) {
    let symbols = snapshot.symbols();
    for (file_id, file) in snapshot.files().iter().enumerate() {
        if only.is_some_and(|want| want != file_id) {
            continue;
        }
        // Dependency export files contribute symbols and usage but are
        // not this plugin's code (same gate as the declaration pass).
        if !file.report_diagnostics {
            continue;
        }
        let Some(decls) = inputs.decls.get(file_id) else {
            continue;
        };
        if decls.is_empty() {
            continue;
        }
        // A partial parse leaves truncated body spans, which would make
        // the mention scan read the wrong region. Skip the file rather
        // than risk a false positive.
        if !file.analysis.parse_errors.is_empty() {
            continue;
        }
        let ctx = FileCtx {
            file_id,
            masked: file.analysis.masked_source(),
            scan: &inputs.scans[file_id],
            value_refs: &inputs.value_refs,
            overridden: &inputs.overridden,
            interface_implementations: &inputs.interface_implementations,
            is_export_file: export_paths.contains(&file.path),
            symbols,
            unused_symbol_names,
            exempt_callbacks,
        };
        for rec in decls {
            if let Some(unused) = ctx.evaluate(rec) {
                out.push(unused);
            }
        }
    }
}

/// Per-file inputs to [`FileCtx::evaluate`].
struct FileCtx<'a> {
    file_id: usize,
    /// Masked source — what spans and the ignore-comment scan read.
    masked: &'a str,
    scan: &'a FileScan,
    /// Workspace-wide value-reference set (see [`FileScan::value_refs`]).
    value_refs: &'a HashSet<String>,
    /// Workspace-wide base-of-a-virtual-pair set.
    overridden: &'a HashSet<(String, String)>,
    interface_implementations: &'a HashSet<(String, String)>,
    is_export_file: bool,
    symbols: &'a SymbolTable,
    unused_symbol_names: &'a HashSet<String>,
    exempt_callbacks: &'a dyn Fn(&str) -> bool,
}

impl FileCtx<'_> {
    fn evaluate(&self, rec: &LocalDeclRecord) -> Option<UnusedDeclaration> {
        // `_`-prefixed bindings opt out (same convention as the
        // declaration pass, and Openplanet's own `_`-param rule).
        if rec.name.starts_with('_') {
            return None;
        }
        if has_ignore_comment(self.masked, rec.span.start as usize) {
            return None;
        }
        // No double-warn: the enclosing declaration (or its class) is
        // already reported, and deleting it removes these bindings too.
        if self.enclosing_declaration_is_unused(&rec.function) {
            return None;
        }
        if rec.kind == LocalDeclKind::Param && self.param_signature_is_fixed(rec) {
            return None;
        }
        if self
            .scan
            .mentioned_after(&rec.name, rec.span.start, rec.function_body_end)
        {
            return None;
        }
        Some(UnusedDeclaration {
            qualified_name: format!("{}::{}", rec.function, rec.name),
            simple_name: rec.name.clone(),
            kind: match rec.kind {
                LocalDeclKind::Local => "local variable",
                LocalDeclKind::Param => "parameter",
            },
            span: rec.span,
            file_id: self.file_id,
        })
    }

    /// True when the declaration-level pass already reported the enclosing
    /// function, or the class that owns it.
    fn enclosing_declaration_is_unused(&self, function: &str) -> bool {
        if self.unused_symbol_names.contains(function) {
            return true;
        }
        // Constructors/destructors are never reported themselves (they are
        // invoked through the class name), so an unused class is the only
        // signal that their bindings are already covered.
        function
            .rsplit_once("::")
            .is_some_and(|(owner, _)| self.unused_symbol_names.contains(owner))
    }

    /// True when the enclosing function's signature is imposed by its
    /// caller rather than chosen by the author, so an unused parameter is
    /// not something the author can fix.
    fn param_signature_is_fixed(&self, rec: &LocalDeclRecord) -> bool {
        // Public API: the plugin's own export files are consumed by other
        // plugins, which fix the signature.
        if self.is_export_file {
            return true;
        }
        // Virtual dispatch: the base declaration fixes the signature.
        if rec.function_is_override {
            return true;
        }
        // Mixin members splice into consuming classes by declaration —
        // the consumer (possibly an interface it implements) fixes the
        // signature, not the mixin author.
        if rec.function_owner_is_mixin {
            return true;
        }
        let member = tail_name(&rec.function);
        // Engine/framework callbacks, invoked by name.
        if rec.function_is_top_level_free && (self.exempt_callbacks)(member) {
            return true;
        }
        let owner = rec.function.rsplit_once("::").map(|(owner, _)| owner);
        // Handed to a funcdef somewhere (handler tables, `startnew`
        // coroutines): the funcdef fixes the signature. Constructors and
        // destructors are excluded — they share the class's name, which
        // appears in every type position, and are never funcdef targets.
        let is_ctor_or_dtor =
            member.starts_with('~') || owner.is_some_and(|o| tail_name(o) == member);
        if !is_ctor_or_dtor && self.value_refs.contains(member) {
            return true;
        }
        let Some(owner) = owner else {
            return false;
        };
        if !self.owner_is_a_type(owner) {
            return false;
        }
        // Operator overloads / conversions are invoked through operator
        // syntax; their signature is the language's, not the author's.
        if member.starts_with("op") {
            return true;
        }
        // An ancestor declaring the same member makes this an override /
        // interface implementation even without the `override` keyword.
        if class_chain_declares(self.symbols, owner, member) {
            return true;
        }
        if self
            .interface_implementations
            .contains(&(owner.to_string(), member.to_string()))
        {
            return true;
        }
        // …and a DESCENDANT declaring it makes this the base of a virtual
        // pair, whose signature the descendants fix (abstract stubs).
        self.overridden
            .contains(&(owner.to_string(), member.to_string()))
    }

    /// True when `name` resolves to a class or interface — distinguishes
    /// `Class::method` from a namespaced free function `Ns::func`.
    fn owner_is_a_type(&self, name: &str) -> bool {
        self.symbols.lookup(name).iter().any(|s| {
            matches!(
                s.kind,
                SymbolKind::Class { .. } | SymbolKind::Interface { .. }
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::analysis::unused::test_support::{names, snap_of};
    use crate::analysis::unused::unused_declarations;
    use crate::analysis_snapshot::AnalysisSnapshot;
    use crate::config::LspConfig;
    use std::collections::HashSet as StdHashSet;
    use std::path::PathBuf;

    /// Report for a single-file snapshot, no export files.
    fn report_of(src: &str) -> Vec<String> {
        let snap = snap_of(&[("a.as", src)]);
        names(&unused_declarations(&snap, None, &StdHashSet::new()))
    }

    fn assert_has(report: &[String], want: &str) {
        assert!(
            report.iter().any(|n| n == want),
            "expected `{want}` in report, got {report:?}"
        );
    }

    fn assert_lacks(report: &[String], unwanted: &str) {
        assert!(
            !report.iter().any(|n| n == unwanted),
            "`{unwanted}` must not be reported, got {report:?}"
        );
    }

    // ── Locals ──────────────────────────────────────────────────────────

    #[test]
    fn unused_local_warns() {
        assert_eq!(
            report_of("void Main() {\n    int x = 1;\n}\n"),
            vec!["Main::x"]
        );
    }

    #[test]
    fn read_local_does_not_warn() {
        let report = report_of("void Main() {\n    int x = 1;\n    print(\"\" + x);\n}\n");
        assert_lacks(&report, "Main::x");
    }

    #[test]
    fn write_only_local_counts_as_used() {
        // Decision 1: a later *mention* is usage; dead-store analysis is
        // deliberately out of scope (it needs real flow analysis).
        let report = report_of("void Main() {\n    int x = 1;\n    x = 2;\n}\n");
        assert_lacks(&report, "Main::x");
    }

    #[test]
    fn underscore_prefixed_local_suppressed() {
        assert_eq!(
            report_of("void Main() {\n    int _x = 1;\n}\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn ignore_comment_suppresses_local() {
        assert_eq!(
            report_of("void Main() {\n    int x = 1; // lsp: ignore unused\n}\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn local_in_nested_block_warns() {
        assert_eq!(
            report_of("void Main() {\n    {\n        int inner = 1;\n    }\n}\n"),
            vec!["Main::inner"]
        );
    }

    #[test]
    fn multi_declarator_locals_are_evaluated_individually() {
        assert_eq!(
            report_of("void Main() {\n    int a = 1, b = 2;\n    print(\"\" + a);\n}\n"),
            vec!["Main::b"]
        );
    }

    #[test]
    fn for_init_var_used_by_condition_and_step() {
        // Decision 5: condition/step are the same scope — usage.
        assert_eq!(
            report_of("void Main() {\n    for (uint i = 0; i < 3; i++) {}\n}\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn for_init_var_never_mentioned_warns() {
        assert_eq!(
            report_of("void Main() {\n    for (uint i = 0; true;) { break; }\n}\n"),
            vec!["Main::i"]
        );
    }

    #[test]
    fn local_in_namespaced_function_reports_qualified() {
        assert_eq!(
            report_of("namespace N {\n    void F() {\n        int x = 1;\n    }\n}\nvoid Main() { N::F(); }\n"),
            vec!["N::F::x"]
        );
    }

    #[test]
    fn local_in_method_reports_qualified() {
        let report =
            report_of("class C {\n    void M() {\n        int x = 1;\n    }\n}\nvoid Main() { C c; c.M(); }\n");
        assert_has(&report, "C::M::x");
    }

    #[test]
    fn mention_in_inactive_preprocessor_branch_counts_as_usage() {
        // FP guard: masking blanks the inactive branch, so the mention
        // scan must read the ORIGINAL source, not the masked one.
        let report = report_of(
            "void Main() {\n    int x = 1;\n#if NEVER_DEFINED\n    print(\"\" + x);\n#endif\n}\n",
        );
        assert_lacks(&report, "Main::x");
    }

    #[test]
    fn mention_only_in_a_comment_still_warns() {
        let report = report_of("void Main() {\n    int x = 1;\n    // x is unused\n}\n");
        assert_has(&report, "Main::x");
    }

    #[test]
    fn mention_only_in_a_string_literal_still_warns() {
        let report = report_of("void Main() {\n    int x = 1;\n    print(\"x\");\n}\n");
        assert_has(&report, "Main::x");
    }

    #[test]
    fn shadowed_name_mentioned_in_a_sibling_scope_suppresses() {
        // The mention scan is bounded by the enclosing FUNCTION body, a
        // superset of the declaring scope: a same-named mention anywhere
        // later in the function suppresses. Strictly conservative — this
        // can miss a warning, never invent one.
        let report = report_of(
            "void Main() {\n    int x = 1;\n    {\n        int x = 2;\n        print(\"\" + x);\n    }\n}\n",
        );
        assert_lacks(&report, "Main::x");
    }

    #[test]
    fn locals_are_scoped_to_their_own_function() {
        // A mention of `x` in a LATER function must not suppress the
        // warning in an earlier one.
        let report = report_of(
            "void A() {\n    int x = 1;\n}\nvoid B() {\n    int x = 2;\n    print(\"\" + x);\n}\nvoid Main() { A(); B(); }\n",
        );
        assert_has(&report, "A::x");
        assert_lacks(&report, "B::x");
    }

    // ── Parameters ──────────────────────────────────────────────────────

    #[test]
    fn unused_param_of_used_function_warns() {
        assert_eq!(
            report_of("void Helper(int a) {}\nvoid Main() { Helper(1); }\n"),
            vec!["Helper::a"]
        );
    }

    #[test]
    fn used_param_does_not_warn() {
        let report =
            report_of("void Helper(int a) { print(\"\" + a); }\nvoid Main() { Helper(1); }\n");
        assert_lacks(&report, "Helper::a");
    }

    #[test]
    fn param_of_unused_function_warns_only_at_the_declaration() {
        // Decision 2: an unused function already warns; no double-warn.
        let report = report_of("void Dead(int a) {}\nvoid Main() {}\n");
        assert_has(&report, "Dead");
        assert_lacks(&report, "Dead::a");
    }

    #[test]
    fn unused_param_warns_for_every_reference_form() {
        // AGENTS.md generalization: each param form the language offers.
        let cases: &[(&str, &str, &str)] = &[
            ("int a", "", "Helper(1);"),
            ("const int a", "", "Helper(1);"),
            ("const int &in a", "", "Helper(1);"),
            ("int &in a", "", "Helper(1);"),
            ("int &out a", "", "int _v = 0; Helper(_v);"),
            ("int &inout a", "", "int _v = 0; Helper(_v);"),
            ("int &a", "", "int _v = 0; Helper(_v);"),
            ("const string &in a", "", "Helper(\"s\");"),
            ("MyThing@ a", "class MyThing {}\n", "Helper(null);"),
            ("MyThing@ &in a", "class MyThing {}\n", "Helper(null);"),
            (
                "const MyThing &in a",
                "class MyThing {}\n",
                "MyThing _t; Helper(_t);",
            ),
            ("array<int>@ a", "", "Helper(null);"),
            ("array<int>@ &in a", "", "Helper(null);"),
        ];
        for (param, prelude, call) in cases {
            let src = format!("{prelude}void Helper({param}) {{}}\nvoid Main() {{ {call} }}\n");
            let report = report_of(&src);
            assert!(
                report.iter().any(|n| n == "Helper::a"),
                "param form `{param}` must warn; got {report:?}"
            );
            assert!(
                !report.iter().any(|n| n == "Helper"),
                "param form `{param}`: Helper must count as used; got {report:?}"
            );
        }
    }

    #[test]
    fn openplanet_callback_params_exempt() {
        // Decision 3: signature imposed by the runtime, not the author.
        assert_eq!(
            report_of("void OnKeyPress(bool down, VirtualKey key) {}\nvoid Main() {}\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn every_openplanet_callback_params_exempt() {
        let src = "void OnMouseButton(bool down, int button, int x, int y) {}\nvoid OnMouseMove(int x, int y) {}\nvoid OnKeyRelease(bool down, VirtualKey key) {}\nvoid OnError(const string &in msg) {}\nvoid Update(float dt) {}\nvoid Main() {}\n";
        assert_eq!(report_of(src), Vec::<String>::new());
    }

    #[test]
    fn operator_overload_params_exempt() {
        let report = report_of(
            "class C {\n    bool opEquals(const C &in other) { return true; }\n}\nvoid Main() { C _c; }\n",
        );
        assert_lacks(&report, "C::opEquals::other");
    }

    #[test]
    fn override_method_params_exempt() {
        let report = report_of(
            "class Base {\n    void Do(int a) { print(\"\" + a); }\n}\nclass Derived : Base {\n    void Do(int a) override {}\n}\nvoid Main() { Derived _d; _d.Do(1); }\n",
        );
        assert_lacks(&report, "Derived::Do::a");
    }

    #[test]
    fn interface_implementation_params_exempt() {
        let report = report_of(
            "interface IThing {\n    void Do(int a);\n}\nclass C : IThing {\n    void Do(int a) {}\n}\nvoid Main() { IThing@ _i = C(); _i.Do(1); }\n",
        );
        assert_lacks(&report, "C::Do::a");
        assert_lacks(&report, "IThing::Do::a");
    }

    #[test]
    fn interface_implementation_params_exempt_for_concrete_receiver() {
        let report = report_of(
            "interface IThing {\n    void Do(int a);\n}\nclass C : IThing {\n    void Do(int a) {}\n}\nvoid Main() { C c; c.Do(1); }\n",
        );
        assert_lacks(&report, "C::Do::a");
    }

    #[test]
    fn inherited_interface_implementation_params_exempt() {
        let report = report_of(
            "interface IBase {\n    void Do(int a);\n}\ninterface IThing : IBase {}\nclass C : IThing {\n    void Do(int a) {}\n}\nvoid Main() { C c; c.Do(1); }\n",
        );
        assert_lacks(&report, "C::Do::a");
    }

    #[test]
    fn same_short_name_in_another_namespace_is_not_an_interface_match() {
        let report = report_of(
            "namespace Api {\n    interface Parent { void Do(int a); }\n}\nnamespace Impl {\n    class Parent {}\n    class C : Parent { void Do(int a) {} }\n}\nvoid Main() { Impl::C c; c.Do(1); }\n",
        );
        assert_has(&report, "Impl::C::Do::a");
    }

    #[test]
    fn params_of_bodyless_declarations_never_warn() {
        // Decision 2: imports, funcdefs and interface methods have no body.
        let report = report_of(
            "interface IThing { void Do(int a); }\nfuncdef void Cb(int a);\nimport void Ext(int a) from \"Other\";\nvoid Main() { Ext(1); Cb@ _c; IThing@ _i; }\n",
        );
        for n in &report {
            assert!(
                !n.ends_with("::a"),
                "bodyless declarations must not warn on params; got {report:?}"
            );
        }
    }

    #[test]
    fn export_file_params_exempt_locals_still_warn() {
        // Decision 3: public-API signatures are imposed by consumers.
        // Bodies are still this plugin's code.
        let snap = snap_of(&[
            ("Exports.as", "void PubApi(int a) {\n    int dead = 1;\n}\n"),
            ("Main.as", "void Main() {}\n"),
        ]);
        let exports: StdHashSet<PathBuf> = [PathBuf::from("Exports.as")].into_iter().collect();
        let report = names(&unused_declarations(&snap, None, &exports));
        assert_has(&report, "PubApi::dead");
        assert_lacks(&report, "PubApi::a");
    }

    #[test]
    fn method_params_warn_on_a_used_class() {
        let report =
            report_of("class C {\n    void M(int a) {}\n}\nvoid Main() { C c; c.M(1); }\n");
        assert_has(&report, "C::M::a");
    }

    #[test]
    fn method_params_suppressed_when_the_method_is_unused() {
        let report = report_of("class C {\n    void M(int a) {}\n}\nvoid Main() { C _c; }\n");
        assert_has(&report, "C::M");
        assert_lacks(&report, "C::M::a");
    }

    #[test]
    fn constructor_params_warn_when_the_class_is_used() {
        let report = report_of("class C {\n    C(int a) {}\n}\nvoid Main() { C _c(1); }\n");
        assert_has(&report, "C::C::a");
    }

    #[test]
    fn constructor_params_suppressed_when_the_class_is_unused() {
        let report = report_of("class C {\n    C(int a) {}\n}\nvoid Main() {}\n");
        assert_has(&report, "C");
        assert_lacks(&report, "C::C::a");
    }

    #[test]
    fn lambda_params_exempt_but_lambda_body_locals_warn() {
        // A lambda's signature is imposed by the funcdef it is passed to.
        let report = report_of(
            "funcdef void Cb(int);\nvoid Main() {\n    Cb@ _h = function(int a) { int dead = 1; };\n}\n",
        );
        assert_lacks(&report, "Main::a");
        assert_has(&report, "Main::dead");
    }

    #[test]
    fn property_accessor_value_is_never_reported() {
        // The checker binds a synthetic `value` for `set` accessors; it is
        // not a source declaration and has no usable span.
        let report = report_of(
            "class C {\n    int _v;\n    int Thing {\n        get { return _v; }\n        set { _v = value; }\n    }\n}\nvoid Main() { C c; c.Thing = 1; }\n",
        );
        assert_lacks(&report, "C::Thing::value");
        for n in &report {
            assert!(!n.ends_with("::value"), "got {report:?}");
        }
    }

    #[test]
    fn top_level_property_accessors_report_only_unused_locals() {
        let report = report_of(
            "int Thing {\n    get {\n        int deadGet = 1;\n        int liveGet = 2;\n        return liveGet;\n    }\n    set {\n        int deadSet = 1;\n        int liveSet = 2;\n        print(\"\" + liveSet);\n    }\n}\nvoid Main() { int _got = Thing; Thing = 1; }\n",
        );
        assert_has(&report, "Thing::deadGet");
        assert_has(&report, "Thing::deadSet");
        assert_lacks(&report, "Thing::liveGet");
        assert_lacks(&report, "Thing::liveSet");
        assert_lacks(&report, "Thing::value");
    }

    #[test]
    fn class_property_accessors_report_only_unused_locals() {
        let report = report_of(
            "class C {\n    int Thing {\n        get {\n            int deadGet = 1;\n            int liveGet = 2;\n            return liveGet;\n        }\n        set {\n            int deadSet = 1;\n            int liveSet = 2;\n            print(\"\" + liveSet);\n        }\n    }\n}\nvoid Main() { C c; int _got = c.Thing; c.Thing = 1; }\n",
        );
        assert_has(&report, "C::Thing::deadGet");
        assert_has(&report, "C::Thing::deadSet");
        assert_lacks(&report, "C::Thing::liveGet");
        assert_lacks(&report, "C::Thing::liveSet");
        assert_lacks(&report, "C::Thing::value");
    }

    #[test]
    fn mixin_class_method_params_exempt() {
        // Mixin members splice into consuming classes by declaration;
        // the file-level pass already marks them used.
        let report = report_of(
            "mixin class M {\n    void Do(int a) {}\n}\nclass C : M {}\nvoid Main() { C _c; }\n",
        );
        assert_lacks(&report, "M::Do::a");
    }

    #[test]
    fn locals_of_an_unused_function_are_suppressed() {
        // Same no-double-warn rule as parameters: deleting the reported
        // declaration removes its bindings too.
        let report = report_of("void Dead() {\n    int x = 1;\n}\nvoid Main() {}\n");
        assert_has(&report, "Dead");
        assert_lacks(&report, "Dead::x");
    }

    #[test]
    fn dependency_export_files_never_report_locals() {
        let snap = AnalysisSnapshot::from_files_with_flags(
            &[
                (
                    PathBuf::from("own.as"),
                    "void Main() {}\n".to_string(),
                    true,
                ),
                (
                    PathBuf::from("dep.as"),
                    "void DepApi(int a) {\n    int dead = 1;\n}\n".to_string(),
                    false,
                ),
            ],
            &LspConfig::default(),
        );
        let report = names(&unused_declarations(&snap, None, &StdHashSet::new()));
        assert_lacks(&report, "DepApi::a");
        assert_lacks(&report, "DepApi::dead");
    }

    #[test]
    fn a_file_with_parse_errors_reports_no_locals() {
        // Truncated body spans from a partial parse would make the
        // mention scan read the wrong region; skip the file instead.
        let snap = snap_of(&[("a.as", "void Main() {\n    int x = 1;\n    );\n}\n")]);
        assert!(
            !snap.files()[0].analysis.parse_errors.is_empty(),
            "fixture must actually fail to parse"
        );
        let report = names(&unused_declarations(&snap, None, &StdHashSet::new()));
        assert_lacks(&report, "Main::x");
    }

    // ── FP classes found dogfooding (decision 6) ────────────────────────

    #[test]
    fn base_method_params_exempt_when_a_subclass_overrides() {
        // tm-dashboard `Source/Thing.as`, dips++ `src/Animation.as`: an
        // abstract base stub's signature is fixed by the descendants that
        // override it — the mirror image of the `override` exemption.
        let report = report_of(
            "class Base {\n    string Describe(int i) { return \"base\"; }\n}\nclass Derived : Base {\n    string Describe(int i) override { return \"\" + i; }\n}\nvoid Main() { Base@ b = Derived(); print(b.Describe(1)); }\n",
        );
        assert_lacks(&report, "Base::Describe::i");
    }

    #[test]
    fn base_method_params_exempt_without_the_override_keyword() {
        let report = report_of(
            "class Base {\n    string Describe(int i) { return \"base\"; }\n}\nclass Derived : Base {\n    string Describe(int i) { return \"\" + i; }\n}\nvoid Main() { Base@ b = Derived(); print(b.Describe(1)); }\n",
        );
        assert_lacks(&report, "Base::Describe::i");
    }

    #[test]
    fn base_method_params_exempt_when_the_subclass_is_in_another_file() {
        let snap = snap_of(&[
            (
                "base.as",
                "class Base {\n    string Describe(int i) { return \"base\"; }\n}\n",
            ),
            (
                "derived.as",
                "class Derived : Base {\n    string Describe(int i) override { return \"\" + i; }\n}\nvoid Main() { Base@ b = Derived(); print(b.Describe(1)); }\n",
            ),
        ]);
        let report = names(&unused_declarations(&snap, None, &StdHashSet::new()));
        assert_lacks(&report, "Base::Describe::i");
    }

    #[test]
    fn base_method_params_exempt_through_a_grandparent() {
        let report = report_of(
            "class Base {\n    string Describe(int i) { return \"base\"; }\n}\nclass Mid : Base {}\nclass Leaf : Mid {\n    string Describe(int i) override { return \"\" + i; }\n}\nvoid Main() { Base@ b = Leaf(); print(b.Describe(1)); }\n",
        );
        assert_lacks(&report, "Base::Describe::i");
    }

    #[test]
    fn params_exempt_when_the_function_is_referenced_as_a_value() {
        // dips++ `src/Server/Server.as`: handlers registered through a
        // funcdef (`MsgHandler(AuthFailHandler)`) take the funcdef's
        // signature, not one the author chose.
        let cases: &[&str] = &[
            // funcdef "constructor" form
            "funcdef void Handler(int);\nvoid OnThing(int a) {}\nvoid Main() { Handler@ _h = Handler(OnThing); }\n",
            // bare assignment
            "funcdef void Handler(int);\nvoid OnThing(int a) {}\nvoid Main() { Handler@ _h = OnThing; }\n",
            // passed as a call argument
            "funcdef void Handler(int);\nvoid Register(Handler@ h) { h(1); }\nvoid OnThing(int a) {}\nvoid Main() { Register(Handler(OnThing)); }\n",
            // coroutine form, the common Openplanet idiom
            "void OnThing(ref@ a) {}\nvoid Main() { startnew(CoroutineFuncUserdata(OnThing), null); }\n",
        ];
        for src in cases {
            let report = report_of(src);
            assert!(
                !report.iter().any(|n| n == "OnThing::a"),
                "value-referenced function must not warn on params; src={src:?} got {report:?}"
            );
        }
    }

    #[test]
    fn method_params_exempt_when_the_method_is_referenced_as_a_value() {
        let report = report_of(
            "funcdef void Handler(int);\nclass Server {\n    Handler@ h;\n    void Wire() { @h = Handler(OnThing); }\n    void OnThing(int a) {}\n}\nvoid Main() { Server s; s.Wire(); }\n",
        );
        assert_lacks(&report, "Server::OnThing::a");
    }

    #[test]
    fn a_plainly_called_function_still_warns_on_params() {
        // Guard against the value-reference rule swallowing everything.
        let report = report_of("void Helper(int a) {}\nvoid Main() { Helper(1); }\n");
        assert_has(&report, "Helper::a");
    }

    // ── Diagnostic wording (decision 7) ─────────────────────────────────

    #[test]
    fn kinds_are_local_variable_and_parameter() {
        let snap = snap_of(&[(
            "a.as",
            "void Helper(int a) {\n    int x = 1;\n}\nvoid Main() { Helper(1); }\n",
        )]);
        let report = unused_declarations(&snap, None, &StdHashSet::new());
        let mut kinds: Vec<&str> = report
            .iter()
            .filter(|u| u.qualified_name.starts_with("Helper::"))
            .map(|u| u.kind)
            .collect();
        kinds.sort();
        assert_eq!(kinds, vec!["local variable", "parameter"], "got {report:?}");
    }
}
