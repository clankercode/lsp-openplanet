//! Expression / statement walker that produces type diagnostics.
//!
//! This is the "first real" type-checker pass: it walks a `SourceFile`,
//! keeps a stack of lexical scopes for locals, runs a `TypeResolver` on
//! every declared `TypeExpr` (reporting unknown-type diagnostics), and
//! reports undefined identifiers encountered in expressions.
//!
//! YAGNI: no overload resolution, no implicit conversions, no
//! member-access lookup, no real expression type inference beyond
//! literals and identifier lookup. Those are later iterations.

use super::builtins;
use super::global_scope::{GlobalScope, OverloadSig};
use super::repr::{PrimitiveType, TypeRepr};
use super::resolver::TypeResolver;
use crate::lexer::Span;
use crate::parser::ast::*;

#[derive(Debug, Clone, PartialEq)]
pub enum TypeDiagnosticKind {
    UnknownType(String),
    UndefinedIdentifier(String),
    UndefinedMember {
        object_type: String,
        member: String,
    },
    MissingReturn {
        function_name: String,
    },
    ArgCountMismatch {
        function_name: String,
        expected_min: usize,
        expected_max: usize,
        got: usize,
    },
    InvalidAssignmentTarget,
    ReturnTypeMismatch {
        expected: String,
        got: String,
    },
    ArgTypeMismatch {
        function_name: String,
        param_index: usize,
        expected: String,
        got: String,
    },
    HandleValueMismatch {
        detail: String,
    },
    ConstViolation {
        detail: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeDiagnostic {
    pub span: Span,
    pub kind: TypeDiagnosticKind,
}

impl TypeDiagnostic {
    pub fn message(&self) -> String {
        match &self.kind {
            TypeDiagnosticKind::UnknownType(n) => format!("unknown type `{}`", n),
            TypeDiagnosticKind::UndefinedIdentifier(n) => format!("undefined identifier `{}`", n),
            TypeDiagnosticKind::UndefinedMember {
                object_type,
                member,
            } => format!("type `{}` has no member `{}`", object_type, member),
            TypeDiagnosticKind::MissingReturn { function_name } => {
                format!("function `{}` must return a value", function_name)
            }
            TypeDiagnosticKind::ArgCountMismatch {
                function_name,
                expected_min,
                expected_max,
                got,
            } => {
                if expected_min == expected_max {
                    format!(
                        "function `{}` expects {} args, got {}",
                        function_name, expected_min, got
                    )
                } else {
                    format!(
                        "function `{}` expects {}..={} args, got {}",
                        function_name, expected_min, expected_max, got
                    )
                }
            }
            TypeDiagnosticKind::InvalidAssignmentTarget => {
                "invalid left-hand side in assignment".to_string()
            }
            TypeDiagnosticKind::ReturnTypeMismatch { expected, got } => format!(
                "return type mismatch: function returns `{}`, got `{}`",
                expected, got
            ),
            TypeDiagnosticKind::ArgTypeMismatch {
                function_name,
                param_index,
                expected,
                got,
            } => format!(
                "argument {} of `{}`: expected `{}`, got `{}`",
                param_index + 1,
                function_name,
                expected,
                got
            ),
            TypeDiagnosticKind::HandleValueMismatch { detail } => {
                format!("handle/value mismatch: {}", detail)
            }
            TypeDiagnosticKind::ConstViolation { detail } => {
                format!("const violation: {}", detail)
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Local {
    name: String,
    ty: TypeRepr,
    #[allow(dead_code)]
    span: Span,
}

#[derive(Debug, Default)]
struct ScopeFrame {
    locals: Vec<Local>,
}

/// The in-scope class context for methods/fields. Built when the walker
/// descends into a class declaration and popped when it leaves.
#[derive(Debug, Clone)]
struct ClassCtx {
    name: String,
    /// (member_name, resolved_type) — one entry per declarator. For
    /// methods we store the return type as a reasonable approximation
    /// (used so `this.foo()` or bare `foo` in a method doesn't false-
    /// positive; the real call/return-type semantics come in a later
    /// iteration).
    members: Vec<(String, TypeRepr)>,
}

pub struct Checker<'a> {
    source: &'a str,
    scope: &'a GlobalScope<'a>,
    frames: Vec<ScopeFrame>,
    class_stack: Vec<ClassCtx>,
    namespace_stack: Vec<String>,
    /// Map of fully-qualified workspace class names declared in this file
    /// to `(parent_name, members)`. Used so implicit-this member lookups
    /// can walk the parent chain for cross-method resolution within the
    /// same file.
    file_classes: std::collections::HashMap<String, (Option<String>, Vec<(String, TypeRepr)>)>,
    return_type_stack: Vec<TypeRepr>,
    pub diagnostics: Vec<TypeDiagnostic>,
}

impl<'a> Checker<'a> {
    pub fn new(source: &'a str, scope: &'a GlobalScope<'a>) -> Self {
        Self {
            source,
            scope,
            frames: Vec::new(),
            class_stack: Vec::new(),
            namespace_stack: Vec::new(),
            file_classes: std::collections::HashMap::new(),
            return_type_stack: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    pub fn check_file(&mut self, file: &SourceFile) {
        // Build an in-file class index up front so `class_stack`
        // lookups can walk same-file parent chains for implicit-this
        // member resolution.
        self.index_file_classes(&file.items, None);
        for item in &file.items {
            self.check_item(item);
        }
    }

    fn index_file_classes(&mut self, items: &[Item], ns: Option<&str>) {
        for item in items {
            match item {
                Item::Class(cls) => {
                    let simple = cls.name.text(self.source).to_string();
                    let qual = match ns {
                        Some(n) => format!("{}::{}", n, simple),
                        None => simple.clone(),
                    };
                    let parent = cls.base_classes.first().map(|b| {
                        // Use a throwaway resolver purely for the display
                        // string so we get the same qualified form other
                        // lookups use — discard diagnostics.
                        let mut r = TypeResolver::new(self.scope, self.source)
                            .with_namespace_stack(
                                ns.map(|n| n.split("::").map(|s| s.to_string()).collect())
                                    .unwrap_or_default(),
                            );
                        let repr = r.resolve(b);
                        let _ = r.take_diagnostics();
                        match repr.unwrap_const().unwrap_handle() {
                            TypeRepr::Named(n) => n.clone(),
                            TypeRepr::Error(n) => n.clone(),
                            other => other.display(),
                        }
                    });
                    let mut members: Vec<(String, TypeRepr)> = Vec::new();
                    for m in &cls.members {
                        match m {
                            ClassMember::Field(var) => {
                                let ty = {
                                    let mut r = TypeResolver::new(self.scope, self.source)
                                        .with_namespace_stack(
                                            ns.map(|n| {
                                                n.split("::").map(|s| s.to_string()).collect()
                                            })
                                            .unwrap_or_default(),
                                        );
                                    let repr = r.resolve(&var.type_expr);
                                    let _ = r.take_diagnostics();
                                    repr
                                };
                                for d in &var.declarators {
                                    members.push((
                                        d.name.text(self.source).to_string(),
                                        ty.clone(),
                                    ));
                                }
                            }
                            ClassMember::Property(prop) => {
                                let ty = {
                                    let mut r = TypeResolver::new(self.scope, self.source)
                                        .with_namespace_stack(
                                            ns.map(|n| {
                                                n.split("::").map(|s| s.to_string()).collect()
                                            })
                                            .unwrap_or_default(),
                                        );
                                    let repr = r.resolve(&prop.type_expr);
                                    let _ = r.take_diagnostics();
                                    repr
                                };
                                members.push((
                                    prop.name.text(self.source).to_string(),
                                    ty,
                                ));
                            }
                            ClassMember::Method(func) => {
                                let ret = {
                                    let mut r = TypeResolver::new(self.scope, self.source)
                                        .with_namespace_stack(
                                            ns.map(|n| {
                                                n.split("::").map(|s| s.to_string()).collect()
                                            })
                                            .unwrap_or_default(),
                                        );
                                    let repr = r.resolve(&func.return_type);
                                    let _ = r.take_diagnostics();
                                    repr
                                };
                                members.push((
                                    func.name.text(self.source).to_string(),
                                    ret,
                                ));
                            }
                            _ => {}
                        }
                    }
                    self.file_classes.insert(qual, (parent, members));
                }
                Item::Namespace(n) => {
                    let sub_ns = match ns {
                        Some(prefix) => format!("{}::{}", prefix, n.name.text(self.source)),
                        None => n.name.text(self.source).to_string(),
                    };
                    self.index_file_classes(&n.items, Some(&sub_ns));
                }
                _ => {}
            }
        }
    }

    // ── Scope helpers ───────────────────────────────────────────────────────

    fn push_frame(&mut self) {
        self.frames.push(ScopeFrame::default());
    }

    fn pop_frame(&mut self) {
        self.frames.pop();
    }

    fn define_local(&mut self, name: String, ty: TypeRepr, span: Span) {
        if let Some(frame) = self.frames.last_mut() {
            frame.locals.push(Local { name, ty, span });
        }
    }

    fn lookup_local(&self, name: &str) -> Option<&Local> {
        for frame in self.frames.iter().rev() {
            for local in frame.locals.iter().rev() {
                if local.name == name {
                    return Some(local);
                }
            }
        }
        None
    }

    // ── Class context helpers ───────────────────────────────────────────────

    fn push_class(&mut self, ctx: ClassCtx) {
        self.class_stack.push(ctx);
    }

    fn pop_class(&mut self) {
        self.class_stack.pop();
    }

    fn current_class(&self) -> Option<&ClassCtx> {
        self.class_stack.last()
    }

    /// Walks the class stack innermost-first and returns the first member
    /// whose name matches. For nested-class methods (rare in AngelScript)
    /// the innermost class wins. Also walks the parent-class chain via
    /// the file-local class index.
    ///
    /// Also honors AngelScript's virtual-property convention: a reference
    /// to `foo` will match a member named `get_foo` or `set_foo`. This is
    /// critical for `this.windowOpen`-style accesses where the class only
    /// declares `get_windowOpen()` / `set_windowOpen(bool)`.
    fn lookup_class_member(&self, name: &str) -> Option<TypeRepr> {
        let getter = format!("get_{}", name);
        let setter = format!("set_{}", name);
        let matches = |mname: &str| -> bool {
            mname == name || mname == getter || mname == setter
        };
        for cls in self.class_stack.iter().rev() {
            for (mname, ty) in &cls.members {
                if matches(mname) {
                    return Some(ty.clone());
                }
            }
            // Walk the parent chain for this class via the file-local
            // index first — iter 24 relies on this shortcut so same-file
            // const-wrapped parent fields retain their `Const(_)` layer
            // (the workspace walker strips const via type-string parse).
            if let Some((parent, _)) = self.file_classes.get(&cls.name) {
                let mut current = parent.clone();
                let mut hops = 0usize;
                while let Some(pname) = current {
                    hops += 1;
                    if hops > 32 {
                        break;
                    }
                    if let Some((pp, pmembers)) = self.file_classes.get(&pname) {
                        for (mname, ty) in pmembers {
                            if matches(mname) {
                                return Some(ty.clone());
                            }
                        }
                        current = pp.clone();
                    } else {
                        // Parent is not in this file — ask the workspace
                        // walker to continue the chain. It has its own
                        // HashSet cycle guard so we don't loop.
                        if let Some(ty) = self.scope.workspace_class_member(&pname, name) {
                            return Some(ty);
                        }
                        break;
                    }
                }
            } else {
                // No file-local entry for the current class at all — the
                // class was declared in a sibling file (implicit-this
                // through a cross-file class). Fall through to the
                // workspace walker starting from the class itself.
                if let Some(ty) = self.scope.workspace_class_member(&cls.name, name) {
                    return Some(ty);
                }
            }
        }
        None
    }

    // ── Namespace context helpers ───────────────────────────────────────────

    /// Joined form of the current namespace stack (e.g. "Outer::Inner").
    #[allow(dead_code)]
    fn current_namespace_qualified(&self) -> Option<String> {
        if self.namespace_stack.is_empty() {
            None
        } else {
            Some(self.namespace_stack.join("::"))
        }
    }

    // ── Type resolution shim ────────────────────────────────────────────────

    fn resolve_type_expr(&mut self, ty: &TypeExpr) -> TypeRepr {
        let mut resolver = TypeResolver::new(self.scope, self.source)
            .with_namespace_stack(self.namespace_stack.clone());
        let repr = resolver.resolve(ty);
        for diag in resolver.take_diagnostics() {
            self.diagnostics.push(TypeDiagnostic {
                span: diag.span,
                kind: TypeDiagnosticKind::UnknownType(diag.unknown_name),
            });
        }
        repr
    }

    // ── Item walker ─────────────────────────────────────────────────────────

    fn check_item(&mut self, item: &Item) {
        match item {
            Item::Function(func) => self.check_function_decl(func, true),
            Item::Class(cls) => self.check_class_decl(cls),
            Item::Interface(iface) => self.check_interface_decl(iface),
            Item::Enum(_) => {
                // Enum values: underlying type is int; nothing to resolve
                // structurally right now.
            }
            Item::Namespace(ns) => {
                let ns_name = ns.name.text(self.source).to_string();
                self.namespace_stack.push(ns_name);
                for sub in &ns.items {
                    self.check_item(sub);
                }
                self.namespace_stack.pop();
            }
            Item::Funcdef(fd) => {
                let _ = self.resolve_type_expr(&fd.return_type);
                for p in &fd.params {
                    let _ = self.resolve_type_expr(&p.type_expr);
                }
            }
            Item::VarDecl(var) => self.check_var_decl_global(var),
            Item::Property(prop) => {
                let _ = self.resolve_type_expr(&prop.type_expr);
                if let Some(body) = &prop.getter {
                    self.push_frame();
                    self.check_function_body(body);
                    self.pop_frame();
                }
                if let Some((_, body)) = &prop.setter {
                    self.push_frame();
                    self.check_function_body(body);
                    self.pop_frame();
                }
            }
            Item::Import(_) | Item::Error(_) => {}
        }
    }

    fn check_class_decl(&mut self, cls: &ClassDecl) {
        for base in &cls.base_classes {
            let _ = self.resolve_type_expr(base);
        }

        // Build the class context up front so every method sees the full
        // set of sibling members (including ones declared after it).
        let class_name = cls.name.text(self.source).to_string();
        let mut members: Vec<(String, TypeRepr)> = Vec::new();
        for member in &cls.members {
            match member {
                ClassMember::Field(var) => {
                    // Resolve the type once per field block — but don't emit
                    // diagnostics here; those come when we actually visit
                    // the member below. Use a throwaway resolver that drops
                    // its diagnostics.
                    let ty = {
                        let mut resolver = TypeResolver::new(self.scope, self.source)
                            .with_namespace_stack(self.namespace_stack.clone());
                        let repr = resolver.resolve(&var.type_expr);
                        // Intentionally discard diagnostics; visiting the
                        // field via `check_class_member` will re-emit them.
                        let _ = resolver.take_diagnostics();
                        repr
                    };
                    for d in &var.declarators {
                        members.push((d.name.text(self.source).to_string(), ty.clone()));
                    }
                }
                ClassMember::Property(prop) => {
                    let ty = {
                        let mut resolver = TypeResolver::new(self.scope, self.source)
                            .with_namespace_stack(self.namespace_stack.clone());
                        let repr = resolver.resolve(&prop.type_expr);
                        let _ = resolver.take_diagnostics();
                        repr
                    };
                    members.push((prop.name.text(self.source).to_string(), ty));
                }
                ClassMember::Method(func) => {
                    let ret = {
                        let mut resolver = TypeResolver::new(self.scope, self.source)
                            .with_namespace_stack(self.namespace_stack.clone());
                        let repr = resolver.resolve(&func.return_type);
                        let _ = resolver.take_diagnostics();
                        repr
                    };
                    members.push((func.name.text(self.source).to_string(), ret));
                }
                ClassMember::Constructor(_) | ClassMember::Destructor(_) => {
                    // Not addressable by bare name inside the class body.
                }
            }
        }

        self.push_class(ClassCtx {
            name: class_name,
            members,
        });

        for member in &cls.members {
            self.check_class_member(member);
        }

        self.pop_class();
    }

    fn check_interface_decl(&mut self, iface: &InterfaceDecl) {
        for base in &iface.bases {
            let _ = self.resolve_type_expr(base);
        }
        for method in &iface.methods {
            // Interface methods have no body — nothing to enforce.
            self.check_function_decl(method, false);
        }
    }

    fn check_class_member(&mut self, member: &ClassMember) {
        match member {
            ClassMember::Field(var) => {
                // A field does not get scope-tracked as a local; just
                // resolve its declared type and check any initializer expr.
                let _ = self.resolve_type_expr(&var.type_expr);
                for d in &var.declarators {
                    if let Some(init) = &d.init {
                        let _ = self.expr_type(init);
                    }
                }
            }
            ClassMember::Method(f) => {
                self.check_function_decl(f, true);
            }
            ClassMember::Constructor(f) | ClassMember::Destructor(f) => {
                // Ctors / dtors implicitly return; don't enforce return value.
                self.check_function_decl(f, false);
            }
            ClassMember::Property(prop) => {
                let _ = self.resolve_type_expr(&prop.type_expr);
                if let Some(body) = &prop.getter {
                    self.push_frame();
                    self.check_function_body(body);
                    self.pop_frame();
                }
                if let Some((_, body)) = &prop.setter {
                    self.push_frame();
                    self.check_function_body(body);
                    self.pop_frame();
                }
            }
        }
    }

    fn check_function_decl(&mut self, func: &FunctionDecl, enforce_return: bool) {
        let ret_ty = self.resolve_type_expr(&func.return_type);
        self.return_type_stack.push(ret_ty.clone());
        self.push_frame();
        for p in &func.params {
            let ty = self.resolve_type_expr(&p.type_expr);
            if let Some(name) = &p.name {
                self.define_local(name.text(self.source).to_string(), ty, name.span);
            }
            if let Some(dv) = &p.default_value {
                let _ = self.expr_type(dv);
            }
        }
        if let Some(body) = &func.body {
            self.check_function_body(body);
            if enforce_return
                && !matches!(ret_ty, TypeRepr::Void)
                && !self.stmts_terminate(&body.stmts)
            {
                self.diagnostics.push(TypeDiagnostic {
                    span: func.name.span,
                    kind: TypeDiagnosticKind::MissingReturn {
                        function_name: func.name.text(self.source).to_string(),
                    },
                });
            }
        }
        self.pop_frame();
        self.return_type_stack.pop();
    }

    /// Conservative "does the last statement of this slice definitely
    /// return?" check. Used for the MissingReturn diagnostic.
    fn stmts_terminate(&self, stmts: &[Stmt]) -> bool {
        let Some(last) = stmts.last() else {
            return false;
        };
        self.stmt_terminates(last)
    }

    fn stmt_terminates(&self, stmt: &Stmt) -> bool {
        match &stmt.kind {
            StmtKind::Return(_) => true,
            StmtKind::Block(inner) => self.stmts_terminate(inner),
            StmtKind::If {
                then_branch,
                else_branch: Some(eb),
                ..
            } => self.stmt_terminates(then_branch) && self.stmt_terminates(eb),
            StmtKind::Switch { cases, .. } => {
                let has_default =
                    cases.iter().any(|c| matches!(c.label, SwitchLabel::Default));
                has_default
                    && cases
                        .iter()
                        .all(|c| self.stmts_terminate(&c.stmts))
            }
            _ => false,
        }
    }

    fn check_function_body(&mut self, body: &FunctionBody) {
        for stmt in &body.stmts {
            self.check_stmt(stmt);
        }
    }

    // ── Var decl (global / local split) ─────────────────────────────────────

    fn check_var_decl_global(&mut self, var: &VarDeclStmt) {
        let _ = self.resolve_type_expr(&var.type_expr);
        for d in &var.declarators {
            if let Some(init) = &d.init {
                let _ = self.expr_type(init);
            }
        }
    }

    fn check_var_decl_local(&mut self, var: &VarDeclStmt) {
        let is_auto = matches!(var.type_expr.kind, TypeExprKind::Auto);
        let declared_ty = self.resolve_type_expr(&var.type_expr);
        for d in &var.declarators {
            // For `auto`, the local's type comes from the initializer.
            let local_ty = if is_auto {
                match &d.init {
                    Some(init) => {
                        let inferred = self.expr_type(init);
                        if inferred.is_error() {
                            TypeRepr::Error(String::new())
                        } else {
                            inferred
                        }
                    }
                    None => TypeRepr::Error(String::new()),
                }
            } else {
                if let Some(init) = &d.init {
                    let _ = self.expr_type(init);
                }
                declared_ty.clone()
            };
            self.define_local(
                d.name.text(self.source).to_string(),
                local_ty,
                d.name.span,
            );
        }
    }

    // ── Statement walker ────────────────────────────────────────────────────

    fn check_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Expr(e) => {
                let _ = self.expr_type(e);
            }
            StmtKind::VarDecl(var) => self.check_var_decl_local(var),
            StmtKind::Block(stmts) => {
                self.push_frame();
                for s in stmts {
                    self.check_stmt(s);
                }
                self.pop_frame();
            }
            StmtKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let _ = self.expr_type(condition);
                self.push_frame();
                self.check_stmt(then_branch);
                self.pop_frame();
                if let Some(eb) = else_branch {
                    self.push_frame();
                    self.check_stmt(eb);
                    self.pop_frame();
                }
            }
            StmtKind::For {
                init,
                condition,
                step,
                body,
            } => {
                // For-loop init may declare a variable whose scope covers
                // the condition/step/body — push a frame around the whole
                // loop.
                self.push_frame();
                if let Some(init_stmt) = init {
                    self.check_stmt(init_stmt);
                }
                if let Some(c) = condition {
                    let _ = self.expr_type(c);
                }
                for s in step {
                    let _ = self.expr_type(s);
                }
                self.check_stmt(body);
                self.pop_frame();
            }
            StmtKind::While { condition, body } => {
                let _ = self.expr_type(condition);
                self.push_frame();
                self.check_stmt(body);
                self.pop_frame();
            }
            StmtKind::DoWhile { body, condition } => {
                self.push_frame();
                self.check_stmt(body);
                self.pop_frame();
                let _ = self.expr_type(condition);
            }
            StmtKind::Switch { expr, cases } => {
                let _ = self.expr_type(expr);
                for case in cases {
                    if let SwitchLabel::Case(e) = &case.label {
                        let _ = self.expr_type(e);
                    }
                    self.push_frame();
                    for s in &case.stmts {
                        self.check_stmt(s);
                    }
                    self.pop_frame();
                }
            }
            StmtKind::Return(Some(e)) => {
                let got_ty = self.expr_type(e);
                if let Some(expected) = self.return_type_stack.last() {
                    if let (TypeRepr::Primitive(exp_p), TypeRepr::Primitive(got_p)) =
                        (expected, &got_ty)
                    {
                        if !is_convertible(&got_ty, expected) {
                            self.diagnostics.push(TypeDiagnostic {
                                span: e.span,
                                kind: TypeDiagnosticKind::ReturnTypeMismatch {
                                    expected: exp_p.as_str().to_string(),
                                    got: got_p.as_str().to_string(),
                                },
                            });
                        }
                    }
                }
            }
            StmtKind::TryCatch {
                try_body,
                catch_body,
            } => {
                self.check_stmt(try_body);
                self.check_stmt(catch_body);
            }
            StmtKind::Return(None)
            | StmtKind::Break
            | StmtKind::Continue
            | StmtKind::Empty
            | StmtKind::Error => {}
        }
    }

    // ── Member / call helpers ───────────────────────────────────────────────

    /// If `ty` is (after unwrapping `Const`/`Handle`/`Array`) a named type
    /// or a generic base, return the base name suitable for type-index
    /// lookup. Otherwise return None.
    fn base_type_name(ty: &TypeRepr) -> Option<String> {
        let inner = ty.unwrap_const().unwrap_handle();
        match inner {
            TypeRepr::Named(n) => {
                // `auto` is a placeholder for "type inference needed";
                // treat it as unknown so member access stays silent.
                if n == "auto" {
                    None
                } else {
                    Some(n.clone())
                }
            }
            TypeRepr::Generic { base, .. } => Some(base.clone()),
            TypeRepr::Array(_) => Some("array".to_string()),
            _ => None,
        }
    }

    /// True if `ty` represents a "const receiver" — i.e. a value whose
    /// contents should be treated as immutable for field / index access.
    ///
    /// AngelScript distinguishes `const Foo@` (handle to const object) from
    /// `Foo@ const` (const handle, mutable object). Our parser collapses
    /// both shapes into `Const(Handle(Foo))`, so we can't fully honor the
    /// distinction — see iter 32 notes. In practice we treat ANY outer /
    /// through-handle `Const` wrapper as "contents are const", which
    /// matches `const Foo@` / `const array<T>@` use-cases in the wild and
    /// will over-fire on the semantically distinct `Foo@ const` form
    /// (which is rarer).
    fn receiver_is_const(ty: &TypeRepr) -> bool {
        // Outer layer Const (parser output for `const X@`).
        if matches!(ty, TypeRepr::Const(_)) {
            return true;
        }
        // Handle-peeled Const (semantically-correct `Handle(Const(X))`).
        if matches!(ty.unwrap_handle(), TypeRepr::Const(_)) {
            return true;
        }
        false
    }

    /// If `receiver_const` is true and `t` isn't already wrapped, wrap
    /// the field type in `Const(_)` so downstream assignment checks see
    /// the propagated const. Errors and already-const types pass through.
    fn apply_receiver_const(t: TypeRepr, receiver_const: bool) -> TypeRepr {
        if !receiver_const {
            return t;
        }
        if matches!(t, TypeRepr::Const(_) | TypeRepr::Error(_)) {
            return t;
        }
        TypeRepr::Const(Box::new(t))
    }

    /// Derive the type of `obj.member`, emitting an `UndefinedMember` if
    /// the lookup fails against a known (non-error) object type. When the
    /// object type is `Error(_)` we silently propagate `Error` so we don't
    /// double-report the same root cause.
    fn member_access_type(
        &mut self,
        obj_ty: &TypeRepr,
        member: &Ident,
        span: Span,
    ) -> TypeRepr {
        // Propagate error without re-reporting.
        if obj_ty.is_error() {
            return TypeRepr::Error(String::new());
        }
        let member_name = member.text(self.source).to_string();
        // A const receiver propagates `Const` into field access results
        // (iter 32). Method access is routed through `call_type::Member`
        // so this only affects field reads. We do NOT wrap the array /
        // dictionary special-case return types — those are primitive
        // rvalues (`uint` / `bool`) that aren't meaningful assignment
        // targets anyway, and keeping them unwrapped preserves iter 29
        // arg-type-check behaviour on `a.Length` etc.
        let receiver_const = Self::receiver_is_const(obj_ty);
        // Built-in generic array members. AngelScript exposes `Length`
        // / `length` as `uint`, `IsEmpty` as `bool`, and a handful of
        // mutating methods that return void. We special-case the
        // common accessors here so plugin code stops FP'ing on them.
        if obj_ty.is_array_like() {
            match member_name.as_str() {
                "Length" | "length" => {
                    return TypeRepr::Primitive(PrimitiveType::Uint);
                }
                "IsEmpty" | "isEmpty" => {
                    return TypeRepr::Primitive(PrimitiveType::Bool);
                }
                // Everything else on an array: stay silent rather than
                // firing UndefinedMember. Methods like Add / InsertLast
                // / SortAsc / Resize do exist but the checker doesn't
                // consume their return types anywhere, so `Error("")`
                // is the right placeholder.
                _ => return TypeRepr::Error(String::new()),
            }
        }
        // Dictionary is opaque: every member access is silently
        // accepted (no UndefinedMember) until we model its API.
        if obj_ty.is_dictionary_like() {
            return TypeRepr::Error(String::new());
        }
        let Some(type_name) = Self::base_type_name(obj_ty) else {
            // Primitive / Null / Void / Funcdef — not a class, no members.
            // Stay quiet for now (later iterations may add primitive .op
            // overloads etc.).
            return TypeRepr::Error(String::new());
        };
        // Same-file workspace classes: prefer the in-memory `file_classes`
        // index because it preserves full `TypeRepr` (including `Const`)
        // that `scope.lookup_member_type` strips to `Error("")` for
        // workspace hits. This is the const-wrapper preservation path
        // (iter 24) — do NOT reorder this below any other lookup.
        if let Some((_, members)) = self.file_classes.get(&type_name) {
            for (mname, t) in members {
                if mname == &member_name {
                    return Self::apply_receiver_const(t.clone(), receiver_const);
                }
            }
        }
        if let Some(t) = self.scope.lookup_member_type(&type_name, &member_name) {
            return Self::apply_receiver_const(t, receiver_const);
        }
        // Also try: if this is a workspace-local class, check its in-memory
        // ClassCtx members. Handles `this.foo` transitively via explicit
        // receiver with the correct class name.
        for cls in &self.class_stack {
            if cls.name == type_name {
                for (mname, t) in &cls.members {
                    if mname == &member_name {
                        return Self::apply_receiver_const(t.clone(), receiver_const);
                    }
                }
            }
        }
        // Cross-file inheritance walk: if `type_name` is a workspace class
        // declared in a sibling file, walk its parent chain via the
        // GlobalScope. Returns `Error("")` on a hit (silence sentinel) —
        // enough to suppress `UndefinedMember` without fabricating a
        // concrete type we don't actually know.
        if let Some(t) = self.scope.workspace_class_member(&type_name, &member_name) {
            return Self::apply_receiver_const(t, receiver_const);
        }
        // Workspace classes currently don't track parent chains or all
        // members across files, so emitting UndefinedMember against one
        // would be noisy false-positive territory. Only emit when the
        // type is a known external type (where we trust the method /
        // property list). Unknown names (e.g. tail-matched class field
        // identifiers fabricated into `Named(name)` by the Ident walker)
        // are also silenced — we can't trust the object type.
        if !self.scope.is_external_type(&type_name) {
            return TypeRepr::Error(String::new());
        }
        // Nadeo-sourced types have partial member metadata (return types
        // and many property types are empty because the Nadeo format uses
        // type IDs rather than type-name strings), so a failed lookup can
        // mean the DB is incomplete rather than the member is actually
        // missing. Suppress the diagnostic.
        if self.scope.is_nadeo_type(&type_name) {
            return TypeRepr::Error(String::new());
        }
        self.diagnostics.push(TypeDiagnostic {
            span,
            kind: TypeDiagnosticKind::UndefinedMember {
                object_type: type_name,
                member: member_name,
            },
        });
        TypeRepr::Error(String::new())
    }

    /// If `qualified_name` names a unique workspace free function, emit an
    /// `ArgCountMismatch` diagnostic when `got` is outside that function's
    /// `min..=max` parameter range. Overloaded names (2+ matches) are
    /// conservatively skipped — see `GlobalScope::lookup_function_signature`.
    /// `display_name` is the bare name shown in the diagnostic message.
    fn check_arg_count(
        &mut self,
        display_name: &str,
        qualified_name: &str,
        got: usize,
        span: Span,
    ) {
        let Some((min_args, max_args)) = self.scope.lookup_function_signature(qualified_name)
        else {
            return;
        };
        if got < min_args || got > max_args {
            self.diagnostics.push(TypeDiagnostic {
                span,
                kind: TypeDiagnosticKind::ArgCountMismatch {
                    function_name: display_name.to_string(),
                    expected_min: min_args,
                    expected_max: max_args,
                    got,
                },
            });
        }
    }

    /// Centralised dispatch for "the callee resolved to a workspace free
    /// function named `qualified`" — handles both the unique-overload case
    /// (single match: run existing arg-count + primitive arg-type checks)
    /// and the 2+-overload case (run `resolve_overload` and, on a unique
    /// winner, use its return type; on `NoMatch` / `Ambiguous`, silently
    /// fall back to the external return type `fallback_ret`).
    ///
    /// `fallback_ret` is whatever `lookup_function_return(qualified)` gave
    /// us — used verbatim for the unique-overload path (its data comes from
    /// that same lookup) and as a silent fallback for ambiguous / no-match.
    fn resolve_workspace_function_call(
        &mut self,
        display_name: &str,
        qualified: &str,
        args: &[Expr],
        callee_span: Span,
        fallback_ret: TypeRepr,
    ) -> TypeRepr {
        let overloads = self.scope.lookup_function_overloads(qualified);
        match overloads.len() {
            0 => {
                // Not a workspace function (external-only). Preserve old
                // silent walk behaviour.
                self.walk_args(args);
                fallback_ret
            }
            1 => {
                // Single-overload fast path: identical to iter 19/22
                // behaviour. Use the legacy helpers so existing tests keep
                // passing byte-for-byte.
                self.check_arg_count(display_name, qualified, args.len(), callee_span);
                if let Some(param_types) = self.scope.lookup_function_param_types(qualified) {
                    self.walk_args_and_check_types(display_name, args, &param_types);
                } else {
                    self.walk_args(args);
                }
                fallback_ret
            }
            _ => {
                // 2+ overloads: walk args once, run real resolution.
                let arg_tys: Vec<TypeRepr> =
                    args.iter().map(|a| self.expr_type(a)).collect();
                match resolve_overload(&overloads, &arg_tys) {
                    OverloadMatch::Unique(sig) => {
                        // A unique winner means every primitive arg either
                        // matched exactly or was convertible — no further
                        // ArgTypeMismatch emission needed. Parse the
                        // winner's return type.
                        TypeRepr::parse_type_string(&sig.return_type)
                    }
                    OverloadMatch::Ambiguous
                    | OverloadMatch::NoMatch
                    | OverloadMatch::NoOverloads => {
                        // Silent skip — matches iter 19/22 overloaded
                        // behaviour. Return the lookup fallback so downstream
                        // `.member` chains still see *some* type.
                        fallback_ret
                    }
                }
            }
        }
    }

    /// Walk each argument expression exactly once, typing them for side
    /// effects (diagnostics) and discarding the results. Used by call-site
    /// dispatch branches that don't need arg types.
    fn walk_args(&mut self, args: &[Expr]) {
        for a in args {
            let _ = self.expr_type(a);
        }
    }

    /// Walk each argument expression and, for primitive-typed args whose
    /// corresponding declared parameter type is also a primitive, emit an
    /// `ArgTypeMismatch` when they differ. Non-primitive arg types, unknown
    /// param types (non-primitive text), and error types are all silently
    /// skipped — this is deliberately conservative, mirroring
    /// `ReturnTypeMismatch`'s primitive-only strategy.
    ///
    /// Walks each arg exactly once so callers must NOT pre-walk.
    fn walk_args_and_check_types(
        &mut self,
        display_name: &str,
        args: &[Expr],
        param_types: &[String],
    ) {
        for (i, arg) in args.iter().enumerate() {
            let arg_ty = self.expr_type(arg);
            let Some(param_text) = param_types.get(i) else {
                continue;
            };
            let Some(param_p) = PrimitiveType::from_name(param_text.trim()) else {
                continue;
            };
            let param_ty = TypeRepr::Primitive(param_p);
            if matches!(arg_ty, TypeRepr::Primitive(_))
                && matches!(param_ty, TypeRepr::Primitive(_))
                && !is_convertible(&arg_ty, &param_ty)
            {
                let TypeRepr::Primitive(arg_p) = arg_ty else {
                    continue;
                };
                self.diagnostics.push(TypeDiagnostic {
                    span: arg.span,
                    kind: TypeDiagnosticKind::ArgTypeMismatch {
                        function_name: display_name.to_string(),
                        param_index: i,
                        expected: param_p.as_str().to_string(),
                        got: arg_p.as_str().to_string(),
                    },
                });
            }
        }
    }

    /// Derive the type of a call expression's result. Takes the raw `args`
    /// slice and is responsible for walking each arg expression exactly
    /// once via `expr_type`. Callers must NOT pre-walk `args`.
    fn call_type(&mut self, callee: &Expr, args: &[Expr]) -> TypeRepr {
        match &callee.kind {
            ExprKind::Ident(ident) => {
                let name = ident.text(self.source).to_string();
                // 1. Local (function-typed variable) — treat as unknown.
                if self.lookup_local(&name).is_some() {
                    self.walk_args(args);
                    return TypeRepr::Error(String::new());
                }
                // 2. Implicit `this.method()` — find on current class.
                if self.lookup_class_member(&name).is_some() {
                    self.walk_args(args);
                    return TypeRepr::Error(String::new());
                }
                // 3. Namespace-scoped lookups (inside a namespace block).
                //    Try function return type first (for a real typed
                //    return); fall back to any-kind qualified lookup so
                //    type constructors and other callables within the
                //    current namespace stay silent.
                for depth in (1..=self.namespace_stack.len()).rev() {
                    let ns = self.namespace_stack[..depth].join("::");
                    let qualified = format!("{}::{}", ns, name);
                    if let Some(t) = self.scope.lookup_function_return(&qualified) {
                        return self.resolve_workspace_function_call(
                            &name,
                            &qualified,
                            args,
                            callee.span,
                            t,
                        );
                    }
                    if self.scope.has_type(&qualified) {
                        self.walk_args(args);
                        return TypeRepr::Named(qualified);
                    }
                    if self.scope.has_global_ident(&qualified) {
                        self.walk_args(args);
                        return TypeRepr::Error(String::new());
                    }
                }
                // 4. Top-level function.
                if let Some(t) = self.scope.lookup_function_return(&name) {
                    return self.resolve_workspace_function_call(
                        &name,
                        &name,
                        args,
                        callee.span,
                        t,
                    );
                }
                // 5. Maybe it's a type-constructor form that slipped in
                //    as an Ident — surface the type when possible so
                //    chained `.member` access off a constructor can
                //    still resolve. Otherwise just silence.
                if self.scope.has_type(&name) {
                    self.walk_args(args);
                    return TypeRepr::Named(name);
                }
                if self.scope.has_global_ident(&name) {
                    self.walk_args(args);
                    return TypeRepr::Error(String::new());
                }
                // 6. AngelScript / Openplanet hardcoded builtins
                //    (e.g. `CoroutineFunc(X)` constructor).
                if builtins::is_builtin_type(&name) || builtins::is_builtin_global(&name) {
                    self.walk_args(args);
                    return TypeRepr::Error(String::new());
                }
                // Emit an undefined-ident diagnostic on the callee span.
                self.diagnostics.push(TypeDiagnostic {
                    span: callee.span,
                    kind: TypeDiagnosticKind::UndefinedIdentifier(name.clone()),
                });
                self.walk_args(args);
                TypeRepr::Error(name)
            }
            ExprKind::Member { object, member } => {
                let obj_ty = self.expr_type(object);
                self.walk_args(args);
                if obj_ty.is_error() {
                    return TypeRepr::Error(String::new());
                }
                let member_name = member.text(self.source).to_string();
                let Some(type_name) = Self::base_type_name(&obj_ty) else {
                    return TypeRepr::Error(String::new());
                };
                if let Some(t) = self
                    .scope
                    .lookup_method_return(&type_name, &member_name)
                {
                    return t;
                }
                // Workspace-local class: any member is fine — silence.
                for cls in &self.class_stack {
                    if cls.name == type_name
                        && cls.members.iter().any(|(n, _)| n == &member_name)
                    {
                        return TypeRepr::Error(String::new());
                    }
                }
                // Cross-file inherited method: walk the workspace class
                // hierarchy so an inherited method's real return type
                // (iter 28) flows into downstream arg-type checks.
                if let Some(t) = self
                    .scope
                    .workspace_class_member(&type_name, &member_name)
                {
                    return t;
                }
                if !self.scope.is_external_type(&type_name) {
                    return TypeRepr::Error(String::new());
                }
                if self.scope.is_nadeo_type(&type_name) {
                    return TypeRepr::Error(String::new());
                }
                self.diagnostics.push(TypeDiagnostic {
                    span: callee.span,
                    kind: TypeDiagnosticKind::UndefinedMember {
                        object_type: type_name,
                        member: member_name,
                    },
                });
                TypeRepr::Error(String::new())
            }
            ExprKind::NamespaceAccess { path } => {
                let qual = path.to_string(self.source);
                self.walk_args(args);
                if let Some(t) = self.scope.lookup_function_return(&qual) {
                    return t;
                }
                if self.scope.has_type(&qual) {
                    return TypeRepr::Named(qual);
                }
                if self.scope.has_global_ident(&qual) {
                    return TypeRepr::Error(String::new());
                }
                // Fully qualified call-like path (`UX::SmallButton(...)`) —
                // stay silent; we can't reliably distinguish user helper
                // namespaces from external APIs yet.
                TypeRepr::Error(String::new())
            }
            _ => {
                let _ = self.expr_type(callee);
                self.walk_args(args);
                TypeRepr::Error(String::new())
            }
        }
    }

    // ── Expression walker / minimal type derivation ─────────────────────────

    fn expr_type(&mut self, expr: &Expr) -> TypeRepr {
        match &expr.kind {
            ExprKind::IntLit(_) | ExprKind::HexLit(_) => TypeRepr::Primitive(PrimitiveType::Int),
            ExprKind::FloatLit(_) => TypeRepr::Primitive(PrimitiveType::Float),
            ExprKind::StringLit => TypeRepr::Primitive(PrimitiveType::String),
            ExprKind::BoolLit(_) => TypeRepr::Primitive(PrimitiveType::Bool),
            ExprKind::Null => TypeRepr::Null,
            ExprKind::This | ExprKind::Super => {
                if let Some(cls) = self.current_class() {
                    TypeRepr::Named(cls.name.clone())
                } else {
                    TypeRepr::Error("this".into())
                }
            }
            ExprKind::Ident(ident) => {
                let name = ident.text(self.source).to_string();
                if let Some(local) = self.lookup_local(&name) {
                    return local.ty.clone();
                }
                // 2. Class member (implicit `this.`).
                if let Some(ty) = self.lookup_class_member(&name) {
                    return ty;
                }
                // 3. Namespace-scoped global: try progressively shorter
                //    namespace prefixes.  Inside Ns "Outer::Inner", try
                //    "Outer::Inner::name" first, then "Outer::name".
                for depth in (1..=self.namespace_stack.len()).rev() {
                    let ns = self.namespace_stack[..depth].join("::");
                    let qualified = format!("{}::{}", ns, name);
                    if self.scope.has_global_ident(&qualified) {
                        return TypeRepr::Named(qualified);
                    }
                }
                // 4. Global top-level lookup.
                if self.scope.has_global_ident(&name) {
                    return TypeRepr::Named(name);
                }
                // 5. AngelScript / Openplanet hardcoded builtins — silent.
                if builtins::is_builtin_type(&name) || builtins::is_builtin_global(&name) {
                    return TypeRepr::Error(String::new());
                }
                // 6. Undefined.
                self.diagnostics.push(TypeDiagnostic {
                    span: expr.span,
                    kind: TypeDiagnosticKind::UndefinedIdentifier(name.clone()),
                });
                TypeRepr::Error(name)
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                let _ = self.expr_type(lhs);
                let _ = self.expr_type(rhs);
                TypeRepr::Error(String::new())
            }
            ExprKind::Unary { expr, .. } | ExprKind::Postfix { expr, .. } => self.expr_type(expr),
            ExprKind::Call { callee, args } => {
                // `call_type` is responsible for walking each `args` entry
                // exactly once via `expr_type`. Do NOT pre-walk here — the
                // Ident arm needs raw arg exprs to do arg-type checking
                // without double-emitting diagnostics.
                self.call_type(callee, args)
            }
            ExprKind::Member { object, member } => {
                let obj_ty = self.expr_type(object);
                self.member_access_type(&obj_ty, member, expr.span)
            }
            ExprKind::Index { object, index } => {
                let obj_ty = self.expr_type(object);
                let _ = self.expr_type(index);
                // `array<T>[i]` / `T[][i]` → element type. If the receiver
                // is (transitively) const — e.g. `const array<T>@` — wrap
                // the element type in `Const(_)` so downstream assignment
                // checks can fire `ConstViolation` (iter 32). Pure reads
                // of `Const(T)` still type-check fine because iter 24's
                // const check only fires on assignment LHS.
                if let Some(elem) = obj_ty.array_element_type() {
                    if Self::receiver_is_const(&obj_ty)
                        && !matches!(elem, TypeRepr::Const(_))
                    {
                        return TypeRepr::Const(Box::new(elem.clone()));
                    }
                    return elem.clone();
                }
                // Dictionary-like and everything else: stay silent.
                TypeRepr::Error(String::new())
            }
            ExprKind::Cast { target_type, expr: inner } => {
                let _ = self.resolve_type_expr(target_type);
                let _ = self.expr_type(inner);
                TypeRepr::Error(String::new())
            }
            ExprKind::TypeConstruct { target_type, args } => {
                let _ = self.resolve_type_expr(target_type);
                for a in args {
                    let _ = self.expr_type(a);
                }
                TypeRepr::Error(String::new())
            }
            ExprKind::ArrayInit(items) => {
                for i in items {
                    let _ = self.expr_type(i);
                }
                TypeRepr::Error(String::new())
            }
            ExprKind::Assign { lhs, rhs, .. } => {
                if !matches!(
                    lhs.kind,
                    ExprKind::Ident(_)
                        | ExprKind::Member { .. }
                        | ExprKind::Index { .. }
                        | ExprKind::NamespaceAccess { .. }
                ) {
                    self.diagnostics.push(TypeDiagnostic {
                        span: lhs.span,
                        kind: TypeDiagnosticKind::InvalidAssignmentTarget,
                    });
                }
                let lhs_ty = self.expr_type(lhs);
                let _ = self.expr_type(rhs);
                if matches!(lhs_ty, TypeRepr::Const(_)) {
                    self.diagnostics.push(TypeDiagnostic {
                        span: lhs.span,
                        kind: TypeDiagnosticKind::ConstViolation {
                            detail: "cannot assign to const value".to_string(),
                        },
                    });
                }
                TypeRepr::Error(String::new())
            }
            ExprKind::HandleAssign { lhs, rhs } => {
                if !matches!(
                    lhs.kind,
                    ExprKind::Ident(_)
                        | ExprKind::Member { .. }
                        | ExprKind::Index { .. }
                        | ExprKind::NamespaceAccess { .. }
                ) {
                    self.diagnostics.push(TypeDiagnostic {
                        span: lhs.span,
                        kind: TypeDiagnosticKind::InvalidAssignmentTarget,
                    });
                }
                let lhs_ty = self.expr_type(lhs);
                let rhs_ty = self.expr_type(rhs);
                // LHS check: only fire when clearly not handle-capable
                // (Primitive / Void). Named types are ambiguous — a bare
                // class name can be a handle slot in practice.
                if matches!(lhs_ty, TypeRepr::Primitive(_) | TypeRepr::Void) {
                    self.diagnostics.push(TypeDiagnostic {
                        span: lhs.span,
                        kind: TypeDiagnosticKind::HandleValueMismatch {
                            detail: "left-hand side of @= is not a handle type"
                                .to_string(),
                        },
                    });
                }
                // RHS check: only fire when clearly not handle/null.
                // Accept Handle, Null, Error, Named (ambiguous).
                if matches!(rhs_ty, TypeRepr::Primitive(_) | TypeRepr::Void) {
                    self.diagnostics.push(TypeDiagnostic {
                        span: rhs.span,
                        kind: TypeDiagnosticKind::HandleValueMismatch {
                            detail: "right-hand side of @= must be a handle or null"
                                .to_string(),
                        },
                    });
                }
                if matches!(lhs_ty, TypeRepr::Const(_)) {
                    self.diagnostics.push(TypeDiagnostic {
                        span: lhs.span,
                        kind: TypeDiagnosticKind::ConstViolation {
                            detail: "cannot assign to const value".to_string(),
                        },
                    });
                }
                TypeRepr::Error(String::new())
            }
            ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                let _ = self.expr_type(condition);
                let _ = self.expr_type(then_expr);
                let _ = self.expr_type(else_expr);
                TypeRepr::Error(String::new())
            }
            ExprKind::Is {
                expr: inner,
                target,
                ..
            } => {
                let _ = self.expr_type(inner);
                match target {
                    IsTarget::Type(t) => {
                        let _ = self.resolve_type_expr(t);
                    }
                    IsTarget::Expr(e) => {
                        let _ = self.expr_type(e);
                    }
                    IsTarget::Null => {}
                }
                TypeRepr::Primitive(PrimitiveType::Bool)
            }
            ExprKind::NamespaceAccess { path } => {
                let qual = path.to_string(self.source);
                if self.scope.has_type(&qual) {
                    TypeRepr::Named(qual)
                } else {
                    TypeRepr::Error(String::new())
                }
            }
            ExprKind::Lambda { params, body } => {
                // Push an Error sentinel onto the return-type stack so
                // the outer function's expected return type doesn't leak
                // into `return` statements inside the lambda body.
                self.return_type_stack.push(TypeRepr::Error(String::new()));
                self.push_frame();
                for p in params {
                    let ty = self.resolve_type_expr(&p.type_expr);
                    if let Some(name) = &p.name {
                        self.define_local(
                            name.text(self.source).to_string(),
                            ty,
                            name.span,
                        );
                    }
                }
                self.check_function_body(body);
                self.pop_frame();
                self.return_type_stack.pop();
                TypeRepr::Error(String::new())
            }
            ExprKind::Error => TypeRepr::Error(String::new()),
        }
    }
}

/// True if `p` is one of the numeric primitive families (signed / unsigned
/// integers or floating-point). Bool and string are deliberately excluded.
fn is_numeric_primitive(p: &PrimitiveType) -> bool {
    matches!(
        p,
        PrimitiveType::Int8
            | PrimitiveType::Int16
            | PrimitiveType::Int
            | PrimitiveType::Int64
            | PrimitiveType::Uint8
            | PrimitiveType::Uint16
            | PrimitiveType::Uint
            | PrimitiveType::Uint64
            | PrimitiveType::Float
            | PrimitiveType::Double
    )
}

/// Shallow implicit-conversion check used by arg and return type diagnostics.
///
/// Rules, evaluated in order:
/// 1. If either side is an `Error(_)`, return `true` so we don't stack a
///    type-mismatch on top of an unresolved name.
/// 2. `Null` converts to any `Handle(_)`.
/// 3. After stripping `Const` wrappers, structurally equal types convert.
/// 4. Numeric primitive widening/narrowing is allowed (both sides must be
///    numeric — bool and string are excluded).
/// 5. Otherwise, not convertible.
fn is_convertible(from: &TypeRepr, to: &TypeRepr) -> bool {
    if matches!(from, TypeRepr::Error(_)) || matches!(to, TypeRepr::Error(_)) {
        return true;
    }
    if matches!(from, TypeRepr::Null) && matches!(to, TypeRepr::Handle(_)) {
        return true;
    }
    let from_s = from.unwrap_const();
    let to_s = to.unwrap_const();
    if from_s == to_s {
        return true;
    }
    if let (TypeRepr::Primitive(fp), TypeRepr::Primitive(tp)) = (from_s, to_s) {
        if is_numeric_primitive(fp) && is_numeric_primitive(tp) {
            return true;
        }
        return false;
    }
    false
}

/// Result of `resolve_overload`. `Unique` is the only variant that lets the
/// caller use the winning overload's return type / param list. `NoOverloads`
/// means the name isn't a workspace free function at all — fall through.
/// `Ambiguous` and `NoMatch` both resolve to silent skip this iter.
enum OverloadMatch<'a> {
    Unique(&'a OverloadSig),
    Ambiguous,
    NoMatch,
    NoOverloads,
}

/// Pick the best workspace-function overload for a call site given its
/// already-computed argument types.
///
/// Scoring (per primitive arg/param pair):
/// - exact primitive match: +2
/// - convertible primitive: +1
/// - mismatched primitive: candidate is rejected entirely
/// - non-primitive on either side, or Error-typed arg: 0 (neutral)
///
/// A candidate must first pass arity (`arg_tys.len() ∈ [min_args, params]`).
fn resolve_overload<'a>(
    overloads: &'a [OverloadSig],
    arg_tys: &[TypeRepr],
) -> OverloadMatch<'a> {
    if overloads.is_empty() {
        return OverloadMatch::NoOverloads;
    }
    let mut scored: Vec<(&OverloadSig, i32)> = Vec::new();
    for sig in overloads {
        if arg_tys.len() < sig.min_args || arg_tys.len() > sig.param_types.len() {
            continue;
        }
        let mut score: i32 = 0;
        let mut rejected = false;
        for (arg_ty, param_text) in arg_tys.iter().zip(sig.param_types.iter()) {
            let Some(param_p) = PrimitiveType::from_name(param_text.trim()) else {
                continue;
            };
            if matches!(arg_ty, TypeRepr::Error(_)) {
                continue;
            }
            if let TypeRepr::Primitive(arg_p) = arg_ty {
                if *arg_p == param_p {
                    score += 2;
                } else if is_convertible(
                    &TypeRepr::Primitive(*arg_p),
                    &TypeRepr::Primitive(param_p),
                ) {
                    score += 1;
                } else {
                    rejected = true;
                    break;
                }
            }
            // Non-primitive arg: neutral.
        }
        if !rejected {
            scored.push((sig, score));
        }
    }
    if scored.is_empty() {
        return OverloadMatch::NoMatch;
    }
    if scored.len() == 1 {
        return OverloadMatch::Unique(scored[0].0);
    }
    let max = scored.iter().map(|(_, s)| *s).max().unwrap();
    let top: Vec<&OverloadSig> = scored
        .iter()
        .filter(|(_, s)| *s == max)
        .map(|(sig, _)| *sig)
        .collect();
    if top.len() == 1 {
        OverloadMatch::Unique(top[0])
    } else {
        OverloadMatch::Ambiguous
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize_filtered;
    use crate::parser::Parser;
    use crate::symbols::SymbolTable;

    fn check(source: &str) -> Vec<TypeDiagnostic> {
        let tokens = tokenize_filtered(source);
        let mut parser = Parser::new(&tokens, source);
        let file = parser.parse_file();
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        let syms = SymbolTable::extract_symbols(fid, source, &file);
        ws.set_file_symbols(fid, syms);
        let scope = GlobalScope::new(&ws, None);
        let mut checker = Checker::new(source, &scope);
        checker.check_file(&file);
        checker.diagnostics
    }

    #[test]
    fn unknown_type_in_vardecl() {
        let diags = check("NotAType x;");
        assert_eq!(diags.len(), 1, "expected 1 diagnostic, got {:?}", diags);
        assert_eq!(
            diags[0].kind,
            TypeDiagnosticKind::UnknownType("NotAType".into())
        );
    }

    #[test]
    fn undefined_ident_in_expr() {
        let diags = check("void f() { int x = y; }");
        assert_eq!(diags.len(), 1, "expected 1 diagnostic, got {:?}", diags);
        assert_eq!(
            diags[0].kind,
            TypeDiagnosticKind::UndefinedIdentifier("y".into())
        );
    }

    #[test]
    fn local_shadows_nothing() {
        let diags = check("void f() { int x = 5; int y = x; }");
        assert!(diags.is_empty(), "expected no diagnostics, got {:?}", diags);
    }

    #[test]
    fn function_param_is_local() {
        let diags = check("void f(int x) { int y = x; }");
        assert!(diags.is_empty(), "expected no diagnostics, got {:?}", diags);
    }

    #[test]
    fn class_member_unknown_type() {
        let diags = check("class C { NotAType field; }");
        assert_eq!(diags.len(), 1, "expected 1 diagnostic, got {:?}", diags);
        assert_eq!(
            diags[0].kind,
            TypeDiagnosticKind::UnknownType("NotAType".into())
        );
    }

    #[test]
    fn nested_block_scope() {
        let diags = check("void f() { if (true) { int x = 5; } int y = x; }");
        assert_eq!(diags.len(), 1, "expected 1 diagnostic, got {:?}", diags);
        assert_eq!(
            diags[0].kind,
            TypeDiagnosticKind::UndefinedIdentifier("x".into())
        );
    }

    #[test]
    fn namespace_items_are_checked() {
        let diags = check("namespace Foo { NotAType g; }");
        assert_eq!(diags.len(), 1, "expected 1 diagnostic, got {:?}", diags);
        assert_eq!(
            diags[0].kind,
            TypeDiagnosticKind::UnknownType("NotAType".into())
        );
    }

    #[test]
    fn this_resolves_to_class_type() {
        let diags = check("class C { void f() { C@ x = this; } }");
        assert!(diags.is_empty(), "expected no diagnostics, got {:?}", diags);
    }

    #[test]
    fn implicit_this_member_resolves() {
        let diags = check("class C { int x; void f() { int y = x; } }");
        assert!(diags.is_empty(), "expected no diagnostics, got {:?}", diags);
    }

    #[test]
    fn namespace_scoped_ident_resolves() {
        let diags =
            check("namespace Ns { class Foo {} void f() { Foo@ x; } }");
        assert!(diags.is_empty(), "expected no diagnostics, got {:?}", diags);
    }

    #[test]
    fn member_access_workspace_class_silenced() {
        // Using `this.field` inside a method resolves through
        // `base_type_name(this) → current class` + the in-memory
        // ClassCtx members, so `this.x` should not emit a diagnostic.
        let diags = check("class C { int x; void f() { int y = this.x; } }");
        assert!(
            diags.is_empty(),
            "expected no diagnostics, got {:?}",
            diags
        );
    }

    #[test]
    fn member_access_unknown_on_workspace_class_is_silent() {
        // Workspace classes don't carry full member / parent-chain
        // information across files yet, so a missing member on a
        // workspace class should NOT emit UndefinedMember — it would
        // produce pervasive false positives against real user code
        // that inherits from unresolved base classes. Emission is
        // reserved for external (typedb-backed) types.
        let diags = check("class C { int x; void f() { int y = this.bogus; } }");
        let undef_member: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { .. }))
            .collect();
        assert!(
            undef_member.is_empty(),
            "expected no UndefinedMember diag, got {:?}",
            diags
        );
    }

    #[test]
    fn member_access_on_error_is_silent() {
        // Calling `.foo` on an unknown-type value should only emit the
        // UnknownType diagnostic, not an UndefinedMember cascade.
        let diags = check("void f() { NotAType x; int y = x.foo; }");
        let undef_member: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { .. }))
            .collect();
        assert!(
            undef_member.is_empty(),
            "expected no UndefinedMember diag, got {:?}",
            diags
        );
    }

    #[test]
    fn coroutine_func_builtin_is_silent() {
        // `CoroutineFunc` is an engine-registered funcdef that isn't in
        // the loaded type DB. Treat it as a known builtin rather than
        // emitting undefined-ident.
        let diags = check("void worker() {} void f() { CoroutineFunc(worker); }");
        let undef: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedIdentifier(n) if n.starts_with("CoroutineFunc")))
            .collect();
        assert!(
            undef.is_empty(),
            "expected no undefined-ident for CoroutineFunc, got {:?}",
            diags
        );
    }

    #[test]
    fn implicit_this_virtual_property_resolves() {
        // `windowOpen` is declared only via `get_windowOpen` / `set_windowOpen`.
        // Bare `windowOpen` inside a method should match the getter/setter.
        let source = r#"
            class C {
                bool tabOpen;
                bool get_windowOpen() { return !tabOpen; }
                void set_windowOpen(bool value) { tabOpen = !value; }
                void f() {
                    windowOpen = !windowOpen;
                }
            }
        "#;
        let diags = check(source);
        let undef: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedIdentifier(n) if n == "windowOpen"))
            .collect();
        assert!(
            undef.is_empty(),
            "expected no undefined-ident for windowOpen, got {:?}",
            diags
        );
    }

    #[test]
    fn auto_local_inferred_from_int_literal() {
        let diags = check("void f() { auto x = 42; int y = x; }");
        assert!(diags.is_empty(), "expected no diagnostics, got {:?}", diags);
    }

    #[test]
    fn auto_local_inferred_from_member_access() {
        let diags = check("class Foo { int bar; } void f() { Foo@ foo; auto b = foo.bar; }");
        assert!(diags.is_empty(), "expected no diagnostics, got {:?}", diags);
    }

    #[test]
    fn missing_return_on_nonvoid_function_fires() {
        let diags = check("int f() { }");
        let missing: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::MissingReturn { .. }))
            .collect();
        assert_eq!(missing.len(), 1, "expected 1 MissingReturn, got {:?}", diags);
        assert_eq!(
            missing[0].kind,
            TypeDiagnosticKind::MissingReturn {
                function_name: "f".into()
            }
        );
    }

    #[test]
    fn return_in_all_if_branches_suppresses_missing_return() {
        let diags = check("int f() { if (true) { return 1; } else { return 2; } }");
        let missing: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::MissingReturn { .. }))
            .collect();
        assert!(missing.is_empty(), "expected no MissingReturn, got {:?}", diags);
    }

    #[test]
    fn void_function_without_return_ok() {
        let diags = check("void f() { }");
        let missing: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::MissingReturn { .. }))
            .collect();
        assert!(missing.is_empty(), "expected no MissingReturn, got {:?}", diags);
    }

    #[test]
    fn return_in_single_branch_if_does_not_suppress() {
        let diags = check("int f() { if (true) { return 1; } }");
        let missing: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::MissingReturn { .. }))
            .collect();
        assert_eq!(missing.len(), 1, "expected 1 MissingReturn, got {:?}", diags);
    }

    #[test]
    fn arg_count_match_ok() {
        let diags = check("void f(int a, int b) {} void main() { f(1, 2); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert!(bad.is_empty(), "expected no ArgCountMismatch, got {:?}", diags);
    }

    #[test]
    fn arg_count_too_few_fires() {
        let diags = check("void f(int a, int b) {} void main() { f(1); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert_eq!(bad.len(), 1, "expected 1 ArgCountMismatch, got {:?}", diags);
        assert_eq!(
            bad[0].kind,
            TypeDiagnosticKind::ArgCountMismatch {
                function_name: "f".into(),
                expected_min: 2,
                expected_max: 2,
                got: 1,
            }
        );
    }

    #[test]
    fn arg_count_too_many_fires() {
        let diags = check("void f(int a) {} void main() { f(1, 2); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert_eq!(bad.len(), 1, "expected 1 ArgCountMismatch, got {:?}", diags);
        assert_eq!(
            bad[0].kind,
            TypeDiagnosticKind::ArgCountMismatch {
                function_name: "f".into(),
                expected_min: 1,
                expected_max: 1,
                got: 2,
            }
        );
    }

    #[test]
    fn arg_count_optional_params_respected() {
        let diags = check("void f(int a, int b = 3) {} void main() { f(1); f(1, 2); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert!(bad.is_empty(), "expected no ArgCountMismatch, got {:?}", diags);
    }

    #[test]
    fn arg_count_overloaded_suppressed() {
        let diags = check("void f(int a) {} void f(int a, int b) {} void main() { f(); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgCountMismatch for overloaded call, got {:?}",
            diags
        );
    }

    #[test]
    fn super_resolves_in_class() {
        let diags = check("class C { void f() { auto x = super; } }");
        let undef: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedIdentifier(n) if n == "super"))
            .collect();
        assert!(
            undef.is_empty(),
            "expected no undefined-ident for super, got {:?}",
            diags
        );
    }

    #[test]
    fn assign_to_ident_ok() {
        let diags = check("void f() { int x = 1; x = 2; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::InvalidAssignmentTarget))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no InvalidAssignmentTarget, got {:?}",
            diags
        );
    }

    #[test]
    fn assign_to_member_ok() {
        let diags = check("class C { int x; } void f() { C@ c; c.x = 1; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::InvalidAssignmentTarget))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no InvalidAssignmentTarget, got {:?}",
            diags
        );
    }

    #[test]
    fn assign_to_index_ok() {
        let diags = check("void f() { array<int> a; a[0] = 1; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::InvalidAssignmentTarget))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no InvalidAssignmentTarget, got {:?}",
            diags
        );
    }

    #[test]
    fn assign_to_literal_fires() {
        let diags = check("void f() { 1 = 2; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::InvalidAssignmentTarget))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 InvalidAssignmentTarget, got {:?}",
            diags
        );
    }

    #[test]
    fn assign_to_call_fires() {
        let diags = check("int g() { return 0; } void f() { g() = 1; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::InvalidAssignmentTarget))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 InvalidAssignmentTarget, got {:?}",
            diags
        );
    }

    #[test]
    fn assign_to_binary_fires() {
        let diags = check("void f() { int a=1; int b=2; (a + b) = 3; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::InvalidAssignmentTarget))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 InvalidAssignmentTarget, got {:?}",
            diags
        );
    }

    #[test]
    fn return_int_from_int_ok() {
        let diags = check("int f() { return 1; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ReturnTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ReturnTypeMismatch, got {:?}",
            diags
        );
    }

    #[test]
    fn return_string_from_int_fires() {
        let diags = check("int f() { return \"hello\"; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ReturnTypeMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ReturnTypeMismatch, got {:?}",
            diags
        );
    }

    #[test]
    fn return_ident_preserves_silence() {
        let diags = check("int f() { return undefined_name; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ReturnTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ReturnTypeMismatch, got {:?}",
            diags
        );
    }

    #[test]
    fn return_from_void_ok() {
        let diags = check("void f() { return; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ReturnTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ReturnTypeMismatch, got {:?}",
            diags
        );
    }

    #[test]
    fn return_null_from_handle_suppressed() {
        let diags = check("class C {} C@ f() { return null; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ReturnTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ReturnTypeMismatch, got {:?}",
            diags
        );
    }

    #[test]
    fn arg_type_primitive_match_ok() {
        let diags = check("void f(int a) {} void main() { f(1); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(bad.is_empty(), "expected no ArgTypeMismatch, got {:?}", diags);
    }

    #[test]
    fn arg_type_primitive_mismatch_fires() {
        let diags = check("void f(int a) {} void main() { f(\"x\"); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert_eq!(bad.len(), 1, "expected 1 ArgTypeMismatch, got {:?}", diags);
        assert_eq!(
            bad[0].kind,
            TypeDiagnosticKind::ArgTypeMismatch {
                function_name: "f".into(),
                param_index: 0,
                expected: "int".into(),
                got: "string".into(),
            }
        );
    }

    #[test]
    fn arg_type_non_primitive_suppressed() {
        let diags = check("class C {} void f(C@ c) {} void main() { f(null); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch on non-primitive param, got {:?}",
            diags
        );
    }

    #[test]
    fn arg_type_error_type_suppressed() {
        let diags = check("void f(int a) {} void main() { f(undefined_name); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch when arg is error-typed, got {:?}",
            diags
        );
    }

    #[test]
    fn arg_type_overloaded_suppressed() {
        let diags = check("void f(int a) {} void f(string a) {} void main() { f(1); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch for overloaded call, got {:?}",
            diags
        );
    }

    #[test]
    fn handle_assign_both_handles_ok() {
        let diags = check("class C {} void f() { C@ a; C@ b; @a = b; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::HandleValueMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no HandleValueMismatch, got {:?}",
            diags
        );
    }

    #[test]
    fn handle_assign_null_rhs_ok() {
        let diags = check("class C {} void f() { C@ a; @a = null; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::HandleValueMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no HandleValueMismatch, got {:?}",
            diags
        );
    }

    #[test]
    fn handle_assign_lhs_primitive_fires() {
        let diags = check("void f() { int x; @x = null; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::HandleValueMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 HandleValueMismatch, got {:?}",
            diags
        );
    }

    #[test]
    fn handle_assign_rhs_primitive_fires() {
        let diags = check("class C {} void f() { C@ a; @a = 42; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::HandleValueMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 HandleValueMismatch, got {:?}",
            diags
        );
    }

    #[test]
    fn const_local_assign_fires() {
        let diags = check("void f() { const int x = 5; x = 6; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ConstViolation, got {:?}",
            diags
        );
    }

    #[test]
    fn non_const_local_assign_ok() {
        let diags = check("void f() { int x = 5; x = 6; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ConstViolation, got {:?}",
            diags
        );
    }

    #[test]
    fn const_field_assign_fires() {
        let diags =
            check("class C { const int x; } void f() { C@ c; c.x = 6; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ConstViolation, got {:?}",
            diags
        );
    }

    #[test]
    fn const_compound_assign_fires() {
        let diags = check("void f() { const int x = 1; x += 2; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ConstViolation, got {:?}",
            diags
        );
    }

    #[test]
    fn handle_assign_to_const_fires() {
        let diags =
            check("class C {} void f() { const C@ a = null; @a = null; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .collect();
        assert!(
            !bad.is_empty(),
            "expected at least 1 ConstViolation, got {:?}",
            diags
        );
    }

    #[test]
    fn const_array_element_assign_fires() {
        // `const array<int>@ arr; arr[0] = 5;` — the receiver is const,
        // so indexing returns a `Const(int)` lvalue. Assigning into it
        // must fire a ConstViolation (iter 32).
        let diags = check(
            "void f() { const array<int>@ arr = null; arr[0] = 5; }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ConstViolation, got {:?}",
            diags
        );
    }

    #[test]
    fn const_array_element_read_is_fine() {
        // Pure reads of `const array<int>@ arr; int x = arr[0];` must
        // NOT fire ConstViolation — only assignment through the const
        // element should. (Iter 32 wraps the read in `Const(int)`, but
        // iter 24's const check only looks at assignment LHS.)
        let diags = check(
            "void f() { const array<int>@ arr = null; int x = arr[0]; }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ConstViolation on pure read, got {:?}",
            diags
        );
    }

    #[test]
    fn const_member_chain_fires() {
        // `const Foo@ f; f.field = 5;` where `field` is a non-const
        // `int` must still fire ConstViolation because the receiver is
        // const — iter 32 propagates that through `member_access_type`.
        let diags = check(
            "class Foo { int field; } void f() { const Foo@ x = null; x.field = 5; }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ConstViolation via chained const member access, got {:?}",
            diags
        );
    }

    #[test]
    fn non_const_member_receiver_not_const() {
        // Non-const receiver: `Foo f; f.field = 5;` must NOT fire.
        let diags = check(
            "class Foo { int field; } void f() { Foo x; x.field = 5; }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ConstViolation on non-const receiver, got {:?}",
            diags
        );
    }

    #[test]
    fn arg_type_int_to_float_implicitly_ok() {
        let diags = check("void f(float a) {} void main() { f(1); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch on int->float, got {:?}",
            diags
        );
    }

    #[test]
    fn arg_type_int_to_bool_fires() {
        let diags = check("void f(bool a) {} void main() { f(1); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ArgTypeMismatch on int->bool, got {:?}",
            diags
        );
    }

    #[test]
    fn arg_type_string_to_int_still_fires() {
        let diags = check("void f(int a) {} void main() { f(\"hi\"); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ArgTypeMismatch on string->int, got {:?}",
            diags
        );
    }

    #[test]
    fn return_int_from_double_ok() {
        let diags = check("double f() { return 1; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ReturnTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ReturnTypeMismatch on int->double, got {:?}",
            diags
        );
    }

    #[test]
    fn return_bool_from_int_fires() {
        let diags = check("bool f() { return 1; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ReturnTypeMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ReturnTypeMismatch on int->bool, got {:?}",
            diags
        );
    }

    #[test]
    fn overload_exact_match_picked() {
        let diags = check(
            "void f(int a) {} void f(string a) {} void main() { f(1); }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch for exact overload match, got {:?}",
            diags
        );
    }

    #[test]
    fn overload_convertible_match_picked() {
        let diags = check(
            "void f(float a) {} void f(string a) {} void main() { f(1); }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch for convertible overload match, got {:?}",
            diags
        );
    }

    #[test]
    fn overload_no_match_all_fail() {
        let diags = check(
            "void f(int a) {} void f(bool a) {} void main() { f(\"hi\"); }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch on no-match overload (silent skip), got {:?}",
            diags
        );
    }

    #[test]
    fn overload_ambiguous_silent() {
        let diags = check(
            "void f(int a, float b) {} void f(float a, int b) {} void main() { f(1, 1); }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch on ambiguous overload (silent skip), got {:?}",
            diags
        );
    }

    #[test]
    fn overload_single_via_arg_count() {
        let diags = check(
            "void f(int a) {} void f(int a, int b) {} void main() { f(1); }",
        );
        let count: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        let tys: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            count.is_empty(),
            "expected no ArgCountMismatch when arity uniquely picks overload, got {:?}",
            diags
        );
        assert!(
            tys.is_empty(),
            "expected no ArgTypeMismatch when arity uniquely picks overload, got {:?}",
            diags
        );
    }

    // ── iter 27: cross-file class hierarchy ─────────────────────────────

    /// Check a "main" source against a workspace that also contains every
    /// entry in `siblings` (extracted into the same `SymbolTable` under
    /// distinct file ids). Returns the diagnostics produced by checking
    /// the main source only.
    fn check_workspace(main: &str, siblings: &[&str]) -> Vec<TypeDiagnostic> {
        let mut ws = SymbolTable::new();
        // Sibling files first so their symbols are visible when `main`
        // references them by name. File id assignment is arbitrary.
        for sibling in siblings {
            let tokens = tokenize_filtered(sibling);
            let mut parser = Parser::new(&tokens, sibling);
            let file = parser.parse_file();
            let fid = ws.allocate_file_id();
            let syms = SymbolTable::extract_symbols(fid, sibling, &file);
            ws.set_file_symbols(fid, syms);
        }
        // Main file last.
        let tokens = tokenize_filtered(main);
        let mut parser = Parser::new(&tokens, main);
        let file = parser.parse_file();
        let fid = ws.allocate_file_id();
        let syms = SymbolTable::extract_symbols(fid, main, &file);
        ws.set_file_symbols(fid, syms);

        let scope = GlobalScope::new(&ws, None);
        let mut checker = Checker::new(main, &scope);
        checker.check_file(&file);
        checker.diagnostics
    }

    #[test]
    fn child_inherits_parent_field_cross_file() {
        // Base is in file A, Foo : Base in file B. Accessing the
        // inherited `base_field` through a `Foo` instance must not
        // fire `UndefinedMember`.
        let base = "class Base { int base_field; }";
        let main = "class Foo : Base {} void use() { Foo f; int y = f.base_field; }";
        let diags = check_workspace(main, &[base]);
        let undef_member: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { .. }))
            .collect();
        assert!(
            undef_member.is_empty(),
            "expected no UndefinedMember on inherited cross-file field, got {:?}",
            diags
        );
    }

    #[test]
    fn child_inherits_parent_method_cross_file() {
        // Base is in file A with a method, Foo : Base in file B. Calling
        // `f.base_method()` must resolve through the cross-file chain.
        let base = "class Base { int base_method() { return 0; } }";
        let main =
            "class Foo : Base {} void use() { Foo f; int y = f.base_method(); }";
        let diags = check_workspace(main, &[base]);
        let undef_member: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { .. }))
            .collect();
        assert!(
            undef_member.is_empty(),
            "expected no UndefinedMember on inherited cross-file method, got {:?}",
            diags
        );
    }

    #[test]
    fn grandchild_two_levels_cross_file() {
        // Three-level chain: A → B → C, each in its own file. Accessing
        // A's member through a C instance must walk both hops.
        let a = "class GA { int ga_field; }";
        let b = "class GB : GA {}";
        let main = "class GC : GB {} void use() { GC c; int y = c.ga_field; }";
        let diags = check_workspace(main, &[a, b]);
        let undef_member: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { .. }))
            .collect();
        assert!(
            undef_member.is_empty(),
            "expected no UndefinedMember on two-level inherited field, got {:?}",
            diags
        );
    }

    #[test]
    fn override_shadows_parent_field() {
        // Both parent and child have a field named `shared`. The child's
        // declaration must be considered first (the walker terminates at
        // the first hit), so no UndefinedMember fires and the lookup
        // succeeds without ever ascending the chain.
        let base = "class Base { int shared; }";
        let main =
            "class Foo : Base { string shared; } void use() { Foo f; string y = f.shared; }";
        let diags = check_workspace(main, &[base]);
        let undef_member: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { .. }))
            .collect();
        assert!(
            undef_member.is_empty(),
            "expected no UndefinedMember when child shadows parent field, got {:?}",
            diags
        );
    }

    #[test]
    fn cycle_does_not_loop() {
        // Pathological inheritance cycle: A : B, B : A, each in its own
        // file. The cross-file walker must terminate (visited-set guard)
        // rather than stack-overflow or hang. Accessing a nonexistent
        // member should return cleanly — no UndefinedMember (workspace
        // types are silenced) and, critically, no infinite loop.
        let b = "class CycB : CycA { int b_field; }";
        let main =
            "class CycA : CycB { int a_field; } void use() { CycA a; int y = a.nonexistent_member; }";
        let diags = check_workspace(main, &[b]);
        // The test's primary assertion is "does not hang / stack-overflow".
        // As a secondary check, ensure we didn't crash and got back a
        // reasonable diagnostics list (either silent or with UndefinedMember
        // — both are fine; the key is termination).
        let _ = diags.len();
    }

    // ── iter 28: inherited types flow through downstream checks ─────────

    #[test]
    fn cross_file_inherited_field_type_flows_to_arg_check() {
        // Parent in sibling file declares `int counter`. Child in main
        // inherits it. Passing `f.counter` (int) to a `string` parameter
        // must fire ArgTypeMismatch — proves the inherited field's real
        // type (not `Error("")`) flows through member_access_type into
        // the arg-type check.
        let base = "class CBase { int counter; }";
        let main = "class CFoo : CBase {} void take_str(string s) {} \
                    void use() { CFoo f; take_str(f.counter); }";
        let diags = check_workspace(main, &[base]);
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ArgTypeMismatch on int→string via inherited field, got {:?}",
            diags
        );
    }

    #[test]
    fn cross_file_inherited_method_return_flows_to_arg_check() {
        // Parent in sibling file has `int get_count()`. Child inherits.
        // `child.get_count()` is called and its result (int) passed to a
        // `string` parameter — must fire ArgTypeMismatch via the return
        // type of the inherited method.
        let base = "class MBase { int get_count() { return 0; } }";
        let main = "class MChild : MBase {} void take_str(string s) {} \
                    void use() { MChild c; take_str(c.get_count()); }";
        let diags = check_workspace(main, &[base]);
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ArgTypeMismatch on inherited method return, got {:?}",
            diags
        );
    }

    #[test]
    fn cross_file_shadowed_field_keeps_child_type() {
        // Parent has `int x`, child redeclares `string x`. Accessing
        // `c.x` must resolve as string (child wins) — pass it to an
        // `int` parameter to see a string→int ArgTypeMismatch. If the
        // walker mistakenly returned the parent's int type no mismatch
        // would fire.
        //
        // Note: the child override lives in the *sibling* file so the
        // `file_classes` in-file fast path cannot shortcut — this
        // exercises `workspace_class_member`'s first-hit-wins ordering
        // (child is walked before parent) end-to-end.
        let base = "class SBase { int x; }";
        let sibling_child = "class SChild : SBase { string x; }";
        let main = "void take_int(int n) {} \
                    void use() { SChild c; take_int(c.x); }";
        let diags = check_workspace(main, &[base, sibling_child]);
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ArgTypeMismatch on shadowed child `string x` → int, got {:?}",
            diags
        );
    }

    #[test]
    fn cross_file_inherited_field_exact_type_still_silent() {
        // Regression: parent `int counter`, passing `f.counter` to an
        // int parameter must NOT fire ArgTypeMismatch — the inherited
        // type should match exactly.
        let base = "class OBase { int counter; }";
        let main = "class OFoo : OBase {} void take_int(int n) {} \
                    void use() { OFoo f; take_int(f.counter); }";
        let diags = check_workspace(main, &[base]);
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch when inherited int matches int param, got {:?}",
            diags
        );
    }

    // ── iter 31: implicit-this cross-file inherited members ─────────────

    #[test]
    fn method_uses_inherited_field_cross_file() {
        // Base in file A declares `int counter`. Foo in file B inherits
        // Base and a method body references `counter` by bare name
        // (implicit `this.counter`). Must not fire UndefinedIdentifier.
        let base = "class MFBase { int counter; }";
        let main = "class MFFoo : MFBase { void inc() { counter = counter + 1; } }";
        let diags = check_workspace(main, &[base]);
        let undef: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedIdentifier(_)))
            .collect();
        assert!(
            undef.is_empty(),
            "expected no UndefinedIdentifier on implicit-this inherited field, got {:?}",
            diags
        );
    }

    #[test]
    fn method_uses_inherited_method_cross_file() {
        // Base has `int get_count()`. Child's own method body calls
        // `get_count()` bare. Must not fire UndefinedIdentifier on the
        // callee.
        let base = "class MMBase { int get_count() { return 0; } }";
        let main = "class MMChild : MMBase { int wrap() { return get_count(); } }";
        let diags = check_workspace(main, &[base]);
        let undef: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedIdentifier(_)))
            .collect();
        assert!(
            undef.is_empty(),
            "expected no UndefinedIdentifier on implicit-this inherited method call, got {:?}",
            diags
        );
    }

    #[test]
    fn method_uses_inherited_field_with_type_flows() {
        // Base has `int counter`. Child method passes the bare `counter`
        // to a `string` parameter — must fire ArgTypeMismatch, proving
        // the inherited field's real type (int) flowed through the
        // implicit-this lookup into the arg-type check.
        let base = "class TFBase { int counter; }";
        let main = "class TFChild : TFBase { void go() { take_str(counter); } } \
                    void take_str(string s) {}";
        let diags = check_workspace(main, &[base]);
        let undef: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedIdentifier(_)))
            .collect();
        assert!(
            undef.is_empty(),
            "expected no UndefinedIdentifier on implicit-this inherited field, got {:?}",
            diags
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ArgTypeMismatch on int→string via implicit-this inherited field, got {:?}",
            diags
        );
    }

    #[test]
    fn cycle_cross_file_method_terminates() {
        // Pathological inheritance cycle: CycMA : CycMB, CycMB : CycMA.
        // A method body references a non-existent member by bare name
        // (triggers the implicit-this walker). Must terminate via the
        // cross-file walker's cycle guard.
        let b = "class CycMB : CycMA { int b_field; }";
        let main = "class CycMA : CycMB { void touch() { int _ = nonexistent_member; } }";
        let diags = check_workspace(main, &[b]);
        // Primary assertion: we got here (no hang / stack-overflow).
        let _ = diags.len();
    }

    #[test]
    fn array_index_returns_element_type() {
        // `array<int>[0]` feeds an `int` parameter — should not fire
        // ArgTypeMismatch. Passing it to a `bool` parameter should fire
        // because the element type is propagating correctly.
        let diags = check(
            "void ti(int n) {} void f() { array<int> a; ti(a[0]); }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch for int elem → int param, got {:?}",
            diags
        );

        let diags = check(
            "void tb(bool b) {} void f() { array<int> a; tb(a[0]); }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ArgTypeMismatch for int elem → bool param, got {:?}",
            diags
        );
    }

    #[test]
    fn array_length_is_numeric() {
        // `arr.Length` must flow as a uint into a uint-expected arg slot
        // without firing ArgTypeMismatch.
        let diags = check(
            "void tu(uint n) {} void f() { array<int> a; tu(a.Length); }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch for .Length → uint, got {:?}",
            diags
        );
        // And the lowercase variant.
        let diags = check(
            "void tu(uint n) {} void f() { array<int> a; tu(a.length); }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch for .length → uint, got {:?}",
            diags
        );
    }

    #[test]
    fn nested_array_of_handles() {
        // `array<Foo@>[0]` should resolve to a Foo handle, and accessing
        // `.x` on it should not fire UndefinedMember.
        let diags = check(
            "class Foo { int x; } \
             void f() { array<Foo@> a; int y = a[0].x; }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no UndefinedMember on array<Foo@>[0].x, got {:?}",
            diags
        );
    }

    #[test]
    fn array_shorthand_syntax() {
        // `int[] arr;` followed by indexing should work identically to
        // `array<int> arr;`.
        let diags = check(
            "void ti(int n) {} void f() { int[] a; ti(a[0]); }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::ArgTypeMismatch { .. }
                        | TypeDiagnosticKind::UndefinedMember { .. }
                )
            })
            .collect();
        assert!(
            bad.is_empty(),
            "expected no diagnostics for int[] index, got {:?}",
            diags
        );
    }

    #[test]
    fn dictionary_no_false_positive() {
        // `dictionary d; d.Set("k", 1);` must not emit UndefinedMember
        // or any spurious diagnostic — dictionary is opaque for now.
        let diags = check(
            "void f() { dictionary d; d.Set(\"k\", 1); }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::UndefinedMember { .. }
                        | TypeDiagnosticKind::ArgTypeMismatch { .. }
                        | TypeDiagnosticKind::UndefinedIdentifier(_)
                )
            })
            .collect();
        assert!(
            bad.is_empty(),
            "expected no diagnostics on dictionary usage, got {:?}",
            diags
        );
    }
}
