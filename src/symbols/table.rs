use std::collections::{BTreeMap, HashMap};

use super::scope::{DeclModifiers, Symbol, SymbolKind};
use crate::parser::ast;

/// B114: the modifiers a FUNCTION-shaped declaration was written with.
/// Covers class methods, constructors, destructors, interface methods and
/// free functions — the parser fills the same `FunctionDecl` for all of
/// them, so one helper keeps them from drifting apart.
fn fn_modifiers(f: &ast::FunctionDecl) -> DeclModifiers {
    DeclModifiers {
        is_shared: f.is_shared,
        is_const: f.is_const,
        is_override: f.is_override,
        is_final: f.is_final,
        is_private: f.is_private,
        is_protected: f.is_protected,
        ..DeclModifiers::default()
    }
}

/// B114: the modifiers a CLASS declaration was written with.
fn class_modifiers(c: &ast::ClassDecl) -> DeclModifiers {
    DeclModifiers {
        is_shared: c.is_shared,
        is_mixin: c.is_mixin,
        is_abstract: c.is_abstract,
        ..DeclModifiers::default()
    }
}

/// B114: `shared` is the only modifier an interface, enum or funcdef can
/// carry.
fn shared_only(is_shared: bool) -> DeclModifiers {
    DeclModifiers {
        is_shared,
        ..DeclModifiers::default()
    }
}

/// Extract a base-class name from a `TypeExpr`. Strips `Handle`, `Const`,
/// `Reference` wrappers and pulls the underlying `QualifiedName` if present.
/// Returns `None` for anything that isn't a simple named type — e.g. an
/// erroneous parse or a primitive (which can't be a base class).
fn base_class_name(source: &str, ty: &ast::TypeExpr) -> Option<String> {
    match &ty.kind {
        ast::TypeExprKind::Named(qn) => Some(qn.to_string(source)),
        ast::TypeExprKind::Handle(inner)
        | ast::TypeExprKind::Reference(inner, _)
        | ast::TypeExprKind::Const(inner) => base_class_name(source, inner),
        ast::TypeExprKind::Template(qn, _) => Some(qn.to_string(source)),
        _ => None,
    }
}

/// `(name, value)` for every member of an enum declaration.
///
/// AngelScript allows any constant expression as an initializer, but hover
/// only needs the shapes real plugins use: an integer literal (optionally
/// negated or hexadecimal), or no initializer at all, in which case the value
/// is the previous member's plus one. Anything else leaves the value unknown,
/// and — because the implicit run is broken by it — so are the members that
/// follow it.
fn enum_values(source: &str, en: &ast::EnumDecl) -> Vec<(String, Option<i64>)> {
    let mut next_implicit: Option<i64> = Some(0);
    en.values
        .iter()
        .map(|v| {
            let value = match &v.value {
                Some(expr) => const_int_expr(expr),
                None => next_implicit,
            };
            next_implicit = value.and_then(|v| v.checked_add(1));
            (v.name.text(source).to_string(), value)
        })
        .collect()
}

/// Integer value of a literal initializer expression, when it is one.
fn const_int_expr(expr: &ast::Expr) -> Option<i64> {
    match &expr.kind {
        ast::ExprKind::IntLit(v) => Some(*v),
        ast::ExprKind::HexLit(v) => i64::try_from(*v).ok(),
        ast::ExprKind::Unary {
            op: ast::UnaryOp::Neg,
            expr,
        } => const_int_expr(expr).and_then(i64::checked_neg),
        _ => None,
    }
}

/// Extract `(params, min_args)` from an AST parameter list. `params` is a
/// `Vec<(name, type_text)>` using source text for the type expression. `min_args`
/// counts parameters with no default value (AngelScript, like C++, requires
/// defaults be contiguous at the tail of the list).
fn extract_params(source: &str, params: &[ast::Param]) -> (Vec<(String, String)>, usize) {
    let out: Vec<(String, String)> = params
        .iter()
        .map(|p| {
            let name = p
                .name
                .as_ref()
                .map(|i| i.text(source).to_string())
                .unwrap_or_default();
            let type_text = p.type_expr.span.text(source).to_string();
            (name, type_text)
        })
        .collect();
    let min_args = params.iter().filter(|p| p.default_value.is_none()).count();
    (out, min_args)
}

/// Per-file symbol contributions
#[derive(Debug, Default, Clone)]
pub struct FileSymbols {
    pub file_id: usize,
    pub symbols: Vec<Symbol>,
}

/// Workspace-wide symbol table
#[derive(Debug, Default, Clone)]
pub struct SymbolTable {
    /// file_id → file symbols
    files: HashMap<usize, FileSymbols>,
    /// name → list of symbols (cross-file)
    global_index: HashMap<String, Vec<(usize, usize)>>, // (file_id, symbol_index)
    /// last `::`-segment → symbols carrying that tail (qualified names only;
    /// bare names live in `global_index`). Backs `lookup_tail` (GH #41).
    tail_index: HashMap<String, Vec<(usize, usize)>>,
    /// every `::`-prefix → symbols registered under it. Backs
    /// `lookup_members` (GH #41).
    member_index: HashMap<String, Vec<(usize, usize)>>,
    /// File ids hidden from lookups (strict own-`exports` files — decls
    /// there are consumer-only; the defining plugin's own module view
    /// hides them, game parity 2026-08-23). Mutation before pooling is
    /// complete; empty = no filtering.
    hidden_files: std::collections::HashSet<usize>,
    /// True when this pool is the plugin's COMPLETE compile surface. Only
    /// then is the absence of a name evidence that the game compiler would
    /// reject it — AngelScript namespaces are open and additive across every
    /// file the game compiles together, so one unread file may contribute the
    /// very member being called (GH #58).
    ///
    /// The conditions are enumerated ONCE, on
    /// `workspace::load::PluginWorkspaceLoad::dependency_closure_complete`;
    /// do not restate them here, or the two copies will drift. Defaults to
    /// false so ad-hoc pools (single-file diagnostics, in-memory test tables)
    /// never claim an authority they don't have.
    dependency_closure_complete: bool,
    next_file_id: usize,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allocate_file_id(&mut self) -> usize {
        let id = self.next_file_id;
        self.next_file_id += 1;
        id
    }

    /// Register symbols from a parsed file. Replaces any previous symbols for this file.
    pub fn set_file_symbols(&mut self, file_id: usize, symbols: Vec<Symbol>) {
        // Remove old entries from global index
        self.remove_file(file_id);

        // Add new entries
        for (idx, sym) in symbols.iter().enumerate() {
            self.global_index
                .entry(sym.name.clone())
                .or_default()
                .push((file_id, idx));
            if let Some(tail) = sym.name.rsplit("::").next() {
                // Guard: for a bare name `tail == name`; the exact index
                // already covers it, the tail index is for qualified names.
                if tail != sym.name.as_str() {
                    self.tail_index
                        .entry(tail.to_string())
                        .or_default()
                        .push((file_id, idx));
                }
            }
            // Register under every `::`-prefix so `lookup_members(owner)`
            // matches everything `name.starts_with(owner + "::")` would.
            let mut acc = String::new();
            for seg in sym.name.split("::") {
                if acc.is_empty() {
                    acc.push_str(seg);
                } else {
                    self.member_index
                        .entry(acc.clone())
                        .or_default()
                        .push((file_id, idx));
                    acc.push_str("::");
                    acc.push_str(seg);
                }
            }
        }

        self.files.insert(file_id, FileSymbols { file_id, symbols });
    }

    pub fn remove_file(&mut self, file_id: usize) {
        if self.files.remove(&file_id).is_some() {
            for entries in self.global_index.values_mut() {
                entries.retain(|(fid, _)| *fid != file_id);
            }
            for entries in self.tail_index.values_mut() {
                entries.retain(|(fid, _)| *fid != file_id);
            }
            for entries in self.member_index.values_mut() {
                entries.retain(|(fid, _)| *fid != file_id);
            }
        }
    }

    pub fn lookup(&self, name: &str) -> Vec<&Symbol> {
        self.global_index
            .get(name)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|(fid, _)| !self.hidden_files.contains(fid))
                    .filter_map(|(file_id, idx)| self.files.get(file_id)?.symbols.get(*idx))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn all_symbols(&self) -> impl Iterator<Item = &Symbol> {
        self.files
            .values()
            .filter(|f| !self.hidden_files.contains(&f.file_id))
            .flat_map(|f| f.symbols.iter())
    }

    /// Symbols declared in one file, by file id (unused-decl analysis walks
    /// per-file so findings carry a file_id).
    pub fn file_symbols(&self, file_id: usize) -> &[Symbol] {
        self.files
            .get(&file_id)
            .map(|f| f.symbols.as_slice())
            .unwrap_or(&[])
    }

    /// Symbols whose qualified name ends in `::tail`. Index-backed —
    /// O(candidates) instead of a linear scan of every workspace symbol
    /// (GH #41). Bare names (no `::`) are excluded: they're exact
    /// `global_index` hits, not tail matches.
    pub fn lookup_tail(&self, tail: &str) -> Vec<&Symbol> {
        let empty: Vec<(usize, usize)> = Vec::new();
        let entries = self.tail_index.get(tail).unwrap_or(&empty);
        entries
            .iter()
            .filter(|(fid, _)| !self.hidden_files.contains(fid))
            .filter_map(|(file_id, idx)| self.files.get(file_id)?.symbols.get(*idx))
            .collect()
    }

    /// Every symbol whose qualified name starts with `owner + "::"`
    /// (e.g. `Class` matches `Class::field`, `Class::method`). Index-backed
    /// via `member_index` (GH #41).
    pub fn lookup_members(&self, owner: &str) -> Vec<&Symbol> {
        let empty: Vec<(usize, usize)> = Vec::new();
        let entries = self.member_index.get(owner).unwrap_or(&empty);
        entries
            .iter()
            .filter(|(fid, _)| !self.hidden_files.contains(fid))
            .filter_map(|(file_id, idx)| self.files.get(file_id)?.symbols.get(*idx))
            .collect()
    }

    /// True when the workspace declares `ns` as a NAMESPACE (B107).
    ///
    /// A class is not one. AngelScript has no statics, so `MyClass::` names
    /// nothing — the same fence GH #58 decision 4 keeps on the checking
    /// side, and the reason this is a predicate rather than "did the walk
    /// return anything": a class's members ARE pooled as `Class::member`, so
    /// enumerating without asking this first would spill them.
    pub fn namespace_is_declared(&self, ns: &str) -> bool {
        !ns.is_empty()
            && self
                .lookup(ns)
                .iter()
                .any(|sym| matches!(sym.kind, SymbolKind::Namespace))
    }

    /// The DIRECT members of namespace `ns`, as symbols — one per name, in
    /// deterministic order (B107).
    ///
    /// THE one enumeration of a workspace namespace's members. It was
    /// already here in spirit, buried inside
    /// `GlobalScope::workspace_namespace_member_names`, which projected the
    /// names out and dropped the symbols; that made it useless to anything
    /// needing a KIND, and completion consequently had no workspace producer
    /// at all. Hoisted rather than copied: that method is now a projection
    /// of this, so the checker's near-name hint and the editor's `Ns::`
    /// completion read one walk. A rule living at one of N sites is the
    /// recurring defect shape in this tree (B073, B076, B098).
    ///
    /// "Direct" excludes anything deeper: `A::B::Deep` is a member of
    /// `A::B`, never of `A`. `A::B` itself IS a member of `A`, because a
    /// nested namespace is pooled as its own `Namespace` symbol.
    ///
    /// Deliberately NOT gated on the dependency-closure fence. This answers
    /// "what can I see", not "what does not exist"; the fence belongs to the
    /// caller that makes the second claim.
    pub fn namespace_member_symbols(&self, ns: &str) -> Vec<&Symbol> {
        let prefix = format!("{ns}::");
        let mut seen: BTreeMap<&str, &Symbol> = BTreeMap::new();
        for sym in self.lookup_members(ns) {
            let Some(rest) = sym.name.strip_prefix(&prefix) else {
                continue;
            };
            if rest.is_empty() || rest.contains("::") {
                continue;
            }
            seen.entry(rest).or_insert(sym);
        }
        seen.into_values().collect()
    }

    /// Hide a file's symbols from lookups (strict own-`exports` files —
    /// consumer-only decls, game parity 2026-08-23). `file_symbols` and
    /// other by-id accessors stay unfiltered so unused-decl analysis and
    /// hover still see them; only name-resolution lookups are affected.
    pub fn hide_file(&mut self, file_id: usize) {
        self.hidden_files.insert(file_id);
    }

    /// Reverse of [`Self::hide_file`] — a file always sees its own
    /// declarations (an export file's `import … from "Mod";` decls
    /// resolve within itself).
    pub fn unhide_file(&mut self, file_id: usize) {
        self.hidden_files.remove(&file_id);
    }

    /// True when `file_id` is currently hidden from lookups.
    pub fn is_file_hidden(&self, file_id: usize) -> bool {
        self.hidden_files.contains(&file_id)
    }

    /// Declare this pool the complete compile surface — see the
    /// `dependency_closure_complete` field docs, and the enumerated
    /// conditions they point at. Set by the snapshot builder from the
    /// loader's verdict; carried by `clone()` so the own-module view keeps
    /// it.
    pub fn mark_dependency_closure_complete(&mut self) {
        self.dependency_closure_complete = true;
    }

    /// True when this pool is the complete module closure (see the field
    /// docs). Diagnostics that treat a missing name as a game-compiler
    /// rejection must gate on this.
    pub fn dependency_closure_complete(&self) -> bool {
        self.dependency_closure_complete
    }

    /// Resolve a source-level reference by the same ladder the checker
    /// applies: exact qualified name first, then — when the name is
    /// qualified — an exact hit on the bare tail, then any symbol whose
    /// qualified tail matches. Absorbs the per-feature qualified→bare
    /// retry ladders that hover / goto-definition / call-hierarchy each
    /// used to carry inline (GH #41).
    pub fn lookup_reference(&self, name: &str) -> Vec<&Symbol> {
        let exact = self.lookup(name);
        if !exact.is_empty() {
            return exact;
        }
        if let Some((_, bare)) = name.rsplit_once("::") {
            let bare_hits = self.lookup(bare);
            if !bare_hits.is_empty() {
                return bare_hits;
            }
            return self.lookup_tail(bare);
        }
        Vec::new()
    }

    /// Extract symbols from a parsed AST file
    pub fn extract_symbols(file_id: usize, source: &str, file: &ast::SourceFile) -> Vec<Symbol> {
        let mut symbols = Vec::new();
        for item in &file.items {
            Self::extract_item_symbols(file_id, source, item, None, &mut symbols);
        }
        symbols
    }

    fn extract_item_symbols(
        file_id: usize,
        source: &str,
        item: &ast::Item,
        namespace: Option<&str>,
        out: &mut Vec<Symbol>,
    ) {
        let qualify = |name: &str| -> String {
            match namespace {
                Some(ns) => format!("{}::{}", ns, name),
                None => name.to_string(),
            }
        };

        match item {
            ast::Item::Class(cls) => {
                let class_name = qualify(cls.name.text(source));
                let parents = cls
                    .base_classes
                    .iter()
                    .filter_map(|b| base_class_name(source, b))
                    .collect();
                out.push(Symbol {
                    modifiers: class_modifiers(cls),
                    name: class_name.clone(),
                    kind: SymbolKind::Class {
                        parents,
                        // B111: a mixin never owes the interface it names.
                    },
                    span: cls.span,
                    file_id,
                    doc: None,
                });
                // Descend into class members. Each gets a qualified name like
                // "ClassName::member" (or "Ns::ClassName::member"). We lift
                // the raw source text of each member's type expression /
                // return type into the symbol so cross-file inherited access
                // (`GlobalScope::workspace_class_member`) can parse it into
                // a real `TypeRepr` later — see iter 28.
                let simple_class = cls.name.text(source).to_string();
                for member in &cls.members {
                    match member {
                        ast::ClassMember::Field(var) => {
                            let type_text = var.type_expr.span.text(source).to_string();
                            for decl in &var.declarators {
                                let mname = format!("{}::{}", class_name, decl.name.text(source));
                                out.push(Symbol {
                                    modifiers: DeclModifiers::default(),
                                    name: mname,
                                    kind: SymbolKind::Variable {
                                        type_name: type_text.clone(),
                                    },
                                    span: var.span,
                                    file_id,
                                    doc: None,
                                });
                            }
                        }
                        ast::ClassMember::Method(func) => {
                            let mname = format!("{}::{}", class_name, func.name.text(source));
                            let (params, min_args) = extract_params(source, &func.params);
                            let return_type = func.return_type.span.text(source).to_string();
                            out.push(Symbol {
                                modifiers: fn_modifiers(func),
                                name: mname,
                                kind: SymbolKind::Function {
                                    return_type,
                                    params,
                                    min_args,
                                    imported: false,
                                    // B101: the trailing `const`. The engine
                                    // treats `void M() const` and `void M()`
                                    // as different signatures.
                                },
                                span: func.span,
                                file_id,
                                doc: None,
                            });
                        }
                        ast::ClassMember::Constructor(func) => {
                            // Qualify as ClassName::ClassName — the simple
                            // unqualified class tail, not the namespace one.
                            let mname = format!("{}::{}", class_name, simple_class);
                            let (params, min_args) = extract_params(source, &func.params);
                            // Constructor "return type" is the class itself
                            // (as a value). Use the simple class name so a
                            // caller parsing via `TypeRepr::parse_type_string`
                            // gets a Named(simple) that downstream lookups
                            // can reason about.
                            out.push(Symbol {
                                modifiers: fn_modifiers(func),
                                name: mname,
                                kind: SymbolKind::Function {
                                    return_type: simple_class.clone(),
                                    params,
                                    min_args,
                                    imported: false,
                                    // A constructor cannot be const.
                                },
                                span: func.span,
                                file_id,
                                doc: None,
                            });
                        }
                        ast::ClassMember::Destructor(func) => {
                            // Destructor name in source is ~ClassName.
                            // Destructors have no return type — leave empty
                            // (matches the pre-iter-28 behaviour for them).
                            let mname = format!("{}::{}", class_name, func.name.text(source));
                            let (params, min_args) = extract_params(source, &func.params);
                            out.push(Symbol {
                                modifiers: fn_modifiers(func),
                                name: mname,
                                kind: SymbolKind::Function {
                                    return_type: String::new(),
                                    params,
                                    min_args,
                                    imported: false,
                                    // A destructor cannot be const.
                                },
                                span: func.span,
                                file_id,
                                doc: None,
                            });
                        }
                        ast::ClassMember::Property(prop) => {
                            let mname = format!("{}::{}", class_name, prop.name.text(source));
                            let type_text = prop.type_expr.span.text(source).to_string();
                            out.push(Symbol {
                                modifiers: DeclModifiers::default(),
                                name: mname,
                                kind: SymbolKind::Variable {
                                    type_name: type_text,
                                },
                                span: prop.span,
                                file_id,
                                doc: None,
                            });
                        }
                    }
                }
            }
            ast::Item::Interface(iface) => {
                // B054: mirror the class arm. The interface symbol carries
                // its declared bases, and every method becomes its own
                // `I::method` symbol — without those, hover, goto-definition,
                // workspace symbols and rename are all blind to a whole
                // member surface.
                let iface_name = qualify(iface.name.text(source));
                let parents = iface
                    .bases
                    .iter()
                    .filter_map(|b| base_class_name(source, b))
                    .collect();
                out.push(Symbol {
                    modifiers: shared_only(iface.is_shared),
                    name: iface_name.clone(),
                    kind: SymbolKind::Interface { parents },
                    span: iface.span,
                    file_id,
                    doc: None,
                });
                for method in &iface.methods {
                    let (params, min_args) = extract_params(source, &method.params);
                    out.push(Symbol {
                        // The METHOD's own modifiers — `void M() const;` is
                        // a const requirement. `shared` belongs to the
                        // interface declaration, not to each method.
                        modifiers: fn_modifiers(method),
                        name: format!("{}::{}", iface_name, method.name.text(source)),
                        kind: SymbolKind::Function {
                            return_type: method.return_type.span.text(source).to_string(),
                            params,
                            min_args,
                            imported: false,
                            // B101: `void M() const;` in an interface is a
                            // const REQUIREMENT, not decoration.
                        },
                        span: method.span,
                        file_id,
                        doc: None,
                    });
                }
            }
            ast::Item::Enum(en) => {
                let enum_name = qualify(en.name.text(source));
                let values = enum_values(source, en);
                out.push(Symbol {
                    modifiers: shared_only(en.is_shared),
                    name: enum_name.clone(),
                    kind: SymbolKind::Enum {
                        values: values.clone(),
                    },
                    span: en.span,
                    file_id,
                    doc: None,
                });
                // Also register each enum value
                for (val, (_, value)) in en.values.iter().zip(&values) {
                    out.push(Symbol {
                        // An enum VALUE carries no modifier of its own; it
                        // is covered by its enum's declaration span, which
                        // is how membership of a `shared` entity is
                        // decided (see `is_shared_or_inside_shared`).
                        modifiers: DeclModifiers::default(),
                        name: format!("{}::{}", enum_name, val.name.text(source)),
                        kind: SymbolKind::EnumValue {
                            enum_name: enum_name.clone(),
                            value: *value,
                        },
                        span: val.span,
                        file_id,
                        doc: None,
                    });
                }
            }
            ast::Item::Namespace(ns) => {
                let ns_name = qualify(ns.name.text(source));
                out.push(Symbol {
                    modifiers: DeclModifiers::default(),
                    name: ns_name.clone(),
                    kind: SymbolKind::Namespace,
                    span: ns.span,
                    file_id,
                    doc: None,
                });
                for sub_item in &ns.items {
                    Self::extract_item_symbols(file_id, source, sub_item, Some(&ns_name), out);
                }
            }
            ast::Item::Funcdef(fd) => {
                let name = qualify(fd.name.text(source));
                // B027: carry the full signature (modes included — the
                // `Reference` type span covers `&out`/`&in`/`&inout`) so
                // `GlobalScope::lookup_funcdef` can compare delegate args.
                let (params, _) = extract_params(source, &fd.params);
                out.push(Symbol {
                    modifiers: shared_only(fd.is_shared),
                    name,
                    kind: SymbolKind::Funcdef {
                        return_type: fd.return_type.span.text(source).to_string(),
                        params,
                        is_typedef: false,
                    },
                    span: fd.span,
                    file_id,
                    doc: None,
                });
            }
            ast::Item::Typedef(td) => {
                let name = qualify(td.name.text(source));
                out.push(Symbol {
                    modifiers: DeclModifiers::default(),
                    name,
                    // A typedef is a first-class type name. The current
                    // symbol surface already represents user-defined
                    // type-only aliases with `Funcdef`, so use that category
                    // rather than widening it for one declaration form.
                    // `is_typedef` keeps it out of callable-signature
                    // lookups (shape alone can't discriminate now that
                    // parameterless funcdefs carry signatures).
                    kind: SymbolKind::Funcdef {
                        return_type: td.target_type.span.text(source).to_string(),
                        params: Vec::new(),
                        is_typedef: true,
                    },
                    span: td.span,
                    file_id,
                    doc: None,
                });
            }
            ast::Item::Function(func) => {
                let name = qualify(func.name.text(source));
                let (params, min_args) = extract_params(source, &func.params);
                out.push(Symbol {
                    modifiers: fn_modifiers(func),
                    name,
                    kind: SymbolKind::Function {
                        return_type: func.return_type.span.text(source).to_string(),
                        params,
                        min_args,
                        imported: false,
                    },
                    span: func.span,
                    file_id,
                    doc: None,
                });
            }
            ast::Item::Import(import) => match &import.what {
                ast::ImportTarget::Function {
                    return_type,
                    name,
                    params,
                } => {
                    let qualified_name = qualify(name.text(source));
                    let (params, min_args) = extract_params(source, params);
                    out.push(Symbol {
                        modifiers: DeclModifiers::default(),
                        name: qualified_name,
                        kind: SymbolKind::Function {
                            return_type: return_type.span.text(source).to_string(),
                            params,
                            min_args,
                            // Import-site declaration: an alias for a function
                            // defined elsewhere — navigation should prefer the
                            // real definition when one exists in the workspace.
                            imported: true,
                            // `import` declares a free function.
                        },
                        span: import.span,
                        file_id,
                        doc: None,
                    });
                }
                ast::ImportTarget::Module { .. } => {}
            },
            ast::Item::VarDecl(var) => {
                // Lift the raw type-expression text (same as class fields) so
                // Ident value-lookup can parse a real TypeRepr instead of
                // collapsing globals to Named(varName) — better-totd FPs on
                // UI::PushStyleColor / PushFont / Json::ToFile.
                let type_text = var.type_expr.span.text(source).to_string();
                for decl in &var.declarators {
                    let name = qualify(decl.name.text(source));
                    out.push(Symbol {
                        modifiers: DeclModifiers::default(),
                        name,
                        kind: SymbolKind::Variable {
                            type_name: type_text.clone(),
                        },
                        span: var.span,
                        file_id,
                        doc: None,
                    });
                }
            }
            ast::Item::Property(prop) => {
                let name = qualify(prop.name.text(source));
                let type_text = prop.type_expr.span.text(source).to_string();
                out.push(Symbol {
                    modifiers: DeclModifiers::default(),
                    name,
                    kind: SymbolKind::Variable {
                        type_name: type_text,
                    },
                    span: prop.span,
                    file_id,
                    doc: None,
                });
            }
            ast::Item::Error(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::{self, Span};
    use crate::parser::Parser;

    #[test]
    fn test_extract_symbols_from_enum() {
        let src = "enum WheelType { FL, FR, RL, RR }";
        let tokens = lexer::tokenize_filtered(src);
        let mut parser = Parser::new(&tokens, src);
        let file = parser.parse_file();
        let symbols = SymbolTable::extract_symbols(0, src, &file);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"WheelType"));
        assert!(names.contains(&"WheelType::FL"));
    }

    #[test]
    fn test_extract_symbols_from_namespace() {
        let src = r#"namespace AgentSettings {
    string S_Provider = "minimax";
}"#;
        let tokens = lexer::tokenize_filtered(src);
        let mut parser = Parser::new(&tokens, src);
        let file = parser.parse_file();
        let symbols = SymbolTable::extract_symbols(0, src, &file);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"AgentSettings"));
        assert!(names.contains(&"AgentSettings::S_Provider"));
    }

    #[test]
    fn test_extract_class_methods_and_fields() {
        let src = "class Foo { int x; void bar() {} }";
        let tokens = lexer::tokenize_filtered(src);
        let mut parser = Parser::new(&tokens, src);
        let file = parser.parse_file();
        let symbols = SymbolTable::extract_symbols(0, src, &file);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Foo"), "missing Foo in {:?}", names);
        assert!(names.contains(&"Foo::x"), "missing Foo::x in {:?}", names);
        assert!(
            names.contains(&"Foo::bar"),
            "missing Foo::bar in {:?}",
            names
        );
    }

    #[test]
    fn test_extract_class_in_namespace() {
        let src = "namespace Ns { class Foo { int x; } }";
        let tokens = lexer::tokenize_filtered(src);
        let mut parser = Parser::new(&tokens, src);
        let file = parser.parse_file();
        let symbols = SymbolTable::extract_symbols(0, src, &file);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Ns::Foo"), "missing Ns::Foo in {:?}", names);
        assert!(
            names.contains(&"Ns::Foo::x"),
            "missing Ns::Foo::x in {:?}",
            names
        );
    }

    /// B054: interface members must reach the pool the same way class
    /// members do. The arm used to push one `Interface` symbol and never
    /// descend, so no `I::method` symbol existed and every pool consumer
    /// (hover, goto-definition, workspace symbols, rename) was blind to
    /// interface members.
    #[test]
    fn interface_methods_enter_the_symbol_pool() {
        let src = "interface I { void Go(); int Count(const string &in key); }";
        let symbols = extract(src);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"I"), "missing I in {:?}", names);
        assert!(names.contains(&"I::Go"), "missing I::Go in {:?}", names);
        assert!(
            names.contains(&"I::Count"),
            "missing I::Count in {:?}",
            names
        );
    }

    /// The method symbols must carry the same payload a class method does
    /// — return type, parameter list (passing modes included) and
    /// `min_args` — or signature help and arity checking see a skeleton.
    #[test]
    fn interface_method_symbols_carry_their_signature() {
        let src = "interface I { int Count(const string &in key, bool deep = false); }";
        let symbols = extract(src);
        let sym = symbols
            .iter()
            .find(|s| s.name == "I::Count")
            .expect("I::Count must be pooled");
        match &sym.kind {
            SymbolKind::Function {
                return_type,
                params,
                min_args,
                imported,
            } => {
                assert_eq!(return_type, "int");
                assert_eq!(
                    params,
                    &vec![
                        ("key".to_string(), "const string &in".to_string()),
                        ("deep".to_string(), "bool".to_string()),
                    ]
                );
                assert_eq!(*min_args, 1, "the defaulted parameter is optional");
                assert!(!imported);
                assert!(
                    !sym.modifiers.is_const,
                    "no trailing `const` was written"
                );
            }
            other => panic!("I::Count must be a Function symbol, got {:?}", other),
        }
    }

    // ── B111: the `mixin` modifier on a pooled class ─────────────────

    /// `is_mixin` of the pooled class symbol named `name`.
    fn pooled_is_mixin(src: &str, name: &str) -> bool {
        let symbols = extract(src);
        let sym = symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("{name} must be pooled in `{src}`"));
        match &sym.kind {
            SymbolKind::Class { .. } => sym.modifiers.is_mixin,
            other => panic!("{name} must be a Class symbol, got {other:?}"),
        }
    }

    /// The modifier the parser records must SURVIVE POOLING — B101 was
    /// exactly the case where it did not, so this is asserted directly
    /// rather than only through the consumer.
    #[test]
    fn a_mixin_class_is_pooled_as_a_mixin() {
        assert!(pooled_is_mixin("mixin class X {}", "X"));
        assert!(!pooled_is_mixin("class X {}", "X"));
    }

    /// Every spelling the modifier can appear in, and the neighbours it can
    /// be written beside — `shared` and `abstract` are eaten by the same
    /// modifier loop and are exactly what could swallow it.
    #[test]
    fn the_mixin_modifier_survives_every_spelling() {
        for (src, want) in [
            ("mixin class X {}", true),
            ("shared mixin class X {}", true),
            ("mixin abstract class X {}", true),
            ("shared mixin abstract class X {}", true),
            ("interface I {}\nmixin class X : I {}", true),
            ("class B {}\ninterface I {}\nmixin class X : B, I {}", true),
            ("class X {}", false),
            ("shared class X {}", false),
            ("abstract class X {}", false),
            ("shared abstract class X {}", false),
        ] {
            assert_eq!(
                pooled_is_mixin(src, "X"),
                want,
                "wrong is_mixin for `{src}`"
            );
        }
    }

    /// Inside a namespace, where the class pools under its long name.
    #[test]
    fn a_namespaced_mixin_is_pooled_as_a_mixin() {
        assert!(pooled_is_mixin("namespace N { mixin class X {} }", "N::X"));
        assert!(!pooled_is_mixin("namespace N { class X {} }", "N::X"));
    }

    /// A mixin's MEMBERS are pooled exactly as any class's are — the
    /// modifier must not change what is stored under it, because the
    /// including class relies on those members to satisfy its interface.
    #[test]
    fn a_mixins_members_are_pooled_like_any_classs() {
        let symbols = extract("mixin class X { void M() {} int f; }");
        assert!(
            symbols.iter().any(|s| s.name == "X::M"),
            "a mixin's method must still be pooled"
        );
        assert!(
            symbols.iter().any(|s| s.name == "X::f"),
            "a mixin's field must still be pooled"
        );
    }

    /// An interface is never a mixin — the flag lives on `Class` only, so
    /// this pins that an interface does not accidentally get one.
    #[test]
    fn an_interface_is_not_a_class_symbol() {
        let symbols = extract("interface I {}");
        let sym = symbols.iter().find(|s| s.name == "I").unwrap();
        assert!(
            matches!(sym.kind, SymbolKind::Interface { .. }),
            "an interface must pool as Interface, not Class"
        );
    }

    // ── B101: the trailing `const` on a pooled method ────────────────

    /// `is_const` of the pooled symbol named `name`.
    fn pooled_is_const(src: &str, name: &str) -> bool {
        let symbols = extract(src);
        let sym = symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("{name} must be pooled in `{src}`"));
        assert!(
            matches!(sym.kind, SymbolKind::Function { .. }),
            "{name} must be a Function symbol, got {:?}",
            sym.kind
        );
        sym.modifiers.is_const
    }

    /// A class method's trailing `const` survives into the pool, and its
    /// absence does too — the two must be TELLABLE APART, which is the
    /// whole of B101.
    #[test]
    fn a_class_methods_trailing_const_is_pooled() {
        assert!(pooled_is_const("class C { void M() const {} }", "C::M"));
        assert!(!pooled_is_const("class C { void M() {} }", "C::M"));
    }

    /// An interface method's `const` is a REQUIREMENT and must be pooled
    /// the same way.
    #[test]
    fn an_interface_methods_trailing_const_is_pooled() {
        assert!(pooled_is_const("interface I { void M() const; }", "I::M"));
        assert!(!pooled_is_const("interface I { void M(); }", "I::M"));
    }

    /// Constness travels with every parameter shape, per CLAUDE.md — it is
    /// parsed after the parameter list, so a `&out` or a defaulted
    /// argument is exactly the kind of thing that could eat it.
    #[test]
    fn constness_survives_every_parameter_shape() {
        for params in [
            "",
            "int a",
            "const string &in s",
            "int &out a",
            "int &inout a",
            "string &s",
            "C@ h",
            "const C@ h",
            "int a = 1",
            "const string &in s, int &out b, C@ h",
        ] {
            let with = format!("class C {{ void M({params}) const {{}} }}");
            let without = format!("class C {{ void M({params}) {{}} }}");
            assert!(pooled_is_const(&with, "C::M"), "const lost for `{params}`");
            assert!(
                !pooled_is_const(&without, "C::M"),
                "const invented for `{params}`"
            );
        }
    }

    /// `const` may be written in any order beside `override` and `final`,
    /// and the parser accepts any sequence — the pool must not care either.
    #[test]
    fn constness_survives_beside_override_and_final() {
        for tail in [
            "const",
            "const override",
            "override const",
            "const final",
            "final const",
            "const override final",
        ] {
            let src = format!("class C {{ void M() {tail} {{}} }}");
            assert!(pooled_is_const(&src, "C::M"), "const lost for `{tail}`");
        }
        for tail in ["", "override", "final", "override final"] {
            let src = format!("class C {{ void M() {tail} {{}} }}");
            assert!(!pooled_is_const(&src, "C::M"), "const invented for `{tail}`");
        }
    }

    /// A LEADING `const` is part of the RETURN TYPE, not the method
    /// qualifier — the two must not be confused.
    #[test]
    fn a_const_return_type_is_not_a_const_method() {
        assert!(
            !pooled_is_const("class C { const string M() { return \"\"; } }", "C::M"),
            "`const string M()` is a const RETURN, not a const method"
        );
        assert!(
            pooled_is_const("class C { const string M() const { return \"\"; } }", "C::M"),
            "both at once: the trailing one is the qualifier"
        );
    }

    /// A private or protected method keeps its constness.
    #[test]
    fn access_modifiers_do_not_lose_constness() {
        assert!(pooled_is_const("class C { private void M() const {} }", "C::M"));
        assert!(pooled_is_const(
            "class C { protected void M() const {} }",
            "C::M"
        ));
    }

    /// Constructors and destructors cannot be const and are pooled false.
    ///
    /// Both pool under `C::C` — the parser eats the `~` before taking the
    /// destructor's name Ident, so the two are indistinguishable by name
    /// in the pool (pre-existing, noted while writing this test). Assert
    /// over EVERY symbol of that name so neither is skipped.
    #[test]
    fn constructors_and_destructors_are_never_const() {
        let symbols = extract("class C { C() {} ~C() {} }");
        let ctor_dtor: Vec<&Symbol> = symbols.iter().filter(|s| s.name == "C::C").collect();
        assert_eq!(ctor_dtor.len(), 2, "constructor and destructor both pooled");
        for sym in ctor_dtor {
            match &sym.kind {
                SymbolKind::Function { .. } => {
                    assert!(!sym.modifiers.is_const, "a ctor/dtor can never be const")
                }
                other => panic!("expected a Function symbol, got {other:?}"),
            }
        }
    }

    /// A const getter and a non-const setter under the same class are
    /// pooled independently — one method's qualifier must not leak onto a
    /// sibling.
    #[test]
    fn sibling_methods_do_not_share_constness() {
        let src = "class C { int get_X() const { return 0; } void set_X(int v) {} }";
        assert!(pooled_is_const(src, "C::get_X"));
        assert!(!pooled_is_const(src, "C::set_X"));
    }

    /// A free function carries no qualifier, and an `import` alias is a
    /// free function too.
    #[test]
    fn free_functions_and_import_aliases_are_not_const() {
        assert!(!pooled_is_const("void F() {}", "F"));
        assert!(!pooled_is_const(
            "import void F() from \"Plugin\";",
            "F"
        ));
    }

    // ── B114: every declaration modifier reaches the pool ─────────────
    //
    // B101 and B111 each added ONE modifier, one ticket at a time, and each
    // was found by the false output it caused rather than by anyone noticing
    // a field was missing. B114 pools the rest at once. These assert the
    // pooled bit DIRECTLY rather than only through a consumer, which is what
    // B111 established: a consumer test passes for free if the consumer
    // never asks.

    /// The modifiers of the pooled symbol named `name`.
    fn pooled_modifiers(src: &str, name: &str) -> DeclModifiers {
        let symbols = extract(src);
        symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("{name} must be pooled in `{src}`"))
            .modifiers
    }

    /// `shared` is the modifier with the widest reach — five declaration
    /// kinds carry it, and B087 had to walk the AST for it because the pool
    /// dropped it from all five. Both directions per kind: lost and
    /// invented are equally wrong.
    #[test]
    fn the_shared_modifier_is_pooled_for_every_declaration_kind() {
        for (kind, shared, plain) in [
            ("class", "shared class X {}", "class X {}"),
            ("interface", "shared interface X {}", "interface X {}"),
            ("enum", "shared enum X { A }", "enum X { A }"),
            (
                "funcdef",
                "shared funcdef void X();",
                "funcdef void X();",
            ),
            ("function", "shared void X() {}", "void X() {}"),
        ] {
            assert!(
                pooled_modifiers(shared, "X").is_shared,
                "shared lost for a {kind}: `{shared}`"
            );
            assert!(
                !pooled_modifiers(plain, "X").is_shared,
                "shared invented for a {kind}: `{plain}`"
            );
        }
    }

    /// `shared` beside the neighbours the same modifier loop eats, and
    /// inside a namespace where the symbol pools under its long name.
    #[test]
    fn the_shared_modifier_survives_its_neighbours() {
        for (src, name) in [
            ("shared abstract class X {}", "X"),
            ("shared mixin class X {}", "X"),
            ("shared mixin abstract class X {}", "X"),
            ("namespace N { shared class X {} }", "N::X"),
            ("namespace N { shared void X() {} }", "N::X"),
            ("namespace N { shared enum X { A } }", "N::X"),
        ] {
            assert!(
                pooled_modifiers(src, name).is_shared,
                "shared lost for `{src}`"
            );
        }
    }

    /// `abstract`, on a class.
    ///
    /// Carried for UNIFORMITY only. B094 measured that `abstract` does NOT
    /// exempt a class from implementing the interface it names, and
    /// `an_abstract_class_is_not_exempt` pins that behaviour; pooling the
    /// bit does not license changing it.
    #[test]
    fn the_abstract_modifier_is_pooled() {
        for (src, want) in [
            ("abstract class X {}", true),
            ("shared abstract class X {}", true),
            ("mixin abstract class X {}", true),
            ("class X {}", false),
            ("shared class X {}", false),
            ("mixin class X {}", false),
        ] {
            assert_eq!(
                pooled_modifiers(src, "X").is_abstract,
                want,
                "wrong is_abstract for `{src}`"
            );
        }
    }

    /// The four method modifiers, each on its own, both directions.
    #[test]
    fn every_method_modifier_is_pooled() {
        for (name, src, read) in [
            (
                "override",
                "class C { void M() override {} }",
                (|m: DeclModifiers| m.is_override) as fn(DeclModifiers) -> bool,
            ),
            ("final", "class C { void M() final {} }", |m| m.is_final),
            (
                "private",
                "class C { private void M() {} }",
                |m| m.is_private,
            ),
            (
                "protected",
                "class C { protected void M() {} }",
                |m| m.is_protected,
            ),
        ] {
            assert!(read(pooled_modifiers(src, "C::M")), "{name} lost: `{src}`");
            assert!(
                !read(pooled_modifiers("class C { void M() {} }", "C::M")),
                "{name} invented for a plain method"
            );
        }
    }

    /// The modifiers are INDEPENDENT: one written must not set another, and
    /// several written together must all survive. `const`, `override` and
    /// `final` share one trailing modifier loop, which is exactly what could
    /// let one swallow the next.
    #[test]
    fn method_modifiers_do_not_leak_into_each_other() {
        let m = pooled_modifiers("class C { private void M() const override final {} }", "C::M");
        assert!(m.is_private, "private lost");
        assert!(m.is_const, "const lost");
        assert!(m.is_override, "override lost");
        assert!(m.is_final, "final lost");
        assert!(!m.is_protected, "protected invented");
        assert!(!m.is_shared, "shared invented");
        assert!(!m.is_mixin && !m.is_abstract, "a method is not a class");

        let plain = pooled_modifiers("class C { void M() {} }", "C::M");
        assert_eq!(plain, DeclModifiers::default(), "a plain method declares nothing");
    }

    /// `private` and `protected` are mutually exclusive in the parser — it
    /// only looks for `protected` when `private` was absent — so pin that
    /// each excludes the other rather than both landing true.
    #[test]
    fn private_and_protected_are_exclusive() {
        let private = pooled_modifiers("class C { private void M() {} }", "C::M");
        assert!(private.is_private && !private.is_protected);
        let protected = pooled_modifiers("class C { protected void M() {} }", "C::M");
        assert!(protected.is_protected && !protected.is_private);
    }

    /// An interface method carries the same modifiers a class method does.
    #[test]
    fn interface_method_modifiers_are_pooled() {
        assert!(pooled_modifiers("interface I { void M() const; }", "I::M").is_const);
        assert_eq!(
            pooled_modifiers("interface I { void M(); }", "I::M"),
            DeclModifiers::default(),
            "a plain interface method declares nothing"
        );
    }

    /// A visibility modifier written before a CONSTRUCTOR or DESTRUCTOR
    /// reaches the pool too — the parser eats it before it knows which
    /// member kind follows, and threads it into the synthesized
    /// `FunctionDecl`.
    #[test]
    fn constructor_and_destructor_visibility_is_pooled() {
        let symbols = extract("class C { private C() {} }");
        let ctor = symbols.iter().find(|s| s.name == "C::C").unwrap();
        assert!(ctor.modifiers.is_private, "a private constructor is private");
        assert!(!ctor.modifiers.is_const, "a constructor is never const");
    }

    /// A symbol nothing declared — an `import` alias — carries no modifiers
    /// at all. All-false here is the honest answer, not a dropped one.
    #[test]
    fn an_import_alias_declares_no_modifiers() {
        assert_eq!(
            pooled_modifiers("import void F() from \"Plugin\";", "F"),
            DeclModifiers::default()
        );
    }

    /// THE POINT OF THE TICKET: a modifier survives for a symbol declared in
    /// a DIFFERENT file from the consumer asking about it.
    ///
    /// This is what no AST sidecar could ever answer — a sidecar is built
    /// per document, while the pool is the cross-file surface — and it is
    /// the property every sidecar retirement rests on.
    #[test]
    fn pooled_modifiers_survive_across_a_file_boundary() {
        let mut table = SymbolTable::new();
        for src in [
            "shared class Far { void M() const {} }",
            "void Near() { }",
        ] {
            let fid = table.allocate_file_id();
            let tokens = lexer::tokenize_filtered(src);
            let mut parser = Parser::new(&tokens, src);
            let file = parser.parse_file();
            table.set_file_symbols(fid, SymbolTable::extract_symbols(fid, src, &file));
        }
        let far = table.lookup("Far");
        assert_eq!(far.len(), 1, "the far class must be pooled once");
        assert!(
            far[0].modifiers.is_shared,
            "`shared` must survive for a class declared in another file"
        );
        let method = table.lookup("Far::M");
        assert_eq!(method.len(), 1, "the far method must be pooled once");
        assert!(
            method[0].modifiers.is_const,
            "`const` must survive for a method declared in another file"
        );
    }

    /// RECORDED, NOT FIXED — a gap one layer EARLIER than this ticket.
    ///
    /// Visibility on a class FIELD is dropped by the PARSER, before pooling
    /// can see it: `parse_class_member` eats `private` / `protected` and
    /// threads them into every `FunctionDecl` it builds, but
    /// `ClassMember::Field` holds a `VarDeclStmt`, which has no field to put
    /// them in. So the pooled answer for a private field is `false` — known
    /// wrong, asserted here so it reads as a recorded gap rather than as
    /// intent, and so the parser ticket that closes it has a test to flip.
    ///
    /// Consequence for the feature this ticket unblocks: completion
    /// visibility filtering will be correct for METHODS and half-blind for
    /// FIELDS until that lands.
    #[test]
    fn a_private_field_loses_its_visibility_at_parse_time() {
        let m = pooled_modifiers("class C { private int x; }", "C::x");
        assert!(
            !m.is_private,
            "KNOWN GAP: if this now passes the visibility, the parser was \
             fixed — delete this test and pin the true value instead"
        );
        // The METHOD beside it, in the same class, does keep its visibility
        // — so this is a field-shaped hole, not a broken modifier.
        assert!(
            pooled_modifiers("class C { private void M() {} }", "C::M").is_private,
            "a private METHOD must keep its visibility"
        );
    }

    /// Parameter-mode variants, per CLAUDE.md: `&in` / `&out` / `&inout`,
    /// a bare reference, a handle and a const handle must all survive into
    /// the stored type text exactly as a class method's would.
    #[test]
    fn interface_method_parameter_modes_survive() {
        let src = "interface I { void Go(const string &in a, int &out b, float &inout c, Thing@ d, const Thing@ e, string &f); }";
        let symbols = extract(src);
        let sym = symbols.iter().find(|s| s.name == "I::Go").unwrap();
        let SymbolKind::Function { params, .. } = &sym.kind else {
            panic!("expected a function symbol");
        };
        let types: Vec<&str> = params.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            types,
            vec![
                "const string &in",
                "int &out",
                "float &inout",
                "Thing@",
                "const Thing@",
                "string &",
            ]
        );
    }

    /// A namespaced interface qualifies its members the same way a
    /// namespaced class does.
    #[test]
    fn interface_methods_in_a_namespace_are_qualified() {
        let src = "namespace Ns { interface I { void Go(); } }";
        let names: Vec<String> = extract(src).iter().map(|s| s.name.clone()).collect();
        assert!(names.contains(&"Ns::I".to_string()), "got {:?}", names);
        assert!(names.contains(&"Ns::I::Go".to_string()), "got {:?}", names);
    }

    /// Interface inheritance is real in AngelScript (`interface B : A`).
    /// The interface symbol records its bases as `parents`, mirroring
    /// `Class::parents`, so the inheritance walkers can cross an
    /// interface boundary.
    #[test]
    fn interface_bases_are_recorded_as_parents() {
        let src = "interface A { void One(); }\ninterface B : A { void Two(); }\n";
        let symbols = extract(src);
        let b = symbols.iter().find(|s| s.name == "B").unwrap();
        match &b.kind {
            SymbolKind::Interface { parents } => assert_eq!(parents, &vec!["A".to_string()]),
            other => panic!("B must be an Interface symbol, got {:?}", other),
        }
        let a = symbols.iter().find(|s| s.name == "A").unwrap();
        match &a.kind {
            SymbolKind::Interface { parents } => assert!(parents.is_empty()),
            other => panic!("A must be an Interface symbol, got {:?}", other),
        }
    }

    /// Two interfaces declaring the same method name each get their own
    /// qualified symbol — the pool must not collapse them.
    #[test]
    fn two_interfaces_with_the_same_method_name_stay_distinct() {
        let src = "interface A { void Go(); }\ninterface B { void Go(); }\n";
        let symbols = extract(src);
        let hits: Vec<&str> = symbols
            .iter()
            .map(|s| s.name.as_str())
            .filter(|n| n.ends_with("::Go"))
            .collect();
        assert_eq!(hits, vec!["A::Go", "B::Go"]);
    }

    fn extract(src: &str) -> Vec<Symbol> {
        let tokens = lexer::tokenize_filtered(src);
        let mut parser = Parser::new(&tokens, src);
        let file = parser.parse_file();
        SymbolTable::extract_symbols(0, src, &file)
    }

    #[test]
    fn test_symbol_table_lookup() {
        let mut table = SymbolTable::new();
        let fid = table.allocate_file_id();
        table.set_file_symbols(
            fid,
            vec![Symbol {
                modifiers: DeclModifiers::default(),
                name: "Main".to_string(),
                kind: SymbolKind::Function {
                    return_type: "void".into(),
                    params: vec![],
                    min_args: 0,
                    imported: false,
                },
                span: Span::new(0, 4),
                file_id: fid,
                doc: None,
            }],
        );
        let results = table.lookup("Main");
        assert_eq!(results.len(), 1);
    }

    /// B027: the symbol table must preserve parameter passing modes.
    /// Function/method params keep the mode in the stored type text (the
    /// parser's `Reference` span covers `&out`/`&in`/`&inout`; consumers
    /// strip the decoration via `TypeRepr::parse_type_string`), and funcdef
    /// symbols carry their full signature — `lookup_funcdef` previously saw
    /// only an empty skeleton, which disabled the whole ctor arg check for
    /// workspace funcdefs.
    #[test]
    fn extract_symbols_funcdef_signature_and_param_modes() {
        let src = "funcdef void FDO(int &out);\n\
                   class C {\n\
                   \x20   void MOut(int &out x) {}\n\
                   \x20   void MIn(const int &in y) {}\n\
                   \x20   void MPlain(int z) {}\n\
                   }\n";
        let tokens = lexer::tokenize_filtered(src);
        let mut parser = Parser::new(&tokens, src);
        let file = parser.parse_file();
        let symbols = SymbolTable::extract_symbols(0, src, &file);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        let find = |name: &str| -> &Symbol {
            symbols
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("missing {name} in {names:?}"))
        };
        match &find("FDO").kind {
            SymbolKind::Funcdef {
                return_type,
                params,
                is_typedef: _,
            } => {
                assert_eq!(return_type, "void");
                assert_eq!(params, &[("".to_string(), "int &out".to_string())]);
            }
            other => panic!("expected Funcdef, got {other:?}"),
        }
        let params_of = |name: &str| match &find(name).kind {
            SymbolKind::Function { params, .. } => params.clone(),
            other => panic!("expected Function, got {other:?}"),
        };
        assert_eq!(
            params_of("C::MOut"),
            vec![("x".to_string(), "int &out".to_string())]
        );
        assert_eq!(
            params_of("C::MIn"),
            vec![("y".to_string(), "const int &in".to_string())]
        );
        assert_eq!(
            params_of("C::MPlain"),
            vec![("z".to_string(), "int".to_string())]
        );
    }

    /// B033: the stored parameter type text must round-trip the `const`
    /// qualifier on a handle. `const Foo@` parses to `Handle(Const(Foo))`
    /// and the span of both nodes started at `Foo`, so everything that reads
    /// the type back as source text saw a bare `Foo@` and hover dropped the
    /// `const`. Every other decoration is pinned alongside it.
    #[test]
    fn extract_symbols_param_type_text_keeps_every_decoration() {
        let cases = [
            ("const C@ p", "const C@"),
            ("const C@ &in p", "const C@ &in"),
            ("C@ const p", "C@ const"),
            ("C@ p", "C@"),
            ("C@ &in p", "C@ &in"),
            ("const string &in p", "const string &in"),
            ("const string p", "const string"),
            ("int &out p", "int &out"),
            ("int &inout p", "int &inout"),
            ("int p", "int"),
            ("const int[] &in p", "const int[] &in"),
            ("array<C@>@ p", "array<C@>@"),
        ];
        for (decl, expected) in cases {
            let src = format!("class C {{}}\nvoid f({}) {{}}\n", decl);
            let tokens = lexer::tokenize_filtered(&src);
            let mut parser = Parser::new(&tokens, &src);
            let file = parser.parse_file();
            let symbols = SymbolTable::extract_symbols(0, &src, &file);
            let params = symbols
                .iter()
                .find(|s| s.name == "f")
                .map(|s| match &s.kind {
                    SymbolKind::Function { params, .. } => params.clone(),
                    other => panic!("expected Function, got {other:?}"),
                })
                .expect("missing f");
            assert_eq!(
                params,
                vec![("p".to_string(), expected.to_string())],
                "param {decl:?} lost its decoration"
            );
        }
    }

    /// B033: enum members carry their numeric value so hover can show it —
    /// the workspace path used to render bare names while the type-database
    /// path rendered `Name = 1`.
    #[test]
    fn extract_symbols_enum_values_are_evaluated() {
        let src = "enum E { A, B, C = 10, D, E2 = 0x1F, F = -3, G = A + 1, H }\n";
        let tokens = lexer::tokenize_filtered(src);
        let mut parser = Parser::new(&tokens, src);
        let file = parser.parse_file();
        let symbols = SymbolTable::extract_symbols(0, src, &file);
        let values = symbols
            .iter()
            .find(|s| s.name == "E")
            .map(|s| match &s.kind {
                SymbolKind::Enum { values } => values.clone(),
                other => panic!("expected Enum, got {other:?}"),
            })
            .expect("missing E");
        assert_eq!(
            values,
            vec![
                ("A".to_string(), Some(0)),
                ("B".to_string(), Some(1)),
                ("C".to_string(), Some(10)),
                ("D".to_string(), Some(11)),
                ("E2".to_string(), Some(31)),
                ("F".to_string(), Some(-3)),
                // `A + 1` is not a literal — unknown, and it breaks the
                // implicit run for everything after it.
                ("G".to_string(), None),
                ("H".to_string(), None),
            ]
        );
        // The per-member `EnumValue` symbols agree with the enum's list.
        let value_of = |name: &str| match &symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("missing {name}"))
            .kind
        {
            SymbolKind::EnumValue { value, .. } => *value,
            other => panic!("expected EnumValue, got {other:?}"),
        };
        assert_eq!(value_of("E::A"), Some(0));
        assert_eq!(value_of("E::F"), Some(-3));
        assert_eq!(value_of("E::G"), None);
    }
}
