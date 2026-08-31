//! Expression walks over a parsed [`SourceFile`].
//!
//! Call hierarchy intentionally walks executable function/method bodies only.
//! Navigation's access-position classifier additionally needs declaration
//! initialisers and `property` accessor bodies, so it uses the explicitly
//! complete traversal below. Keeping the entry points separate preserves the
//! call-hierarchy surface while sharing the recursive statement/expression
//! machinery.

use crate::parser::ast::{
    ClassMember, Expr, ExprKind, FunctionDecl, ImportTarget, Item, Param, SourceFile, Stmt,
    StmtKind, SwitchLabel,
};

pub(crate) fn walk_file_exprs<F: FnMut(&Expr)>(file: &SourceFile, f: &mut F) {
    for item in &file.items {
        walk_item_exprs(item, f);
    }
}

/// Visit expressions in every executable body and declaration initializer.
///
/// Unlike [`walk_file_exprs`], this includes top-level and class-field
/// initializers, parameter defaults on every declaration shape, and
/// top-level/class `property` accessor bodies. Namespaces recurse through this
/// same complete entry point, so nested namespace/class combinations do not
/// silently fall back to the call-hierarchy subset.
pub(crate) fn walk_file_exprs_complete<F: FnMut(&Expr)>(file: &SourceFile, f: &mut F) {
    for item in &file.items {
        walk_item_exprs_complete(item, f);
    }
}

fn walk_param_defaults<F: FnMut(&Expr)>(params: &[Param], f: &mut F) {
    for param in params {
        if let Some(default) = &param.default_value {
            walk_expr_complete(default, f);
        }
    }
}

fn walk_function_complete<F: FnMut(&Expr)>(function: &FunctionDecl, f: &mut F) {
    walk_param_defaults(&function.params, f);
    if let Some(body) = &function.body {
        for stmt in &body.stmts {
            walk_stmt_exprs_complete(stmt, f);
        }
    }
}

fn walk_var_initializers<F: FnMut(&Expr)>(decl: &crate::parser::ast::VarDeclStmt, f: &mut F) {
    for declarator in &decl.declarators {
        if let Some(init) = &declarator.init {
            walk_expr_complete(init, f);
        }
    }
}

fn walk_property_bodies<F: FnMut(&Expr)>(property: &crate::parser::ast::PropertyDecl, f: &mut F) {
    if let Some(getter) = &property.getter {
        for stmt in &getter.stmts {
            walk_stmt_exprs_complete(stmt, f);
        }
    }
    if let Some((_, setter)) = &property.setter {
        for stmt in &setter.stmts {
            walk_stmt_exprs_complete(stmt, f);
        }
    }
}

fn walk_item_exprs_complete<F: FnMut(&Expr)>(item: &Item, f: &mut F) {
    match item {
        Item::Function(func) => walk_function_complete(func, f),
        Item::Namespace(ns) => {
            for nested in &ns.items {
                walk_item_exprs_complete(nested, f);
            }
        }
        Item::Class(class) => {
            for member in &class.members {
                match member {
                    ClassMember::Field(decl) => walk_var_initializers(decl, f),
                    ClassMember::Method(func)
                    | ClassMember::Constructor(func)
                    | ClassMember::Destructor(func) => walk_function_complete(func, f),
                    ClassMember::Property(property) => walk_property_bodies(property, f),
                }
            }
        }
        Item::Interface(interface) => {
            for method in &interface.methods {
                walk_function_complete(method, f);
            }
        }
        Item::Funcdef(funcdef) => walk_param_defaults(&funcdef.params, f),
        Item::VarDecl(decl) => walk_var_initializers(decl, f),
        Item::Property(property) => walk_property_bodies(property, f),
        Item::Enum(decl) => {
            for value in &decl.values {
                if let Some(expr) = &value.value {
                    walk_expr_complete(expr, f);
                }
            }
        }
        Item::Import(import) => {
            if let ImportTarget::Function { params, .. } = &import.what {
                walk_param_defaults(params, f);
            }
        }
        Item::Typedef(_) | Item::Error(_) => {}
    }
}

pub(crate) fn walk_item_exprs<F: FnMut(&Expr)>(item: &Item, f: &mut F) {
    match item {
        Item::Function(func) => {
            if let Some(body) = &func.body {
                for s in &body.stmts {
                    walk_stmt_exprs(s, f);
                }
            }
        }
        Item::Namespace(ns) => {
            for i in &ns.items {
                walk_item_exprs(i, f);
            }
        }
        Item::Class(cls) => {
            for m in &cls.members {
                if let ClassMember::Method(func)
                | ClassMember::Constructor(func)
                | ClassMember::Destructor(func) = m
                {
                    if let Some(body) = &func.body {
                        for s in &body.stmts {
                            walk_stmt_exprs(s, f);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn walk_stmt_exprs<F: FnMut(&Expr)>(stmt: &Stmt, f: &mut F) {
    walk_stmt_exprs_with(stmt, f, false);
}

fn walk_stmt_exprs_complete<F: FnMut(&Expr)>(stmt: &Stmt, f: &mut F) {
    walk_stmt_exprs_with(stmt, f, true);
}

fn walk_stmt_exprs_with<F: FnMut(&Expr)>(stmt: &Stmt, f: &mut F, complete: bool) {
    match &stmt.kind {
        StmtKind::Expr(e) => walk_expr_with(e, f, complete),
        StmtKind::VarDecl(vd) => {
            for d in &vd.declarators {
                if let Some(e) = &d.init {
                    walk_expr_with(e, f, complete);
                }
            }
        }
        StmtKind::Block(stmts) => {
            for s in stmts {
                walk_stmt_exprs_with(s, f, complete);
            }
        }
        StmtKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            walk_expr_with(condition, f, complete);
            walk_stmt_exprs_with(then_branch, f, complete);
            if let Some(e) = else_branch {
                walk_stmt_exprs_with(e, f, complete);
            }
        }
        StmtKind::For {
            init,
            condition,
            step,
            body,
        } => {
            if let Some(i) = init {
                walk_stmt_exprs_with(i, f, complete);
            }
            if let Some(c) = condition {
                walk_expr_with(c, f, complete);
            }
            for s in step {
                walk_expr_with(s, f, complete);
            }
            walk_stmt_exprs_with(body, f, complete);
        }
        StmtKind::While { condition, body } | StmtKind::DoWhile { body, condition } => {
            walk_expr_with(condition, f, complete);
            walk_stmt_exprs_with(body, f, complete);
        }
        StmtKind::Switch { expr, cases } => {
            walk_expr_with(expr, f, complete);
            for c in cases {
                if let SwitchLabel::Case(e) = &c.label {
                    walk_expr_with(e, f, complete);
                }
                for s in &c.stmts {
                    walk_stmt_exprs_with(s, f, complete);
                }
            }
        }
        StmtKind::Return(Some(e)) => walk_expr_with(e, f, complete),
        StmtKind::TryCatch {
            try_body,
            catch_body,
        } => {
            walk_stmt_exprs_with(try_body, f, complete);
            walk_stmt_exprs_with(catch_body, f, complete);
        }
        _ => {}
    }
}

fn walk_expr_complete<F: FnMut(&Expr)>(expr: &Expr, f: &mut F) {
    walk_expr_with(expr, f, true);
}

fn walk_expr_with<F: FnMut(&Expr)>(expr: &Expr, f: &mut F, complete: bool) {
    f(expr);
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            walk_expr_with(callee, f, complete);
            for a in args {
                walk_expr_with(&a.value, f, complete);
            }
        }
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Assign { lhs, rhs, .. }
        | ExprKind::HandleAssign { lhs, rhs } => {
            walk_expr_with(lhs, f, complete);
            walk_expr_with(rhs, f, complete);
        }
        ExprKind::Unary { expr: inner, .. } | ExprKind::Postfix { expr: inner, .. } => {
            walk_expr_with(inner, f, complete);
        }
        ExprKind::Member { object, .. } => walk_expr_with(object, f, complete),
        ExprKind::Index { object, index } => {
            walk_expr_with(object, f, complete);
            walk_expr_with(index, f, complete);
        }
        ExprKind::Cast { expr: inner, .. } => walk_expr_with(inner, f, complete),
        ExprKind::TypeConstruct { args, .. } => {
            for a in args {
                walk_expr_with(a, f, complete);
            }
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_expr_with(condition, f, complete);
            walk_expr_with(then_expr, f, complete);
            walk_expr_with(else_expr, f, complete);
        }
        ExprKind::ArrayInit(items) => {
            for i in items {
                walk_expr_with(i, f, complete);
            }
        }
        ExprKind::Is {
            expr: inner,
            target,
            ..
        } => {
            walk_expr_with(inner, f, complete);
            if let crate::parser::ast::IsTarget::Expr(e) = target {
                walk_expr_with(e, f, complete);
            }
        }
        ExprKind::Lambda { params, body } => {
            if complete {
                walk_param_defaults(params, f);
            }
            for s in &body.stmts {
                walk_stmt_exprs_with(s, f, complete);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_walk_adds_declaration_and_property_bodies_without_broadening_call_hierarchy() {
        let source = "void f() { a = 1; }\nuint global = (b = 2);\nclass C {\n    uint field = (c = 3);\n    uint Value { get { d = 4; return 0; } }\n}\n";
        let (file, errors) = crate::parser::parser::parse(source);
        assert!(errors.is_empty(), "parse errors: {errors:?}");

        let mut call_hierarchy_assigns = 0;
        walk_file_exprs(&file, &mut |expr| {
            call_hierarchy_assigns += usize::from(matches!(expr.kind, ExprKind::Assign { .. }));
        });
        assert_eq!(
            call_hierarchy_assigns, 1,
            "the original call-hierarchy traversal must remain function-body-only"
        );

        let mut complete_assigns = 0;
        walk_file_exprs_complete(&file, &mut |expr| {
            complete_assigns += usize::from(matches!(expr.kind, ExprKind::Assign { .. }));
        });
        assert_eq!(
            complete_assigns, 4,
            "the access-classification traversal must also include both initializers and property bodies"
        );
    }

    #[test]
    fn complete_walk_adds_all_declaration_parameter_defaults_without_broadening_call_hierarchy() {
        let source = "void free(uint value = (a = 1)) {}\nclass C {\n    C(uint value = (b = 2)) {}\n    void method(uint value = (c = 3)) {}\n}\ninterface I { void method(uint value = (d = 4)); }\nfuncdef void Callback(uint value = (e = 5));\nimport void Remote(uint value = (f = 6)) from 'M';\n";
        let (file, errors) = crate::parser::parser::parse(source);
        assert!(errors.is_empty(), "parse errors: {errors:?}");

        let mut call_hierarchy_assigns = 0;
        walk_file_exprs(&file, &mut |expr| {
            call_hierarchy_assigns += usize::from(matches!(expr.kind, ExprKind::Assign { .. }));
        });
        assert_eq!(
            call_hierarchy_assigns, 0,
            "parameter defaults must remain outside the call-hierarchy traversal"
        );

        let mut complete_assigns = 0;
        walk_file_exprs_complete(&file, &mut |expr| {
            complete_assigns += usize::from(matches!(expr.kind, ExprKind::Assign { .. }));
        });
        assert_eq!(
            complete_assigns, 6,
            "free, constructor, method, interface, funcdef, and import defaults must be visited"
        );
    }
}
