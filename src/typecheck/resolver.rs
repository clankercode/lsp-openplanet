//! Syntactic `TypeExpr` → semantic `TypeRepr` resolution.
//!
//! This is the first stage of type checking: given a parsed type expression
//! (with spans referencing source text), figure out what canonical type it
//! refers to. Unknown names produce diagnostics and an `Error(...)` result
//! so that downstream code can keep going.

use super::global_scope::GlobalScope;
use super::repr::{PrimitiveType, TypeRepr};
use crate::lexer::{Span, TokenKind};
use crate::parser::ast::{QualifiedName, TypeExpr, TypeExprKind};

#[derive(Debug, Clone, PartialEq)]
pub struct ResolveDiagnostic {
    pub span: Span,
    /// The unresolved name (without the `unknown type` prefix), e.g. `Foo`
    /// or `Net::NotAThing`. Callers format the final user-facing message.
    pub unknown_name: String,
}

impl ResolveDiagnostic {
    /// Human-readable form used by the LSP diagnostic layer.
    pub fn message(&self) -> String {
        format!("unknown type `{}`", self.unknown_name)
    }
}

pub struct TypeResolver<'a> {
    pub scope: &'a GlobalScope<'a>,
    pub source: &'a str,
    pub diagnostics: Vec<ResolveDiagnostic>,
    /// Active namespace stack the resolver should consult when a bare
    /// name doesn't resolve at the top level. Outermost first — the
    /// resolver walks from the deepest prefix to the shortest, then
    /// finally tries the bare name.
    pub namespace_stack: Vec<String>,
}

impl<'a> TypeResolver<'a> {
    pub fn new(scope: &'a GlobalScope<'a>, source: &'a str) -> Self {
        Self {
            scope,
            source,
            diagnostics: Vec::new(),
            namespace_stack: Vec::new(),
        }
    }

    /// Attach a namespace stack (e.g. `["Outer","Inner"]`) so the
    /// resolver tries `Outer::Inner::Name`, then `Outer::Name`, then
    /// `Name` when looking up a bare type reference.
    pub fn with_namespace_stack(mut self, stack: Vec<String>) -> Self {
        self.namespace_stack = stack;
        self
    }

    /// Resolve a syntactic type expression to its canonical `TypeRepr`.
    /// Any unknown named types are reported as diagnostics (accessible via
    /// `take_diagnostics`) and replaced with `TypeRepr::Error(name)`.
    pub fn resolve(&mut self, ty: &TypeExpr) -> TypeRepr {
        match &ty.kind {
            TypeExprKind::Primitive(tk) => self.resolve_primitive_token(*tk),

            TypeExprKind::Named(qname) => self.resolve_named(qname),

            TypeExprKind::Handle(inner) => {
                let resolved = self.resolve(inner);
                TypeRepr::Handle(Box::new(resolved))
            }

            TypeExprKind::Reference(inner, _mod) => {
                // The reference modifier (`&in`/`&out`/`&inout`) is a
                // calling-convention marker, not part of the value type
                // itself. Strip it here.
                self.resolve(inner)
            }

            TypeExprKind::Array(inner) => {
                let resolved = self.resolve(inner);
                TypeRepr::Array(Box::new(resolved))
            }

            TypeExprKind::Template(name, args) => {
                let base = name.to_string(self.source);
                let resolved_args: Vec<TypeRepr> = args.iter().map(|a| self.resolve(a)).collect();
                // Trust template base names — `array` / `dictionary` / etc.
                // are not looked up in the global scope.
                TypeRepr::Generic {
                    base,
                    args: resolved_args,
                }
            }

            TypeExprKind::Const(inner) => {
                let resolved = self.resolve(inner);
                TypeRepr::Const(Box::new(resolved))
            }

            TypeExprKind::Auto => TypeRepr::Named("auto".into()),

            TypeExprKind::Error => TypeRepr::Error(String::new()),
        }
    }

    pub fn take_diagnostics(&mut self) -> Vec<ResolveDiagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    // ── Internals ───────────────────────────────────────────────────────────

    fn resolve_primitive_token(&mut self, tk: TokenKind) -> TypeRepr {
        if let Some(prim) = PrimitiveType::from_token_kind(tk) {
            return TypeRepr::Primitive(prim);
        }
        match tk {
            TokenKind::KwVoid => TypeRepr::Void,
            TokenKind::KwAuto => TypeRepr::Named("auto".into()),
            // Any other keyword showing up here is a parser bug — fall
            // through to a generic error so the checker can continue.
            _ => TypeRepr::Error(format!("{:?}", tk)),
        }
    }

    fn resolve_named(&mut self, qname: &QualifiedName) -> TypeRepr {
        let qualified = qname.to_string(self.source);

        // Bare template names referenced without arguments — keep them as
        // empty `Generic` so an outer layer can wrap/instantiate.
        if qualified == "array" || qualified == "dictionary" {
            return TypeRepr::Generic {
                base: qualified,
                args: Vec::new(),
            };
        }

        // GH #38 / B073: a plugin's own TYPE declaration shadows the engine
        // symbol of the same name. The two lookups immediately below both
        // answer from the ENGINE — `lookup_funcdef` walks workspace symbols
        // but only *returns* on a real funcdef before falling through to the
        // typedb, and the builtin table is hardcoded — so asking either one
        // first is what let a plugin's `class CoroutineFunc` resolve to the
        // Core funcdef. Establish the shadowing declaration once, here, and
        // let it win below.
        //
        // The three later `lookup_funcdef` calls in this function need no
        // such guard and deliberately do not have one: each is reached only
        // after this name failed to resolve, which cannot happen while the
        // workspace declares a type of it — an exact declaration answers
        // here, and a namespaced one answers here through the tail table.
        let workspace_shadowing = self.scope.workspace_shadowing_type_name(&qualified);

        if workspace_shadowing.is_none() {
            // Funcdefs are types, but the constructed / annotated form is
            // `TypeRepr::Funcdef`, not `Named` — a handle `T@` is then
            // handle-compatible with the ctor result (GH #63).
            if self.scope.lookup_funcdef(&qualified).is_some() {
                return TypeRepr::Funcdef(qualified);
            }

            // AngelScript builtin funcdef / synthetic helper types that the
            // core database references but does not formally declare as
            // classes. Treat them as known types so user code referencing
            // them doesn't drown in false positives.
            if is_angelscript_builtin_type(&qualified) {
                return TypeRepr::Named(qualified);
            }
        }

        // Primitives occasionally appear as identifiers (e.g. when an
        // external database references them by name). Promote those to
        // actual primitives rather than looking them up as user types.
        if let Some(prim) = PrimitiveType::from_name(&qualified) {
            return TypeRepr::Primitive(prim);
        }
        if qualified == "void" {
            return TypeRepr::Void;
        }

        // The workspace declaration from above, now that the primitives have
        // had their say — a keyword-spelled type is not something a plugin
        // can shadow. Answered with the DECLARATION's qualified name, not the
        // spelling, so `CoroutineFunc` written inside `namespace Nz` is the
        // same `Nz::CoroutineFunc` a non-colliding name would have resolved
        // to through `resolve_unqualified` below.
        if let Some(declared) = workspace_shadowing {
            return TypeRepr::Named(declared);
        }

        if self.scope.resolves(&qualified) {
            // A nested engine enum spelled the way the game wants it — no
            // module prefix — resolves here now (B052), but equality is by
            // the stored key, so hand back the key rather than the spelling.
            if let Some(key) = self
                .scope
                .accepts_enum(&qualified)
                .and_then(|proof| self.scope.canonical_enum_key(&qualified, proof))
            {
                return TypeRepr::Named(key);
            }
            return TypeRepr::Named(qualified);
        }

        // Namespace-scoped fallback: try each active namespace prefix,
        // deepest first. This lets `Foo` inside `namespace Ns { … }` be
        // understood as `Ns::Foo`.
        for depth in (1..=self.namespace_stack.len()).rev() {
            let ns = self.namespace_stack[..depth].join("::");
            let candidate = format!("{}::{}", ns, qualified);
            if self.scope.lookup_funcdef(&candidate).is_some() {
                return TypeRepr::Funcdef(candidate);
            }
            if self.scope.resolves(&candidate) {
                return TypeRepr::Named(candidate);
            }
        }

        // Last resort: ask the scope to match the unqualified name
        // against any known type/enum whose qualified tail matches. This
        // is where Nadeo types like `CGameCtnEditorFree` (stored as
        // `Game::CGameCtnEditorFree` in the typedb) get rescued.
        if !qualified.contains("::") {
            if let Some(resolved) = self.scope.resolve_unqualified(&qualified) {
                if self.scope.lookup_funcdef(&resolved).is_some() {
                    return TypeRepr::Funcdef(resolved);
                }
                return TypeRepr::Named(resolved);
            }
        } else if let Some(resolved) = self.scope.resolve_qualified_suffix(&qualified) {
            if self.scope.lookup_funcdef(&resolved).is_some() {
                return TypeRepr::Funcdef(resolved);
            }
            return TypeRepr::Named(resolved);
        }

        self.diagnostics.push(ResolveDiagnostic {
            span: qname.span,
            unknown_name: qualified.clone(),
        });
        TypeRepr::Error(qualified)
    }
}

/// Hardcoded set of AngelScript builtin / Core SDK types referenced by
/// name in the Openplanet docs but not declared as classes in the JSON
/// type database. Keeping this list small and local keeps the resolver
/// honest — we don't want to accidentally mask genuine typos.
///
/// This delegates to [`super::builtins::is_builtin_type`] for the
/// `CoroutineFunc` funcdef family so the checker and resolver stay in
/// sync, and adds a couple of resolver-only names that never surface as
/// bare identifiers in expression position.
fn is_angelscript_builtin_type(name: &str) -> bool {
    if super::builtins::is_builtin_type(name) {
        return true;
    }
    matches!(
        name,
        // Compiler-intrinsic handle wrapper introduced by some
        // AngelScript builds.
        "awaitable"
            // Generic `ref` / handle-to-anything type occasionally
            // referenced in Core docs as an opaque handle target.
            | "ref"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize_filtered;
    use crate::parser::Parser;
    use crate::symbols::scope::{Symbol, SymbolKind};
    use crate::symbols::SymbolTable;

    fn parse_type(source: &str) -> TypeExpr {
        let tokens = tokenize_filtered(source);
        let mut parser = Parser::new(&tokens, source);
        parser
            .parse_type_expr()
            .expect("type expression should parse")
    }

    fn add_class(ws: &mut SymbolTable, name: &str) {
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![Symbol {
                name: name.to_string(),
                kind: SymbolKind::Class { parents: vec![] },
                span: Span::new(0, 0),
                file_id: fid,
                doc: None,
            }],
        );
    }

    /// The shipped type-DB fixtures, loaded as a `TypeIndex`.
    fn typedb_fixture_index() -> crate::typedb::index::TypeIndex {
        let fixtures =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
        crate::typedb::index::TypeIndex::load(
            &fixtures.join("OpenplanetCore.json"),
            &fixtures.join("OpenplanetNext.json"),
        )
        .expect("typedb fixtures must load")
    }

    /// `(spelling, verdict)` for every row of the committed B052 probe corpus.
    fn b052_probe_corpus() -> Vec<(String, String)> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("docs/probes/b052-qualified-enum-spellings.tsv");
        let text = std::fs::read_to_string(&path).expect("the probe corpus must be committed");
        text.lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .skip(1) // column header
            .map(|l| {
                let mut c = l.split('\t');
                let spelling = c.next().expect("a spelling column").to_string();
                let verdict = c.next().expect("a verdict column").to_string();
                (spelling, verdict)
            })
            .collect()
    }

    /// Spellings the game ACCEPTED that are NOT engine facts, and so must not
    /// be read as evidence about this crate. `docs/probes/README.md` explains
    /// the asymmetry: a game REJECT is solid evidence about the engine, but a
    /// game ACCEPT on an unrecognised prefix may only be an artefact of the
    /// probing machine's installed plugins.
    ///
    /// Each of these compiled only because a plugin on that machine declares
    /// the namespace — `MLFeedRaceData.op`, `vehicle-state-list-dev` and
    /// friends — or, for `Foo`, because the probe plugin declared it itself.
    /// A workspace with none of those installed must reject them, and does.
    const PLUGIN_NAMESPACE_ARTEFACTS: [&str; 12] = [
        "Foo::Bar::VirtualKey",
        "Foo::ESceneCharVisNotice",
        "Foo::VirtualKey",
        "GameData::ESceneCharVisNotice",
        "GameData::ESceneVehicleVisEvent",
        "GameData::VirtualKey",
        "MLFeed::VirtualKey",
        "MLHook::VirtualKey",
        "MathX::ESceneCharVisNotice",
        "MathX::ESceneVehicleVisEvent",
        "MathX::VirtualKey",
        "VehicleState::VirtualKey",
    ];

    /// `GameData` is a live AngelScript scope that NEITHER shipped database
    /// records, so this crate cannot know it is one.
    ///
    /// This is not a plugin artefact and was mislabelled as one when first
    /// written. Nothing on the probing machine declares `namespace GameData`
    /// — all 44 installed plugin entries, all 9 `.op` archives unzipped,
    /// `DisabledPlugins`, `Scripts`, `Extract` and this repo's own fixture
    /// plugins were searched, and the only file containing that string is
    /// `Openplanet.h` itself.
    ///
    /// What the row actually is, is B052's ancestor walk working normally.
    /// `EGameInventoryItemClass` is a globally visible enum — it is ACCEPTED
    /// bare too — and it lives in the header's trailing `namespace Enums`,
    /// not in `namespace GameData`. So it is not a member of that scope, it
    /// is reached THROUGH it, exactly as `Meta::EPlugAnimGraphStateSelectCond`
    /// is. The LSP resolves it the moment it believes `GameData` is a scope,
    /// and refuses it otherwise. It refuses.
    ///
    /// The anomaly is `GameData` alone, and it is exactly one name. Pairing
    /// `<Module>::ZZZNoSuchEnumName` over all 20 Nadeo module names in the
    /// committed corpus splits them cleanly:
    ///
    /// * 4 answer `Identifier ... is not a data type in namespace 'X' or
    ///   parent` — they ARE scopes: `Audio`, `Meta`, `Net`, `GameData`.
    /// * 16 answer `Namespace 'X' doesn't exist.` — `Game`, `Scene`, `Plug`,
    ///   `Hms`, `System`, `Control`, and the rest.
    ///
    /// And three of the four are fully explained: `Audio`, `Meta` and `Net`
    /// are also real CORE namespaces, so `build_scope_index` already has
    /// them and `scope_exists` answers true. `GameData` is not a Core
    /// namespace — the Core DB declares 30 top-level namespaces and it is not
    /// among them.
    ///
    /// So the rule for the other 19 is "a module name is a scope iff Core
    /// declares it as one", and `GameData` is the single exception with no
    /// evidence behind it in either DB.
    ///
    /// OPEN QUESTION, deliberately left open rather than guessed at: what
    /// makes `GameData` a live scope when `Game`, `Hms`, `Plug` and `Scene`
    /// are not? Nothing in the dump or the header distinguishes them — all
    /// twenty are `ns` keys and all twenty have a `namespace X { }` block in
    /// the header. Until that is known, excluding this row is the honest
    /// call: the crate would have to hardcode `GameData` to match, and
    /// hardcoding an engine fact nothing records is how this repo's earlier
    /// wrong numbers were made.
    const UNEXPLAINED_ENGINE_SCOPES: [&str; 1] = ["GameData::EGameInventoryItemClass"];

    /// A pre-existing LEXER gap, not a resolution one: `string` is tokenised
    /// as a keyword, so `string::X` never parses as a type expression at all.
    /// Pinned so the gap stays visible; B052 recorded the same three.
    const PARSER_GAP: [&str; 4] = [
        "string::ESceneCharVisNotice",
        "string::ESceneVehicleVisEvent",
        "string::VirtualKey",
        "string::ZZZNoSuchEnumName",
    ];

    /// B056: every qualified and bare TYPE spelling in the committed corpus,
    /// resolved through the real `TypeResolver`, checked against the live
    /// game's own verdict.
    ///
    /// `docs/probes/b052-qualified-enum-spellings.tsv` is the compiler's
    /// verbatim answer for 1901 spellings submitted as `<spelling> v;`
    /// (RemoteBuild, Trackmania + Openplanet 1.29.5, 2026-08-30). This drives
    /// the same shape — a top-level type reference with an empty namespace
    /// stack — so it exercises `resolves`, the namespace walk and the
    /// `resolve_unqualified` / `resolve_qualified_suffix` fallbacks exactly as
    /// the checker does, rather than re-implementing the rule in the test.
    ///
    /// The rule the corpus supports, and the one implemented:
    ///
    /// * a type or enum has a SOURCE NAME — for Nadeo, its index key minus
    ///   the loader's synthetic leading module segment; for Core, its key in
    ///   full; for a header enum, its bare name — and a spelling is accepted
    ///   when it equals that name;
    /// * or `P::T` where P is a REAL scope and T is a globally visible enum,
    ///   which is AngelScript's ancestor walk (B052, already landed).
    ///
    /// The asymmetry is the part that is easy to get wrong in either
    /// direction: `CGameCtnBlock::ECardinalDirections` is ACCEPTED while
    /// `Game::CGameCtnBlock::ECardinalDirections` is REJECTED, and
    /// `UI::Plot::Axis` is ACCEPTED while `Plot::Axis` is REJECTED. A rule
    /// phrased as "drop leading segments" gets the second pair wrong; one
    /// phrased as "require the whole key" gets the first pair wrong.
    #[test]
    fn every_probed_type_spelling_matches_the_games_verdict() {
        let index = typedb_fixture_index();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        let corpus = b052_probe_corpus();
        assert_eq!(
            corpus.len(),
            1901,
            "the probe corpus changed size — re-probe the game before \
             touching this rule"
        );

        let mut wrong: Vec<String> = Vec::new();
        for (spelling, verdict) in &corpus {
            if PARSER_GAP.contains(&spelling.as_str()) {
                continue;
            }
            // An empty workspace has none of the probing machine's plugins,
            // so these must be rejected here even though the game took them.
            let expected_resolves = if PLUGIN_NAMESPACE_ARTEFACTS.contains(&spelling.as_str())
                || UNEXPLAINED_ENGINE_SCOPES.contains(&spelling.as_str())
            {
                false
            } else {
                verdict == "ACCEPTED"
            };

            let tokens = tokenize_filtered(spelling);
            let mut parser = Parser::new(&tokens, spelling);
            let Ok(ty) = parser.parse_type_expr() else {
                wrong.push(format!("{spelling} (did not parse)"));
                continue;
            };
            let mut r = TypeResolver::new(&scope, spelling);
            let _ = r.resolve(&ty);
            let resolved = r.take_diagnostics().is_empty();

            if resolved != expected_resolves {
                let direction = if resolved {
                    "accepted, game REJECTS"
                } else {
                    "rejected, game ACCEPTS"
                };
                wrong.push(format!("{spelling} [{verdict}] {direction}"));
            }
        }

        assert!(
            wrong.is_empty(),
            "{} of {} probed spellings disagree with the game: {:#?}",
            wrong.len(),
            corpus.len(),
            &wrong[..wrong.len().min(30)]
        );
    }

    #[test]
    fn resolve_primitive_int() {
        let source = "int";
        let ty = parse_type(source);
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, None);
        let mut r = TypeResolver::new(&scope, source);
        let out = r.resolve(&ty);
        assert_eq!(out, TypeRepr::Primitive(PrimitiveType::Int));
        assert!(r.take_diagnostics().is_empty());
    }

    #[test]
    fn resolve_void_keyword() {
        let source = "void";
        let ty = parse_type(source);
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, None);
        let mut r = TypeResolver::new(&scope, source);
        assert_eq!(r.resolve(&ty), TypeRepr::Void);
    }

    #[test]
    fn resolve_handle_of_known_class() {
        let source = "Foo@";
        let ty = parse_type(source);
        let mut ws = SymbolTable::new();
        add_class(&mut ws, "Foo");
        let scope = GlobalScope::new(&ws, None);
        let mut r = TypeResolver::new(&scope, source);
        let out = r.resolve(&ty);
        assert_eq!(
            out,
            TypeRepr::Handle(Box::new(TypeRepr::Named("Foo".into())))
        );
        assert!(r.take_diagnostics().is_empty());
    }

    #[test]
    fn resolve_unknown_named_errors_with_diagnostic() {
        let source = "NotReal";
        let ty = parse_type(source);
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, None);
        let mut r = TypeResolver::new(&scope, source);
        let out = r.resolve(&ty);
        assert_eq!(out, TypeRepr::Error("NotReal".into()));
        let diags = r.take_diagnostics();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].unknown_name, "NotReal");
        assert!(diags[0].message().contains("NotReal"));
    }

    #[test]
    fn resolve_template_array_of_int() {
        // Note: the parser normalizes `array<int>` into `TypeExprKind::Array`,
        // not `Template`, so the resolver reports it as `TypeRepr::Array`.
        let source = "array<int>";
        let ty = parse_type(source);
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, None);
        let mut r = TypeResolver::new(&scope, source);
        let out = r.resolve(&ty);
        assert_eq!(
            out,
            TypeRepr::Array(Box::new(TypeRepr::Primitive(PrimitiveType::Int)))
        );
        assert!(r.take_diagnostics().is_empty());
    }

    #[test]
    fn resolve_nested_array_of_handle() {
        let source = "array<Foo@>";
        let ty = parse_type(source);
        let mut ws = SymbolTable::new();
        add_class(&mut ws, "Foo");
        let scope = GlobalScope::new(&ws, None);
        let mut r = TypeResolver::new(&scope, source);
        let out = r.resolve(&ty);
        assert_eq!(
            out,
            TypeRepr::Array(Box::new(TypeRepr::Handle(Box::new(TypeRepr::Named(
                "Foo".into()
            )))))
        );
        assert!(r.take_diagnostics().is_empty());
    }

    #[test]
    fn resolve_user_template_generic() {
        // Identifier-based templates (`Grid<int>`) do hit the parser's
        // `Template` arm, so this exercises the `Generic { base, args }`
        // path. Note: the parser does not currently emit `Template` for
        // the `dictionary<K,V>` keyword form — see the resolver test
        // notes for why.
        let source = "Grid<int>";
        let ty = parse_type(source);
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, None);
        let mut r = TypeResolver::new(&scope, source);
        let out = r.resolve(&ty);
        assert_eq!(
            out,
            TypeRepr::Generic {
                base: "Grid".into(),
                args: vec![TypeRepr::Primitive(PrimitiveType::Int)],
            }
        );
        // Template base names are trusted, so no diagnostic even though
        // `Grid` isn't in the workspace.
        assert!(r.take_diagnostics().is_empty());
    }

    #[test]
    fn resolve_const_int_keyword() {
        let source = "const int";
        let ty = parse_type(source);
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, None);
        let mut r = TypeResolver::new(&scope, source);
        let out = r.resolve(&ty);
        assert_eq!(
            out,
            TypeRepr::Const(Box::new(TypeRepr::Primitive(PrimitiveType::Int)))
        );
    }

    #[test]
    fn resolve_coroutine_func_builtin() {
        // `CoroutineFunc` is a synthetic AngelScript builtin — should not
        // emit an unknown-type diagnostic even with an empty scope.
        let source = "CoroutineFunc";
        let ty = parse_type(source);
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, None);
        let mut r = TypeResolver::new(&scope, source);
        let out = r.resolve(&ty);
        assert_eq!(out, TypeRepr::Named("CoroutineFunc".into()));
        assert!(r.take_diagnostics().is_empty());
    }

    #[test]
    fn resolve_nvg_font_builtin() {
        // Typedb omits `nvg::Font` but the game accepts it (tm-dashboard).
        let source = "nvg::Font";
        let ty = parse_type(source);
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, None);
        let mut r = TypeResolver::new(&scope, source);
        let out = r.resolve(&ty);
        assert_eq!(out, TypeRepr::Named("nvg::Font".into()));
        assert!(
            r.take_diagnostics().is_empty(),
            "nvg::Font must not be unknown-type"
        );
    }

    #[test]
    fn resolve_unqualified_walks_workspace_namespaces() {
        // A workspace type declared inside `namespace Ns` should be
        // resolvable by its short name from an unrelated file with no
        // active namespace stack, via the last-resort short-name lookup.
        let source = "Foo";
        let ty = parse_type(source);
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            vec![Symbol {
                name: "Ns::Foo".to_string(),
                kind: SymbolKind::Class { parents: vec![] },
                span: Span::new(0, 0),
                file_id: fid,
                doc: None,
            }],
        );
        let scope = GlobalScope::new(&ws, None);
        let mut r = TypeResolver::new(&scope, source);
        let out = r.resolve(&ty);
        assert_eq!(out, TypeRepr::Named("Ns::Foo".into()));
        assert!(r.take_diagnostics().is_empty());
    }

    #[test]
    fn resolve_array_shorthand() {
        let source = "int[]";
        let ty = parse_type(source);
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, None);
        let mut r = TypeResolver::new(&scope, source);
        let out = r.resolve(&ty);
        // Parser may render `int[]` as either an `Array` shorthand or a
        // `Template(array, [int])` — accept whichever shape it picked, as
        // long as it's semantically equivalent.
        match out {
            TypeRepr::Array(inner) => {
                assert_eq!(*inner, TypeRepr::Primitive(PrimitiveType::Int));
            }
            TypeRepr::Generic { base, args } => {
                assert_eq!(base, "array");
                assert_eq!(args, vec![TypeRepr::Primitive(PrimitiveType::Int)]);
            }
            other => panic!("unexpected shape: {:?}", other),
        }
    }

    #[test]
    fn resolve_qualified_enum_spellings_to_canonical_keys() {
        use crate::typedb::TypeIndex;

        let index = TypeIndex::load(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/typedb/OpenplanetCore.json")
                .as_path(),
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/typedb/OpenplanetNext.json")
                .as_path(),
        )
        .expect("failed to load type index");
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        // The nested engine enum maps from its source spelling to the stored
        // module-qualified key. B052 ancestor-walk spellings instead map to
        // the same bare global key as their unqualified spelling.
        for (scoped, canonical, bare) in [
            (
                "CGameEditorPluginMap::ECardinalDirections",
                "Game::CGameEditorPluginMap::ECardinalDirections",
                None,
            ),
            ("UI::VirtualKey", "VirtualKey", Some("VirtualKey")),
            ("UI::Font::VirtualKey", "VirtualKey", Some("VirtualKey")),
            (
                "CGameCtnApp::ESceneVehicleVisEvent",
                "ESceneVehicleVisEvent",
                Some("ESceneVehicleVisEvent"),
            ),
        ] {
            let scoped_ty = parse_type(scoped);
            let mut scoped_resolver = TypeResolver::new(&scope, scoped);
            let scoped_out = scoped_resolver.resolve(&scoped_ty);
            assert!(
                scoped_resolver.take_diagnostics().is_empty(),
                "the game accepts `{scoped}`"
            );
            assert_eq!(scoped_out, TypeRepr::Named(canonical.into()));

            if let Some(bare) = bare {
                let bare_ty = parse_type(bare);
                let mut bare_resolver = TypeResolver::new(&scope, bare);
                let bare_out = bare_resolver.resolve(&bare_ty);
                assert!(bare_resolver.take_diagnostics().is_empty());

                assert_eq!(bare_out, TypeRepr::Named(bare.into()));
                assert_eq!(
                    scoped_out, bare_out,
                    "`{scoped}` and `{bare}` are spellings of the same enum"
                );
            }
        }
    }

    // ── B073 / GH #38: a workspace TYPE shadows an engine funcdef ───────────

    /// Resolve `spelling` against a workspace holding exactly `symbols`,
    /// with the shipped typedb behind it and `stack` as the active
    /// namespace stack. Returns the repr and whether it resolved cleanly.
    fn resolve_against(
        index: &crate::typedb::index::TypeIndex,
        symbols: &[(&str, SymbolKind)],
        stack: &[&str],
        spelling: &str,
    ) -> (TypeRepr, bool) {
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        ws.set_file_symbols(
            fid,
            symbols
                .iter()
                .map(|(name, kind)| Symbol {
                    name: (*name).to_string(),
                    kind: kind.clone(),
                    span: Span::new(0, 0),
                    file_id: fid,
                    doc: None,
                })
                .collect(),
        );
        let scope = GlobalScope::new(&ws, Some(index));
        let ty = parse_type(spelling);
        let mut r = TypeResolver::new(&scope, spelling)
            .with_namespace_stack(stack.iter().map(|s| (*s).to_string()).collect());
        let out = r.resolve(&ty);
        let clean = r.take_diagnostics().is_empty();
        (out, clean)
    }

    /// Every shape a workspace can declare a TYPE in. `typedef` is in the
    /// list deliberately: it reuses `SymbolKind::Funcdef` and is the shape
    /// `lookup_funcdef` skips, so an enumeration that forgets it leaves the
    /// subtlest cell of the hole open.
    fn type_shapes() -> Vec<(&'static str, SymbolKind)> {
        vec![
            ("class", SymbolKind::Class { parents: vec![] }),
            ("interface", SymbolKind::Interface { parents: vec![] }),
            ("enum", SymbolKind::Enum { values: vec![] }),
            (
                "typedef",
                SymbolKind::Funcdef {
                    return_type: "int".into(),
                    params: vec![],
                    is_typedef: true,
                },
            ),
        ]
    }

    /// B073 / GH #38. A plugin's own type declaration shadows the ENGINE
    /// symbol of the same name. That rule was applied for class-vs-class
    /// collisions and not when the engine symbol is a **funcdef**, because
    /// `lookup_funcdef` walks workspace symbols but only *returns* on a real
    /// funcdef and then falls through to the typedb.
    ///
    /// Swept over three axes at once, because a guard that enumerates one
    /// axis passes while leaving the others open:
    ///
    /// * **shape** — class / interface / enum / `typedef`;
    /// * **engine name** — two real Core funcdefs, so the rule is not pinned
    ///   to one spelling;
    /// * **namespace** — declared at the top level, or inside `namespace Nz`
    ///   and reached bare (from inside the namespace and from outside it, via
    ///   the tail table) or fully qualified.
    ///
    /// The engine-funcdef control below proves no row here passes vacuously:
    /// with the same typedb and an empty workspace every one of these
    /// spellings is `TypeRepr::Funcdef`.
    #[test]
    fn workspace_type_shadows_an_engine_funcdef_in_every_shape() {
        let index = typedb_fixture_index();
        let mut wrong: Vec<String> = Vec::new();

        for engine in ["CoroutineFunc", "CoroutineFuncUserdataString"] {
            for (shape, kind) in type_shapes() {
                let namespaced = format!("Nz::{engine}");
                let cells: Vec<(&str, Vec<(&str, SymbolKind)>, Vec<&str>, &str, String)> = vec![
                    (
                        "declared at the top level, spelled bare",
                        vec![(engine, kind.clone())],
                        vec![],
                        engine,
                        engine.to_string(),
                    ),
                    (
                        "declared in `namespace Nz`, spelled bare from inside it",
                        vec![(namespaced.as_str(), kind.clone())],
                        vec!["Nz"],
                        engine,
                        namespaced.clone(),
                    ),
                    (
                        "declared in `namespace Nz`, spelled bare from outside it",
                        vec![(namespaced.as_str(), kind.clone())],
                        vec![],
                        engine,
                        namespaced.clone(),
                    ),
                    (
                        "declared in `namespace Nz`, spelled fully qualified",
                        vec![(namespaced.as_str(), kind.clone())],
                        vec![],
                        namespaced.as_str(),
                        namespaced.clone(),
                    ),
                ];
                for (cell, symbols, stack, spelling, expected) in cells {
                    let (out, clean) = resolve_against(&index, &symbols, &stack, spelling);
                    let want = TypeRepr::Named(expected.clone());
                    if out != want || !clean {
                        wrong.push(format!(
                            "{engine} as {shape}, {cell}: `{spelling}` -> {out:?} \
                             (clean={clean}), want {want:?}"
                        ));
                    }
                }
            }
        }

        assert!(
            wrong.is_empty(),
            "{} cells wrong: {:#?}",
            wrong.len(),
            wrong
        );
    }

    /// The other half of the rule, and the reason the fix cannot be "never
    /// resolve these names as funcdefs": with nothing in the workspace, a
    /// real engine funcdef must still resolve to the engine funcdef.
    ///
    /// The VALUE rows are the over-blocking edge B060 paid four review rounds
    /// for. A workspace variable or function that happens to share the name is
    /// a value, not a type, and shadows nothing.
    #[test]
    fn only_a_workspace_type_shadows_an_engine_funcdef() {
        let index = typedb_fixture_index();
        let mut wrong: Vec<String> = Vec::new();

        for engine in ["CoroutineFunc", "CoroutineFuncUserdataString"] {
            let namespaced = format!("Nz::{engine}");
            let value_shapes: Vec<(&str, Vec<(&str, SymbolKind)>)> = vec![
                ("nothing in the workspace", vec![]),
                (
                    "a workspace VARIABLE of the same name",
                    vec![(
                        engine,
                        SymbolKind::Variable {
                            type_name: "int".into(),
                        },
                    )],
                ),
                (
                    "a workspace FUNCTION of the same name",
                    vec![(
                        engine,
                        SymbolKind::Function {
                            return_type: "void".into(),
                            params: vec![],
                            min_args: 0,
                            imported: false,
                        },
                    )],
                ),
                (
                    "a workspace VARIABLE of the same name inside `namespace Nz`",
                    vec![(
                        namespaced.as_str(),
                        SymbolKind::Variable {
                            type_name: "int".into(),
                        },
                    )],
                ),
                (
                    "a workspace FUNCDEF of the same name — a funcdef is not a \
                     shadowing type, it IS the thing being named",
                    vec![(
                        engine,
                        SymbolKind::Funcdef {
                            return_type: "void".into(),
                            params: vec![],
                            is_typedef: false,
                        },
                    )],
                ),
            ];
            for (cell, symbols) in value_shapes {
                let (out, clean) = resolve_against(&index, &symbols, &[], engine);
                let want = TypeRepr::Funcdef(engine.to_string());
                if out != want || !clean {
                    wrong.push(format!(
                        "{engine} with {cell}: -> {out:?} (clean={clean}), want {want:?}"
                    ));
                }
            }
        }

        assert!(
            wrong.is_empty(),
            "{} cells wrong: {:#?}",
            wrong.len(),
            wrong
        );
    }

    /// The non-colliding control for each shape. Without it the sweep above
    /// cannot distinguish "shadowing works" from "this shape is simply never
    /// treated as a funcdef" — a fix that broke class resolution outright
    /// would still satisfy the shadowing rows.
    #[test]
    fn a_workspace_type_that_collides_with_nothing_resolves_to_itself() {
        let index = typedb_fixture_index();
        let mut wrong: Vec<String> = Vec::new();

        for (shape, kind) in type_shapes() {
            for (cell, symbols, stack, spelling, expected) in [
                (
                    "top level",
                    vec![("ZZZWidget", kind.clone())],
                    vec![],
                    "ZZZWidget",
                    "ZZZWidget",
                ),
                (
                    "inside `namespace Nz`, spelled bare",
                    vec![("Nz::ZZZWidget", kind.clone())],
                    vec!["Nz"],
                    "ZZZWidget",
                    "Nz::ZZZWidget",
                ),
                (
                    "inside `namespace Nz`, spelled fully qualified",
                    vec![("Nz::ZZZWidget", kind.clone())],
                    vec![],
                    "Nz::ZZZWidget",
                    "Nz::ZZZWidget",
                ),
            ] {
                let (out, clean) = resolve_against(&index, &symbols, &stack, spelling);
                let want = TypeRepr::Named(expected.to_string());
                if out != want || !clean {
                    wrong.push(format!(
                        "{shape} at {cell}: `{spelling}` -> {out:?} (clean={clean}), \
                         want {want:?}"
                    ));
                }
            }
        }

        assert!(
            wrong.is_empty(),
            "{} cells wrong: {:#?}",
            wrong.len(),
            wrong
        );
    }

    /// The builtin TABLE is the same hole one step along. `awaitable` is not
    /// a typedb funcdef — it is hardcoded in `is_angelscript_builtin_type` —
    /// and that check also answered from the engine before the workspace was
    /// consulted, so a plugin type of that name inside a namespace lost its
    /// qualifier and came back as the bare builtin.
    ///
    /// Guarding only `lookup_funcdef` leaves this cell red, which is why it
    /// is here rather than folded into the sweep above.
    #[test]
    fn workspace_type_shadows_a_hardcoded_builtin_type_name() {
        let index = typedb_fixture_index();
        let mut wrong: Vec<String> = Vec::new();

        for (shape, kind) in type_shapes() {
            let (out, clean) = resolve_against(
                &index,
                &[("Nz::awaitable", kind.clone())],
                &["Nz"],
                "awaitable",
            );
            let want = TypeRepr::Named("Nz::awaitable".into());
            if out != want || !clean {
                wrong.push(format!(
                    "`awaitable` as {shape} in `namespace Nz` -> {out:?} \
                     (clean={clean}), want {want:?}"
                ));
            }
        }

        // …and with nothing declared it is still the builtin, so the row
        // above is not passing because the table stopped working.
        let (out, clean) = resolve_against(&index, &[], &[], "awaitable");
        assert_eq!(out, TypeRepr::Named("awaitable".into()));
        assert!(clean, "`awaitable` is a builtin and must resolve cleanly");

        assert!(
            wrong.is_empty(),
            "{} cells wrong: {:#?}",
            wrong.len(),
            wrong
        );
    }
}
