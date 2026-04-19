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

        // AngelScript builtin funcdef / synthetic helper types that the
        // core database references but does not formally declare as
        // classes. Treat them as known types so user code referencing
        // them doesn't drown in false positives.
        if is_angelscript_builtin_type(&qualified) {
            return TypeRepr::Named(qualified);
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

        if self.scope.resolves(&qualified) {
            return TypeRepr::Named(qualified);
        }

        // Namespace-scoped fallback: try each active namespace prefix,
        // deepest first. This lets `Foo` inside `namespace Ns { … }` be
        // understood as `Ns::Foo`.
        for depth in (1..=self.namespace_stack.len()).rev() {
            let ns = self.namespace_stack[..depth].join("::");
            let candidate = format!("{}::{}", ns, qualified);
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
                return TypeRepr::Named(resolved);
            }
        } else if let Some(resolved) = self.scope.resolve_qualified_suffix(&qualified) {
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
                kind: SymbolKind::Class {
                    parents: vec![],
                    members: vec![],
                },
                span: Span::new(0, 0),
                file_id: fid,
                doc: None,
            }],
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
                kind: SymbolKind::Class {
                    parents: vec![],
                    members: vec![],
                },
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
    fn resolve_qualified_nested_enum_cgame_editor_plugin_map() {
        use crate::typedb::TypeIndex;

        let source = "CGameEditorPluginMap::ECardinalDirections";
        let ty = parse_type(source);
        let ws = SymbolTable::new();
        let index = TypeIndex::load(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/typedb/OpenplanetCore.json")
                .as_path(),
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/typedb/OpenplanetNext.json")
                .as_path(),
        )
        .expect("failed to load type index");
        let scope = GlobalScope::new(&ws, Some(&index));
        let mut r = TypeResolver::new(&scope, source);
        let out = r.resolve(&ty);
        let diags = r.take_diagnostics();
        assert!(
            diags.is_empty(),
            "expected no diagnostics, got: {:?}",
            diags
        );
        assert_eq!(
            out,
            TypeRepr::Named("Game::CGameEditorPluginMap::ECardinalDirections".into())
        );
    }
}
