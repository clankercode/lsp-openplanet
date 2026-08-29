use std::collections::HashMap;

use super::scope::{Symbol, SymbolKind};
use crate::parser::ast;

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
                    name: class_name.clone(),
                    kind: SymbolKind::Class {
                        parents,
                        members: Vec::new(),
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
                                name: mname,
                                kind: SymbolKind::Function {
                                    return_type,
                                    params,
                                    min_args,
                                    imported: false,
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
                                name: mname,
                                kind: SymbolKind::Function {
                                    return_type: simple_class.clone(),
                                    params,
                                    min_args,
                                    imported: false,
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
                                name: mname,
                                kind: SymbolKind::Function {
                                    return_type: String::new(),
                                    params,
                                    min_args,
                                    imported: false,
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
                let name = qualify(iface.name.text(source));
                out.push(Symbol {
                    name,
                    kind: SymbolKind::Interface {
                        methods: Vec::new(),
                    },
                    span: iface.span,
                    file_id,
                    doc: None,
                });
            }
            ast::Item::Enum(en) => {
                let enum_name = qualify(en.name.text(source));
                let values = enum_values(source, en);
                out.push(Symbol {
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
                        name: qualified_name,
                        kind: SymbolKind::Function {
                            return_type: return_type.span.text(source).to_string(),
                            params,
                            min_args,
                            // Import-site declaration: an alias for a function
                            // defined elsewhere — navigation should prefer the
                            // real definition when one exists in the workspace.
                            imported: true,
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

    #[test]
    fn test_symbol_table_lookup() {
        let mut table = SymbolTable::new();
        let fid = table.allocate_file_id();
        table.set_file_symbols(
            fid,
            vec![Symbol {
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
