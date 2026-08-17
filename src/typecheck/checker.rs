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
use super::call_site::{self, ArgBind};
use super::global_scope::{GlobalScope, OverloadSig};
use super::repr::{PrimitiveType, TypeRepr};
use super::resolver::TypeResolver;
use crate::lexer::token::TokenKind;
use crate::lexer::Span;
use crate::parser::ast::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeDiagnosticSeverity {
    Error,
    Warning,
}

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
    /// Game sanity check: bare `string` params should be `const string &in`.
    StringByValueParam {
        param_name: String,
    },
    /// Game parity: unary `!` is bool-only in AngelScript. Applying it to a
    /// handle, class instance, or other non-bool operand fails in-game with
    /// "Illegal operation on this datatype" (OP 1.29.5, tm-control-mcp
    /// AsyncDispatch.as:136 — `!result.Get("success", false)`).
    IllegalUnaryOperand {
        op: String,
        operand_type: String,
    },
    /// Game parity (GH #37): float→int implicit conversion in a variable
    /// initializer. Two message variants matching the game compiler:
    /// `literal: true` (e.g. `int ms = 3.7;`) → "Implicit conversion of
    /// value is not exact"; `literal: false` (e.g. `int g = f;`) → "Float
    /// value truncated in implicit conversion to integer".
    FloatTruncation {
        literal: bool,
    },
    /// Game parity (GH #37): a compile-time-known negative integer literal
    /// (`-1`) initializes an unsigned type: `uint u = -1;` → "Implicit
    /// conversion changed sign of value". Runtime int→uint conversions of
    /// variables are NOT warned (game behavior).
    SignChange,
    /// Game parity (GH #37): a relational comparison (`<`, `<=`, `>`, `>=`)
    /// between a signed and an unsigned integer operand → "Signed/Unsigned
    /// mismatch". Only compile-time-verifiable primitive mixes warn; Error /
    /// unknown / non-primitive operands stay silent.
    SignedUnsignedMismatch,
    /// GH #37: a second (or later) top-level function declaration with the
    /// same namespace-qualified name and same parameter type list. Matches
    /// the game compiler error exactly (ERROR severity; compile aborts).
    /// Class methods are out of scope (separate world).
    DuplicateFunction {
        function_name: String,
    },
    /// Game parity (GH #37): a local variable declaration whose name already
    /// exists as a local (or parameter — params live in the outermost
    /// function frame) in an enclosing frame of the SAME function. The game
    /// warns: `Variable 'X' hides another variable of same name in outer
    /// scope` at the inner declarator. Conservative: local-vs-local only;
    /// class members and globals never trigger this.
    VariableShadow {
        name: String,
    },
    /// Game parity (GH #37): statement(s) after a terminating statement
    /// (return/break/continue, or an if-else where both branches terminate)
    /// inside the same block. The game warns `Unreachable code` once per
    /// run at the first unreachable statement.
    UnreachableCode,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeDiagnostic {
    pub span: Span,
    pub kind: TypeDiagnosticKind,
}

impl TypeDiagnostic {
    pub fn severity(&self) -> TypeDiagnosticSeverity {
        match &self.kind {
            TypeDiagnosticKind::StringByValueParam { .. }
            | TypeDiagnosticKind::FloatTruncation { .. }
            | TypeDiagnosticKind::SignChange
            | TypeDiagnosticKind::SignedUnsignedMismatch
            | TypeDiagnosticKind::VariableShadow { .. }
            | TypeDiagnosticKind::UnreachableCode => TypeDiagnosticSeverity::Warning,
            _ => TypeDiagnosticSeverity::Error,
        }
    }

    pub fn message(&self) -> String {
        match &self.kind {
            TypeDiagnosticKind::UnknownType(n) => {
                let mut m = format!("unknown type `{}`", n);
                if let Some(note) = unknown_type_export_note(n) {
                    m.push_str(&format!(" (note: {})", note));
                }
                m
            }
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
            TypeDiagnosticKind::StringByValueParam { param_name } => {
                // Match Openplanet 1.29.5 compiler wording exactly (RemoteBuild probe).
                let base = if param_name.is_empty() {
                    "Sanity check: Use 'const string &in' to pass a string by reference".to_string()
                } else {
                    format!(
                        "Sanity check: Use 'const string &in {}' to pass a string by reference",
                        param_name
                    )
                };
                format!(
                    "{} (prefix the parameter name with an underscore to ignore this warning)",
                    base
                )
            }
            TypeDiagnosticKind::IllegalUnaryOperand { op, operand_type } => format!(
                // Match Openplanet 1.29.5 compiler wording class ("Illegal
                // operation on this datatype").
                "illegal operation `{} {}`: `{}` does not support this operator",
                op, operand_type, operand_type
            ),
            TypeDiagnosticKind::FloatTruncation { literal } => {
                // Exact game-compiler wording (RemoteBuild probe 2026-08-17).
                if *literal {
                    "Implicit conversion of value is not exact".to_string()
                } else {
                    "Float value truncated in implicit conversion to integer".to_string()
                }
            }
            TypeDiagnosticKind::SignChange => {
                // Exact game-compiler wording (RemoteBuild probe 2026-08-17).
                "Implicit conversion changed sign of value".to_string()
            }
            TypeDiagnosticKind::SignedUnsignedMismatch => {
                // Exact game-compiler wording (RemoteBuild probe 2026-08-17).
                "Signed/Unsigned mismatch".to_string()
            }
            TypeDiagnosticKind::DuplicateFunction { .. } => {
                // Exact game-compiler wording (RemoteBuild probe 2026-08-17,
                // Openplanet 1.29.5).
                "A function with the same name and parameters already exists".to_string()
            }
            TypeDiagnosticKind::VariableShadow { name } => format!(
                "Variable '{}' hides another variable of same name in outer scope",
                name
            ),
            TypeDiagnosticKind::UnreachableCode => "Unreachable code".to_string(),
        }
    }
}

impl std::fmt::Display for TypeDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// GH #26: when a qualified type name fails to resolve and its leading
/// segment is NOT a known engine namespace, the most common cause is a
/// cross-plugin export dependency that didn't load (missing install, not
/// visible to `--plugins-dir`, or its `exports` failed to parse — see the
/// dep-load pass from GH #20). Return a one-line note for that case; `None`
/// keeps the plain error for bare names and engine-namespace typos.
///
/// The known-namespace set is the static union of Openplanet Core API
/// namespaces (`Text`, `Math`, `IO`, …) and Nadeo engine groups (`Game`,
/// `TrackMania`, `MwFoundations`, …) from the OpenplanetCore/OpenplanetNext
/// typedb layout. It deliberately does NOT consult the loaded typedb so the
/// note stays stable across typedb versions.
fn unknown_type_export_note(name: &str) -> Option<&'static str> {
    let prefix = name.split("::").next()?;
    if prefix == name {
        return None; // bare name — no namespace signal
    }
    if is_engine_namespace(prefix) {
        return None;
    }
    Some(
        "qualified name looks like a plugin export; ensure the dependency \
         is installed and visible to --plugins-dir (folder or .op) and that \
         its exports loaded",
    )
}

/// Leading segments of every namespace/group in the Openplanet Core API and
/// the Nadeo engine typedb (OP 1.29.5 fixture ground truth; see GH #26).
fn is_engine_namespace(prefix: &str) -> bool {
    matches!(
        prefix,
        // Core API namespaces (OpenplanetCore.json `ns` values, leading seg).
        "Audio"
            | "Auth"
            | "Crypto"
            | "Dev"
            | "Discord"
            | "Display"
            | "Fids"
            | "IO"
            | "Icons"
            | "Import"
            | "Json"
            | "Math"
            | "Meta"
            | "Net"
            | "Path"
            | "Permissions"
            | "Reflection"
            | "Regex"
            | "SQLite"
            | "Settings"
            | "Text"
            | "Time"
            | "UI"
            | "XML"
            | "mat3"
            | "mat4"
            | "nvg"
            | "string"
            // Nadeo engine groups (OpenplanetNext.json `ns` keys).
            | "Control"
            | "Function"
            | "Game"
            | "GameData"
            | "Graphic"
            | "Hms"
            | "Input"
            | "MetaNotPersistent"
            | "MwFoundations"
            | "Plug"
            | "Scene"
            | "Script"
            | "ShootMania"
            | "System"
            | "TrackMania"
            | "Vision"
            | "Xml"
            // Deep Core namespaces, split on leading segment above.
            | "Internal"
    )
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
    is_mixin: bool,
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
    /// to `(parent_names, members)`. Used so implicit-this member lookups
    /// can walk the parent chain for cross-method resolution within the
    /// same file.
    file_classes: std::collections::HashMap<String, (Vec<String>, Vec<(String, TypeRepr)>)>,
    /// Set of `(class_qualname, method_name)` for workspace-local methods
    /// declared with a trailing `const` qualifier. Used by AC19
    /// (method-call const propagation): when a non-const method is
    /// invoked on a const receiver, the return type is wrapped in
    /// `Const(_)`; a const method leaves the return type untouched.
    file_const_methods: std::collections::HashSet<(String, String)>,
    return_type_stack: Vec<TypeRepr>,
    /// Stack of `frames.len()` values at each function/lambda entry —
    /// marks the first frame owned by the innermost function so shadow
    /// checks (GH #37) stay within one function.
    function_frame_starts: Vec<usize>,
    pub diagnostics: Vec<TypeDiagnostic>,
    /// Span-start → computed type for every expression visited by
    /// `expr_type` (query surface, GH #42). Only recorded when
    /// `record_types` is set; innermost expression wins on shared starts.
    expr_types: std::collections::HashMap<u32, TypeRepr>,
    /// Full (start, end) → type map backing the span-containment queries
    /// (`type_at_offset`, GH #42 stage 2). Same recording pass as
    /// `expr_types`; kept separate because the two query shapes have
    /// different collision rules.
    span_expr_types: std::collections::HashMap<(u32, u32), TypeRepr>,
    record_types: bool,
    /// Set of `(qualified_name, param_type_signature)` pairs seen for
    /// top-level (free) function declarations in this file. Used by GH #37
    /// slice 3 to flag `DuplicateFunction` at the second and later decl.
    seen_function_sigs: std::collections::HashSet<(String, String)>,
    /// Scoped suppression for the `+`-mix arm of SignedUnsignedMismatch
    /// (GH #37): while walking the operands of a NON-relational arithmetic
    /// op (`*`/`&`/…), an inner Add-mix must not warn — the game only fires
    /// when the mix is topmost (or feeds a comparison). Depth counter so
    /// nested binary walks restore correctly.
    arith_suppress_add_mix: u32,
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
            file_const_methods: std::collections::HashSet::new(),
            return_type_stack: Vec::new(),
            function_frame_starts: Vec::new(),
            diagnostics: Vec::new(),
            expr_types: std::collections::HashMap::new(),
            span_expr_types: std::collections::HashMap::new(),
            record_types: false,
            seen_function_sigs: std::collections::HashSet::new(),
            arith_suppress_add_mix: 0,
        }
    }

    /// Enable span→type recording for the query surface (GH #42).
    /// Diagnostics are still collected; callers that only want queries
    /// simply ignore them.
    pub fn with_type_recording(mut self) -> Self {
        self.record_types = true;
        self
    }

    /// Type recorded for the expression starting at `span_start`
    /// (query surface, GH #42). Requires `with_type_recording`.
    pub fn type_at_span(&self, span_start: u32) -> Option<&TypeRepr> {
        self.expr_types.get(&span_start)
    }

    /// All recorded span→type entries.
    pub fn recorded_expr_types(&self) -> &std::collections::HashMap<u32, TypeRepr> {
        &self.expr_types
    }

    /// Type of the innermost recorded expression whose span contains
    /// `offset` (GH #42 stage 2). `record_types` must have been set; an
    /// empty map yields `None`. Ties on width resolve to the later
    /// recorded entry (innermost walk order).
    pub fn type_at_offset(&self, offset: u32) -> Option<&TypeRepr> {
        self.span_expr_types
            .iter()
            .filter(|((start, end), _)| *start <= offset && offset < *end)
            .min_by_key(|((start, end), _)| end - start)
            .map(|(_, ty)| ty)
    }

    /// Type of the recorded expression spanning exactly
    /// `start..end` (identifier-shaped lookups, GH #42 stage 2).
    pub fn type_at_span_range(&self, start: u32, end: u32) -> Option<&TypeRepr> {
        self.span_expr_types.get(&(start, end))
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
                    let parents = cls
                        .base_classes
                        .iter()
                        .map(|b| {
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
                        })
                        .collect();
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
                                    members
                                        .push((d.name.text(self.source).to_string(), ty.clone()));
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
                                members.push((prop.name.text(self.source).to_string(), ty));
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
                                let method_name = func.name.text(self.source).to_string();
                                // AC19: remember whether this method has a
                                // trailing `const` qualifier so
                                // `call_type::Member` can skip the
                                // receiver-const propagation when the
                                // method promises not to mutate `this`.
                                if func.is_const {
                                    self.file_const_methods
                                        .insert((qual.clone(), method_name.clone()));
                                }
                                members.push((method_name, ret));
                            }
                            _ => {}
                        }
                    }
                    self.file_classes.insert(qual, (parents, members));
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

    /// The number of frames belonging to the currently checked function —
    /// i.e. the param frame plus every block/loop frame opened since
    /// `check_function_decl` (or a lambda body) started. Shadow warnings
    /// (GH #37) only consider frames within this window so a lambda local
    /// never "hides" an outer-function local.
    fn function_frame_start(&self) -> usize {
        self.function_frame_starts.last().copied().unwrap_or(0)
    }

    fn define_local(&mut self, name: String, ty: TypeRepr, span: Span) {
        // GH #37: local-vs-local shadowing inside the SAME function —
        // warn when an enclosing frame of this function already holds the
        // name (params live in the outermost function frame, so a local
        // shadowing a param warns too). Same-frame redefinition is a
        // different (error-class) problem and is left alone here; class
        // members and globals never participate.
        let start = self.function_frame_start();
        if self.frames.len() > start + 1
            && self.frames[start..self.frames.len() - 1]
                .iter()
                .any(|f| f.locals.iter().any(|l| l.name == name))
        {
            self.diagnostics.push(TypeDiagnostic {
                span,
                kind: TypeDiagnosticKind::VariableShadow { name: name.clone() },
            });
        }
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

    fn collect_workspace_base_members(
        &self,
        class_name: &str,
        out: &mut Vec<(String, TypeRepr)>,
        visited: &mut std::collections::HashSet<String>,
    ) {
        if !visited.insert(class_name.to_string()) {
            return;
        }

        if let Some((_, members)) = self.file_classes.get(class_name) {
            out.extend(members.clone());
        } else {
            out.extend(self.scope.workspace_class_member_pairs(class_name));
        }

        for parent in self.scope.workspace_class_parents(class_name) {
            self.collect_workspace_base_members(&parent, out, visited);
        }
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
        let matches = |mname: &str| -> bool { mname == name || mname == getter || mname == setter };
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
            if let Some((parents, _)) = self.file_classes.get(&cls.name) {
                let mut current: std::collections::VecDeque<String> = parents.clone().into();
                let mut visited: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                let mut hops = 0usize;
                while let Some(pname) = current.pop_front() {
                    if !visited.insert(pname.clone()) {
                        continue;
                    }
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
                        current.extend(pp.iter().cloned());
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
            Item::Function(func) => {
                self.check_duplicate_function_decl(func);
                self.check_function_decl(func, true);
            }
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
                let prop_ty = self.resolve_type_expr(&prop.type_expr);
                if let Some(body) = &prop.getter {
                    self.push_frame();
                    self.check_function_body(body);
                    self.pop_frame();
                }
                if let Some((_, body)) = &prop.setter {
                    self.push_frame();
                    self.define_local("value".to_string(), prop_ty.clone(), prop.span);
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
        let simple_class_name = cls.name.text(self.source).to_string();
        let class_name = if self.namespace_stack.is_empty() {
            simple_class_name.clone()
        } else {
            format!("{}::{}", self.namespace_stack.join("::"), simple_class_name)
        };
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

        // AngelScript mixin classes contribute members directly to the class
        // body. Pull those in from the workspace symbol table so bare method
        // references like `DrawPlayerLabel(...)` resolve inside classes that
        // inherit `HasPlayerLabelDraw`.
        for base in &cls.base_classes {
            let Some(base_name) = Self::base_type_name(&self.resolve_type_expr(base)) else {
                continue;
            };
            let mut visited = std::collections::HashSet::new();
            self.collect_workspace_base_members(&base_name, &mut members, &mut visited);
        }

        self.push_class(ClassCtx {
            name: class_name,
            is_mixin: cls.is_mixin,
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
                let prop_ty = self.resolve_type_expr(&prop.type_expr);
                if let Some(body) = &prop.getter {
                    self.push_frame();
                    self.check_function_body(body);
                    self.pop_frame();
                }
                if let Some((_, body)) = &prop.setter {
                    self.push_frame();
                    self.define_local("value".to_string(), prop_ty.clone(), prop.span);
                    self.check_function_body(body);
                    self.pop_frame();
                }
            }
        }
    }

    /// GH #37 slice 3: flag the second (and later) top-level free function
    /// declaration with the same namespace-qualified name and the same
    /// parameter type list. Identity is by (qualified name, whitespace-
    /// normalized param type-expr text) — text comparison, not semantic
    /// type resolution, so `int a` vs `int b` collide (correct) while
    /// differently-spelled-but-equivalent types may slip through (residual).
    /// Class methods are out of scope (they are visited via
    /// `check_class_member`, not here).
    fn check_duplicate_function_decl(&mut self, func: &FunctionDecl) {
        let name = func.name.text(self.source).to_string();
        let qual_name = if self.namespace_stack.is_empty() {
            name
        } else {
            format!("{}::{}", self.namespace_stack.join("::"), name)
        };
        let param_sig = func
            .params
            .iter()
            .map(|p| {
                let ty = p.type_expr.span.text(self.source);
                ty.split_whitespace().collect::<Vec<_>>().join(" ")
            })
            .collect::<Vec<_>>()
            .join(",");
        if !self.seen_function_sigs.insert((qual_name, param_sig)) {
            self.diagnostics.push(TypeDiagnostic {
                span: func.span,
                kind: TypeDiagnosticKind::DuplicateFunction {
                    function_name: func.name.text(self.source).to_string(),
                },
            });
        }
    }

    fn check_function_decl(&mut self, func: &FunctionDecl, enforce_return: bool) {
        let ret_ty = self.resolve_type_expr(&func.return_type);
        self.return_type_stack.push(ret_ty.clone());
        self.function_frame_starts.push(self.frames.len());
        self.push_frame();
        for p in &func.params {
            self.warn_string_by_value_param(p);
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
        self.function_frame_starts.pop();
        self.return_type_stack.pop();
    }

    /// B004: AngelScript/Openplanet sanity check — bare `string` (or
    /// `const string`) parameters copy the string; prefer `const string &in`.
    /// Openplanet suppresses the warning when the parameter name is prefixed
    /// with `_`.
    fn warn_string_by_value_param(&mut self, param: &Param) {
        if !is_string_by_value_type(&param.type_expr) {
            return;
        }
        // Reference params (`string &in`, `const string &in`, …) are fine;
        // those set a non-None modifier extracted from the type expr.
        if param.modifier != ParamModifier::None {
            return;
        }
        let param_name = param
            .name
            .as_ref()
            .map(|n| n.text(self.source).to_string())
            .unwrap_or_default();
        // Game: "prefix the parameter name with an underscore to ignore"
        if param_name.starts_with('_') {
            return;
        }
        let span = param
            .name
            .as_ref()
            .map(|n| n.span)
            .unwrap_or(param.type_expr.span);
        self.diagnostics.push(TypeDiagnostic {
            span,
            kind: TypeDiagnosticKind::StringByValueParam { param_name },
        });
    }

    /// Conservative "does this statement slice definitely terminate control
    /// (return/break/continue) at ANY position?" check. Used for the
    /// MissingReturn diagnostic (via the last statement) and for the
    /// UnreachableCode warning (GH #37).
    fn stmts_terminate(&self, stmts: &[Stmt]) -> bool {
        stmts.iter().any(|s| self.stmt_terminates(s))
    }

    fn stmt_terminates(&self, stmt: &Stmt) -> bool {
        match &stmt.kind {
            StmtKind::Return(_) | StmtKind::Break | StmtKind::Continue => true,
            StmtKind::Block(inner) => self.stmts_terminate(inner),
            StmtKind::If {
                then_branch,
                else_branch: Some(eb),
                ..
            } => self.stmt_terminates(then_branch) && self.stmt_terminates(eb),
            StmtKind::TryCatch {
                try_body,
                catch_body,
            } => self.stmt_terminates(try_body) && self.stmt_terminates(catch_body),
            StmtKind::Switch { cases, .. } => {
                let has_default = cases
                    .iter()
                    .any(|c| matches!(c.label, SwitchLabel::Default));
                let mut suffix_terminates = true;
                for case in cases.iter().rev() {
                    suffix_terminates = if case.stmts.is_empty() {
                        suffix_terminates
                    } else {
                        self.case_returns(&case.stmts) && suffix_terminates
                    };
                }
                has_default && suffix_terminates
            }
            _ => false,
        }
    }

    /// MissingReturn semantics for a switch case body: control must leave
    /// via a RETURN, not a break/continue — a case that `break`s falls out
    /// of the switch, so it must not count as "returns". Only the last
    /// statement decides (a return buried mid-case leaves the rest of the
    /// case dead, which is fine for this conservative check).
    fn case_returns(&self, stmts: &[Stmt]) -> bool {
        match stmts.last() {
            None => false,
            Some(s) => match &s.kind {
                StmtKind::Return(_) => true,
                StmtKind::Block(inner) => self.case_returns(inner),
                StmtKind::If {
                    then_branch,
                    else_branch: Some(eb),
                    ..
                } => {
                    self.case_terminating_stmt_returns(then_branch)
                        && self.case_terminating_stmt_returns(eb)
                }
                _ => false,
            },
        }
    }

    fn case_terminating_stmt_returns(&self, stmt: &Stmt) -> bool {
        match &stmt.kind {
            StmtKind::Return(_) => true,
            StmtKind::Block(inner) => self.case_returns(inner),
            StmtKind::If {
                then_branch,
                else_branch: Some(eb),
                ..
            } => {
                self.case_terminating_stmt_returns(then_branch)
                    && self.case_terminating_stmt_returns(eb)
            }
            _ => false,
        }
    }

    fn check_function_body(&mut self, body: &FunctionBody) {
        self.check_stmt_block(&body.stmts);
    }

    /// Walk a block's statements, warning `Unreachable code` (GH #37) once
    /// per run at the first statement after a terminating one.
    fn check_stmt_block(&mut self, stmts: &[Stmt]) {
        let mut terminated = false;
        for stmt in stmts {
            if terminated {
                self.diagnostics.push(TypeDiagnostic {
                    span: stmt.span,
                    kind: TypeDiagnosticKind::UnreachableCode,
                });
                break;
            }
            self.check_stmt(stmt);
            if self.stmt_terminates(stmt) {
                terminated = true;
            }
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
                    let init_ty = self.expr_type(init);
                    self.check_init_conversion(&declared_ty, init, &init_ty);
                }
                declared_ty.clone()
            };
            self.define_local(d.name.text(self.source).to_string(), local_ty, d.name.span);
        }
    }

    /// GH #37 slice 1: warning-parity implicit conversions in variable
    /// initializers (game-compiler WARNING classes, RemoteBuild probe
    /// 2026-08-17). Conservative silence on Error/unknown/non-primitive
    /// operands — never warn on a type we can't classify confidently.
    fn check_init_conversion(&mut self, declared: &TypeRepr, init: &Expr, init_ty: &TypeRepr) {
        let target = declared.unwrap_const();
        let source = init_ty.unwrap_const();
        if target.is_error() || source.is_error() {
            return;
        }
        let (TypeRepr::Primitive(target_p), TypeRepr::Primitive(source_p)) = (target, source)
        else {
            return;
        };
        if is_unsigned_int(*target_p) {
            // SignChange: `uint u = -1;` — only compile-time-known negatives
            // (unary minus on an integer literal). Runtime int→uint of a
            // variable is NOT warned by the game.
            if let ExprKind::Unary {
                op: UnaryOp::Neg,
                expr: inner,
            } = &init.kind
            {
                if matches!(inner.kind, ExprKind::IntLit(_) | ExprKind::HexLit(_)) {
                    self.diagnostics.push(TypeDiagnostic {
                        span: init.span,
                        kind: TypeDiagnosticKind::SignChange,
                    });
                    return;
                }
            }
        }
        // FloatTruncation: float/double source into any integer target.
        // The game uses different messages for a literal (`int ms = 3.7;`
        // → "value is not exact") vs a non-literal expr (`int g = f;` →
        // "truncated in implicit conversion").
        let source_is_float = matches!(source_p, PrimitiveType::Float | PrimitiveType::Double);
        if source_is_float && is_integer(*target_p) {
            let literal = matches!(init.kind, ExprKind::FloatLit(_));
            self.diagnostics.push(TypeDiagnostic {
                span: init.span,
                kind: TypeDiagnosticKind::FloatTruncation { literal },
            });
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
                self.check_stmt_block(stmts);
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
                    self.check_stmt_block(&case.stmts);
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
        // Peel `Const`/`Handle` wrappers in either order:
        //   `const Foo@`  → Handle(Const(Foo))
        //   `Foo@ const`  → Const(Handle(Foo))
        //   `const Foo`   → Const(Foo)
        //   `Foo@`        → Handle(Foo)
        // We apply each peel twice so both orderings land on the base.
        let inner = ty
            .unwrap_const()
            .unwrap_handle()
            .unwrap_const()
            .unwrap_handle();
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
            // Core API registers `string` (and similar) as classes with
            // methods — map primitives to their keyword so `s.IndexOf(...)`
            // can resolve against the typedb (B003).
            TypeRepr::Primitive(p) => Some(p.as_str().to_string()),
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
    /// distinction — iter 38 (AC20) fixed that, so this predicate now
    /// answers only the contents-const question: is the value pointed /
    /// referred to by `ty` const?
    ///
    /// * `Const(T)`          → yes (the value itself is const)
    /// * `Handle(Const(T))`  → yes (pointee is const — `const Foo@`)
    /// * `Const(Handle(T))`  → NO  (const handle, mutable pointee —
    ///   `Foo@ const`)
    /// * `Handle(T)`         → no
    fn receiver_is_const(ty: &TypeRepr) -> bool {
        match ty {
            TypeRepr::Handle(inner) => matches!(inner.as_ref(), TypeRepr::Const(_)),
            TypeRepr::Const(inner) => {
                // A `Const(Handle(_))` is a const handle to a mutable
                // pointee: the contents are NOT const.
                !matches!(inner.as_ref(), TypeRepr::Handle(_))
            }
            _ => false,
        }
    }

    /// If `receiver_const` is true, propagate contents-const into `t`.
    /// A bare value `T` becomes `Const(T)`; a handle `Handle(T)` becomes
    /// `Handle(Const(T))` so that the const sticks to the pointee
    /// (matching AC20 semantics). Errors and already-const types pass
    /// through unchanged.
    fn apply_receiver_const(t: TypeRepr, receiver_const: bool) -> TypeRepr {
        if !receiver_const {
            return t;
        }
        match t {
            TypeRepr::Error(_) => t,
            TypeRepr::Const(_) => t,
            TypeRepr::Handle(inner) => match *inner {
                TypeRepr::Const(_) => TypeRepr::Handle(inner),
                inner_ty => TypeRepr::Handle(Box::new(TypeRepr::Const(Box::new(inner_ty)))),
            },
            other => TypeRepr::Const(Box::new(other)),
        }
    }

    /// Derive the type of `obj.member`, emitting an `UndefinedMember` if
    /// the lookup fails against a known (non-error) object type. When the
    /// object type is `Error(_)` we silently propagate `Error` so we don't
    /// double-report the same root cause.
    fn member_access_type(&mut self, obj_ty: &TypeRepr, member: &Ident, span: Span) -> TypeRepr {
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
        // Nadeo-sourced types can have incomplete member metadata. When the
        // typedb lists zero properties/methods for the type, a failed
        // lookup may just mean the DB is incomplete — stay silent. When
        // the type has a non-empty member list, trust it and emit
        // UndefinedMember for missing names (B006).
        if self.scope.is_nadeo_type(&type_name) && !self.scope.nadeo_member_list_trusted(&type_name)
        {
            return TypeRepr::Error(String::new());
        }
        // GH #21: `MwAddRef` / `MwRelease` are AngelScript builtins on every
        // CMwNod-derived type (manual refcount). The typedb never declares
        // them, so any CMwNod-derived receiver gets a free pass here.
        if matches!(member_name.as_str(), "MwAddRef" | "MwRelease")
            && self.scope.is_external_derived_from(&type_name, "CMwNod")
        {
            // Returns void; the checker has no Void TypeRepr, so use the
            // silence sentinel (same as untyped Nadeo methods above).
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

    /// Emit `ArgCountMismatch` when `got` falls outside every overload's
    /// `(min, max)` range. If any overload accepts `got`, stay silent
    /// (multi-overload: conservative — type resolution may still pick a
    /// winner later). Used for external method/free-function arity.
    fn check_arity_against_ranges(
        &mut self,
        display_name: &str,
        ranges: &[(usize, usize)],
        got: usize,
        span: Span,
    ) {
        if ranges.is_empty() {
            return;
        }
        if ranges
            .iter()
            .any(|(min_args, max_args)| got >= *min_args && got <= *max_args)
        {
            return;
        }
        let expected_min = ranges.iter().map(|(m, _)| *m).min().unwrap_or(0);
        let expected_max = ranges.iter().map(|(_, m)| *m).max().unwrap_or(0);
        self.diagnostics.push(TypeDiagnostic {
            span,
            kind: TypeDiagnosticKind::ArgCountMismatch {
                function_name: display_name.to_string(),
                expected_min,
                expected_max,
                got,
            },
        });
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
    /// Overload set for a possibly-qualified callable. For a `Class::Method`
    /// (or `Ns::Class::Method`) shape where `Class` is a known workspace class,
    /// include overloads inherited from its parent chain (GH #34) so a call
    /// matching only a parent-declared overload isn't flagged for arity. Free
    /// functions and unknown receivers fall back to the direct overload set.
    fn method_overloads_including_inherited(&self, qualified: &str) -> Vec<OverloadSig> {
        let direct = self.scope.lookup_function_overloads(qualified);
        let Some((class_part, method)) = qualified.rsplit_once("::") else {
            return direct;
        };
        if !self.scope.is_workspace_class(class_part) {
            return direct;
        }
        let augmented = self
            .scope
            .lookup_method_overloads_with_inheritance(class_part, method);
        if augmented.is_empty() {
            direct
        } else {
            augmented
        }
    }

    fn resolve_workspace_function_call(
        &mut self,
        display_name: &str,
        qualified: &str,
        args: &[CallArg],
        callee_span: Span,
        fallback_ret: TypeRepr,
    ) -> TypeRepr {
        let overloads = self.method_overloads_including_inherited(qualified);
        match overloads.len() {
            0 => {
                // Not a workspace function (external-only). Prefer unified
                // callables_free (I3) which returns external overloads when
                // workspace is empty.
                let external = self.scope.callables_free(qualified);
                if !external.is_empty() {
                    let ranges = call_site::arity_ranges(&external);
                    self.check_arity_against_ranges(display_name, &ranges, args.len(), callee_span);
                    self.check_external_call_arg_types(display_name, args, &external);
                } else if let Some(ranges) =
                    self.scope.lookup_external_function_arity_ranges(qualified)
                {
                    self.check_arity_against_ranges(display_name, &ranges, args.len(), callee_span);
                    self.walk_args(args);
                } else {
                    self.walk_args(args);
                }
                fallback_ret
            }
            1 => {
                // Single-overload fast path: identical to iter 19/22
                // behaviour. Use the legacy helpers so existing tests keep
                // passing byte-for-byte.
                self.check_arg_count(display_name, qualified, args.len(), callee_span);
                if let Some(params) = self.scope.lookup_function_params(qualified) {
                    self.walk_args_and_check_types(display_name, args, &params);
                } else {
                    self.walk_args(args);
                }
                fallback_ret
            }
            _ => {
                // 2+ overloads: walk args once, run real resolution.
                // Named-arg overload resolution is not implemented yet —
                // bind types in call order (positional) for matching.
                let arg_tys: Vec<TypeRepr> =
                    args.iter().map(|a| self.expr_type(&a.value)).collect();
                match resolve_overload(&overloads, &arg_tys) {
                    OverloadMatch::Unique(sig) => {
                        // A unique winner means every primitive arg either
                        // matched exactly or was convertible — no further
                        // ArgTypeMismatch emission needed. Parse the
                        // winner's return type.
                        TypeRepr::parse_type_string(&sig.return_type)
                    }
                    OverloadMatch::Ambiguous | OverloadMatch::NoOverloads => {
                        // Silent skip — matches iter 19/22 overloaded
                        // behaviour. Return the lookup fallback so downstream
                        // `.member` chains still see *some* type.
                        fallback_ret
                    }
                    OverloadMatch::NoMatch => {
                        // No overload accepted the args. Stay silent on
                        // TYPE-driven no-match (conservative — a conversion or
                        // workspace-local arg may resolve later), but a call
                        // whose count is outside EVERY overload's arity range
                        // is unambiguously wrong: diagnose it (GH #34 review
                        // Arm B — the augmented inherited set must not swallow
                        // a genuinely-wrong call the single-overload path used
                        // to catch).
                        let ranges: Vec<(usize, usize)> = overloads
                            .iter()
                            .map(|s| (s.min_args, s.param_types.len()))
                            .collect();
                        self.check_arity_against_ranges(
                            display_name,
                            &ranges,
                            args.len(),
                            callee_span,
                        );
                        fallback_ret
                    }
                }
            }
        }
    }

    /// Walk each argument expression exactly once, typing them for side
    /// effects (diagnostics) and discarding the results. Used by call-site
    /// dispatch branches that don't need arg types.
    fn walk_args(&mut self, args: &[CallArg]) {
        for a in args {
            let _ = self.expr_type(&a.value);
        }
    }

    /// Peel `Const` / `Handle` wrappers (either order) down to the value type.
    fn peel_const_handle(ty: &TypeRepr) -> &TypeRepr {
        ty.unwrap_const()
            .unwrap_handle()
            .unwrap_const()
            .unwrap_handle()
    }

    /// True for AngelScript any-type `?` and unsubstituted generic params
    /// (`T`, `T[]`, nested) that appear in Core typedb array methods.
    fn is_unsubstituted_generic_or_any(ty: &TypeRepr) -> bool {
        match ty {
            TypeRepr::Named(n) if n == "?" || n == "T" => true,
            TypeRepr::Array(inner) | TypeRepr::Handle(inner) | TypeRepr::Const(inner) => {
                Self::is_unsubstituted_generic_or_any(inner)
            }
            TypeRepr::Generic { args, .. } => {
                args.iter().any(Self::is_unsubstituted_generic_or_any)
            }
            _ => false,
        }
    }

    /// Bare function values (`Funcdef`) and CoroutineFunc* names are
    /// interchangeable at `startnew` / funcdef parameter sites.
    fn funcdef_converts_to_coroutine(arg: &TypeRepr, param: &TypeRepr) -> bool {
        let is_coro = |t: &TypeRepr| match t {
            TypeRepr::Named(n) | TypeRepr::Funcdef(n) => {
                n == "CoroutineFunc"
                    || n.starts_with("CoroutineFuncUserdata")
                    || n == "CoroutineFuncUserdata"
            }
            _ => false,
        };
        match arg {
            TypeRepr::Funcdef(_) if is_coro(param) => true,
            TypeRepr::Named(n) | TypeRepr::Funcdef(n) if is_coro(arg) && is_coro(param) => {
                let _ = n;
                true
            }
            _ => false,
        }
    }

    /// True when two Named type strings refer to the same type/enum after
    /// typedb suffix / short-name canonicalization (B007).
    fn named_types_equivalent(&self, a: &str, b: &str) -> bool {
        if a == b {
            return true;
        }
        self.scope.canonicalize_type_name(a) == self.scope.canonicalize_type_name(b)
    }

    /// After arity is known OK for a unique external overload, walk args and
    /// emit `ArgTypeMismatch` for primitive mismatches and distinct Named
    /// types (enums). Conservative skips:
    /// - error-typed args
    /// - incomplete/empty param types
    /// - non-primitive / non-Named cores (e.g. generics) — leave silent
    /// - multi-overload / arity-ambiguous sets (caller must pass a unique sig)
    ///
    /// Does **not** coerce distinct int-backed enums to each other.
    fn walk_args_and_check_external_param_types(
        &mut self,
        display_name: &str,
        args: &[CallArg],
        overload: &OverloadSig,
    ) {
        let mut next_positional = 0usize;
        for arg in args {
            let named = arg.name.as_ref().map(|n| n.text(self.source));
            match call_site::bind_arg(named, &overload.param_names, &mut next_positional) {
                ArgBind::UnknownName => {
                    // Unknown names stay conservative; still visit the
                    // value so nested diagnostics are retained.
                    let _ = self.expr_type(&arg.value);
                    continue;
                }
                ArgBind::Index(param_index) => {
                    let arg_ty = self.expr_type(&arg.value);
                    let Some(param_text) = overload.param_types.get(param_index) else {
                        continue;
                    };
                    let param_ty = TypeRepr::parse_type_string(param_text.trim());
                    if matches!(param_ty, TypeRepr::Error(_)) {
                        continue;
                    }
                    if matches!(arg_ty, TypeRepr::Error(_)) {
                        continue;
                    }

                    let arg_core = Self::peel_const_handle(&arg_ty);
                    let param_core = Self::peel_const_handle(&param_ty);

                    // AngelScript `?` is the any-type placeholder (`tostring`).
                    // Generic array/dictionary methods use unsubstituted `T`
                    // in typedb — do not ArgTypeMismatch against the
                    // placeholder name (better-totd InsertLast/Find FPs).
                    if Self::is_unsubstituted_generic_or_any(param_core) {
                        continue;
                    }
                    // Function-pointer decay: bare functions (`Funcdef`) and
                    // engine funcdefs convert to CoroutineFunc* handles.
                    if Self::funcdef_converts_to_coroutine(arg_core, param_core) {
                        continue;
                    }

                    match (arg_core, param_core) {
                        (TypeRepr::Primitive(arg_p), TypeRepr::Primitive(param_p)) => {
                            if !is_convertible(arg_core, param_core) {
                                self.diagnostics.push(TypeDiagnostic {
                                    span: arg.value.span,
                                    kind: TypeDiagnosticKind::ArgTypeMismatch {
                                        function_name: display_name.to_string(),
                                        param_index,
                                        expected: param_p.as_str().to_string(),
                                        got: arg_p.as_str().to_string(),
                                    },
                                });
                            }
                        }
                        (TypeRepr::Named(arg_n), TypeRepr::Named(param_n)) => {
                            // GH #22: subclass → base is convertible in
                            // AngelScript for handle args. When the typedb
                            // inheritance chain links them, accept silently.
                            // Enums have no typedb parent chain, so B007
                            // (distinct enums stay non-convertible) is intact.
                            let is_unknown_workspace_arg = !arg_n.contains("::")
                                && !self.scope.is_external_type(arg_n)
                                && !self.scope.has_enum(arg_n);
                            if !self.named_types_equivalent(arg_n, param_n)
                                && !self.scope.is_external_derived_from(arg_n, param_n)
                                // GH #23: an `auto` (or workspace-local class)
                                // the typedb can't see has no ancestry to
                                // compare — skip rather than FP. Qualified
                                // names (`A::B`) are never skipped: that's
                                // how class-nested enum args appear, and
                                // B007 must keep firing on cross-enum args.
                                && !is_unknown_workspace_arg
                            {
                                self.diagnostics.push(TypeDiagnostic {
                                    span: arg.value.span,
                                    kind: TypeDiagnosticKind::ArgTypeMismatch {
                                        function_name: display_name.to_string(),
                                        param_index,
                                        expected: param_n.clone(),
                                        got: arg_n.clone(),
                                    },
                                });
                            }
                        }
                        // Mixed / unknown shapes: stay silent (conservative).
                        _ => {}
                    }
                }
            }
        }
    }

    /// Pick the unique external overload whose arity accepts `argc`, then
    /// run Named/primitive arg type checks. Multi-match or no-match stays
    /// silent on types (arity diagnostics are handled separately).
    fn check_external_call_arg_types(
        &mut self,
        display_name: &str,
        args: &[CallArg],
        overloads: &[OverloadSig],
    ) {
        if let Some(sig) = call_site::unique_overload_for_argc(overloads, args.len()) {
            self.walk_args_and_check_external_param_types(display_name, args, sig);
        } else {
            self.walk_args(args);
        }
    }

    /// Walk each argument expression and, for primitive-typed args whose
    /// corresponding declared parameter type is also a primitive, emit an
    /// `ArgTypeMismatch` when they differ. Non-primitive arg types, unknown
    /// param types (non-primitive text), and error types are all silently
    /// skipped — this is deliberately conservative, mirroring
    /// `ReturnTypeMismatch`'s primitive-only strategy.
    ///
    /// Binding rules (AngelScript):
    /// - Unnamed (positional) args bind left-to-right to successive parameters.
    /// - Named args (`name: value`) bind to the matching parameter by name and
    ///   may skip optional/defaulted parameters.
    ///
    /// Walks each arg exactly once so callers must NOT pre-walk.
    fn walk_args_and_check_types(
        &mut self,
        display_name: &str,
        args: &[CallArg],
        params: &[(String, String)],
    ) {
        let mut next_positional = 0usize;
        for arg in args {
            let named = arg.name.as_ref().map(|n| n.text(self.source));
            match call_site::bind_arg_workspace(named, params, &mut next_positional) {
                ArgBind::UnknownName => {
                    // Unknown parameter name — still type the value for
                    // nested diagnostics, but don't emit ArgTypeMismatch.
                    let _ = self.expr_type(&arg.value);
                    continue;
                }
                ArgBind::Index(param_index) => {
                    let arg_ty = self.expr_type(&arg.value);
                    let Some((_, param_text)) = params.get(param_index) else {
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
                            span: arg.value.span,
                            kind: TypeDiagnosticKind::ArgTypeMismatch {
                                function_name: display_name.to_string(),
                                param_index,
                                expected: param_p.as_str().to_string(),
                                got: arg_p.as_str().to_string(),
                            },
                        });
                    }
                }
            }
        }
    }

    /// Derive the type of a call expression's result. Takes the raw `args`
    /// slice and is responsible for walking each arg expression exactly
    /// once via `expr_type`. Callers must NOT pre-walk `args`.
    fn call_type(&mut self, callee: &Expr, args: &[CallArg]) -> TypeRepr {
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
                if self
                    .current_class()
                    .and_then(|cls| self.scope.workspace_class_member(&cls.name, &name))
                    .is_some()
                {
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
                if let Some(resolved) = self.scope.resolve_unqualified(&name) {
                    self.walk_args(args);
                    return TypeRepr::Named(resolved);
                }
                if let Some(enum_name) = self.scope.external_enum_by_short_name(&name) {
                    self.walk_args(args);
                    return TypeRepr::Named(enum_name);
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
                // Bare unresolved calls inside a mixin class body can be
                // requirements that the consuming class provides.
                if self.current_class().is_some_and(|cls| cls.is_mixin) {
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
                if obj_ty.is_error() {
                    self.walk_args(args);
                    return TypeRepr::Error(String::new());
                }
                let member_name = member.text(self.source).to_string();
                let Some(type_name) = Self::base_type_name(&obj_ty) else {
                    self.walk_args(args);
                    return TypeRepr::Error(String::new());
                };
                // B003: external method arity (unique + multi with no match).
                // B007: after arity OK, type-check Named/primitive args against
                // the unique matching overload's param types (incl. Nadeo).
                let overloads = self.scope.callables_method(&type_name, &member_name);
                if !overloads.is_empty() {
                    let ranges = call_site::arity_ranges(&overloads);
                    self.check_arity_against_ranges(&member_name, &ranges, args.len(), callee.span);
                    self.check_external_call_arg_types(&member_name, args, &overloads);
                } else if let Some(ranges) = self
                    .scope
                    .lookup_external_method_arity_ranges(&type_name, &member_name)
                {
                    self.check_arity_against_ranges(&member_name, &ranges, args.len(), callee.span);
                    self.walk_args(args);
                } else {
                    self.walk_args(args);
                }
                // AC19: when the receiver is a const object, a non-const
                // method's return value inherits `Const(_)` so that
                // downstream writes (`h.get_arr()[0] = 5`, etc.) fire
                // `ConstViolation`. A method declared with a trailing
                // `const` qualifier promises not to mutate `this` — leave
                // its return type untouched. Only workspace-local methods
                // have reliable const metadata; external-type returns
                // pass through unwrapped (follow-up noted below).
                let receiver_const = Self::receiver_is_const(&obj_ty);
                let method_is_const = self
                    .file_const_methods
                    .contains(&(type_name.clone(), member_name.clone()));
                // Same-file workspace classes: prefer `file_classes` so
                // we get the resolved return type AND know whether this
                // was a method (vs. a field) for const-propagation.
                if let Some((_, members)) = self.file_classes.get(&type_name) {
                    for (mname, t) in members {
                        if mname == &member_name {
                            let ret = t.clone();
                            if receiver_const && !method_is_const {
                                return Self::apply_receiver_const(ret, true);
                            }
                            return ret;
                        }
                    }
                }
                if let Some(t) = self.scope.lookup_method_return(&type_name, &member_name) {
                    // External types: no const-qualifier metadata yet, so
                    // don't over-fire — see AC19 deferred follow-up.
                    return t;
                }
                // Workspace-local class: any member is fine — silence.
                for cls in &self.class_stack {
                    if cls.name == type_name && cls.members.iter().any(|(n, _)| n == &member_name) {
                        return TypeRepr::Error(String::new());
                    }
                }
                // Cross-file inherited method: walk the workspace class
                // hierarchy so an inherited method's real return type
                // (iter 28) flows into downstream arg-type checks.
                if let Some(t) = self.scope.workspace_class_member(&type_name, &member_name) {
                    return t;
                }
                if !self.scope.is_external_type(&type_name) {
                    return TypeRepr::Error(String::new());
                }
                // Same Nadeo completeness rule as `member_access_type` (B006):
                // suppress only when the typedb member list is empty.
                if self.scope.is_nadeo_type(&type_name)
                    && !self.scope.nadeo_member_list_trusted(&type_name)
                {
                    return TypeRepr::Error(String::new());
                }
                // GH #21: `MwAddRef` / `MwRelease` are AngelScript builtins
                // on every CMwNod-derived type; the typedb never declares
                // them. Mirror of the gate in `member_access_type`.
                if matches!(member_name.as_str(), "MwAddRef" | "MwRelease")
                    && self.scope.is_external_derived_from(&type_name, "CMwNod")
                {
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
                let display = qual.rsplit("::").next().unwrap_or(qual.as_str());
                if let Some(t) = self.scope.lookup_function_return(&qual) {
                    // Workspace free functions: reuse the shared resolver
                    // (arg-count + overload path). External-only names fall
                    // through its 0-overload branch which checks typedb arity.
                    if !self.scope.lookup_function_overloads(&qual).is_empty() {
                        return self.resolve_workspace_function_call(
                            display,
                            &qual,
                            args,
                            callee.span,
                            t,
                        );
                    }
                    if let Some(overloads) =
                        self.scope.lookup_external_function_param_overloads(&qual)
                    {
                        let ranges: Vec<(usize, usize)> = overloads
                            .iter()
                            .map(|s| (s.min_args, s.param_types.len()))
                            .collect();
                        self.check_arity_against_ranges(display, &ranges, args.len(), callee.span);
                        self.check_external_call_arg_types(display, args, &overloads);
                        return t;
                    }
                    if let Some(ranges) = self.scope.lookup_external_function_arity_ranges(&qual) {
                        self.check_arity_against_ranges(display, &ranges, args.len(), callee.span);
                    }
                    self.walk_args(args);
                    return t;
                }
                self.walk_args(args);
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
        let ty = self.expr_type_inner(expr);
        if self.record_types {
            self.expr_types.insert(expr.span.start, ty.clone());
            self.span_expr_types
                .insert((expr.span.start, expr.span.end), ty.clone());
        }
        ty
    }

    fn expr_type_inner(&mut self, expr: &Expr) -> TypeRepr {
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
                if let Some(ty) = self
                    .current_class()
                    .and_then(|cls| self.scope.workspace_class_member(&cls.name, &name))
                {
                    return ty;
                }
                // 3. Namespace-scoped global: try progressively shorter
                //    namespace prefixes.  Inside Ns "Outer::Inner", try
                //    "Outer::Inner::name" first, then "Outer::name".
                for depth in (1..=self.namespace_stack.len()).rev() {
                    let ns = self.namespace_stack[..depth].join("::");
                    let qualified = format!("{}::{}", ns, name);
                    if let Some(ty) = self.scope.lookup_global_value_type(&qualified) {
                        return ty;
                    }
                    if self.scope.has_global_ident(&qualified) {
                        // Known name without a stored value type — silence
                        // rather than Named(varName) which false-positives
                        // ArgTypeMismatch against real param types.
                        return TypeRepr::Error(String::new());
                    }
                }
                // 4. Global top-level lookup (vars, functions-as-values, enums).
                if let Some(ty) = self.scope.lookup_global_value_type(&name) {
                    return ty;
                }
                if self.scope.has_global_ident(&name) {
                    return TypeRepr::Error(String::new());
                }
                // 5. AngelScript / Openplanet hardcoded builtins — silent.
                if builtins::is_builtin_type(&name) || builtins::is_builtin_global(&name) {
                    return TypeRepr::Error(String::new());
                }
                // Bare unresolved idents inside a mixin class body can be
                // members the *consuming* class provides (GH #46: mixin's
                // `tabs.Length` where `tabs` is declared by the class that
                // mixes it in). The game compiler checks mixin bodies in
                // the consumer's context, so stay silent here — same policy
                // as unresolved bare calls inside a mixin (above).
                if self.current_class().is_some_and(|cls| cls.is_mixin) {
                    return TypeRepr::Error(String::new());
                }
                // 6. Undefined.
                self.diagnostics.push(TypeDiagnostic {
                    span: expr.span,
                    kind: TypeDiagnosticKind::UndefinedIdentifier(name.clone()),
                });
                TypeRepr::Error(name)
            }
            ExprKind::Binary { lhs, rhs, op } => {
                // GH #37 slice 1 (tightened, game-verified RemoteBuild
                // granularity probes 2026-08-17): `Signed/Unsigned mismatch`
                // fires in exactly two situations:
                //
                //  a) A relational op `<`/`<=`/`>`/`>=` (NOT `==`/`!=`)
                //     directly compares a pure signed int with a pure
                //     unsigned int, and neither operand subtree contains an
                //     integer literal (`n > 0` and `0 < n` are silent).
                //     Warning span covers the whole comparison.
                //  b) A `+` add directly mixes a pure signed int with a pure
                //     unsigned int with NO integer literal in either direct
                //     operand and no *chained* arithmetic sibling
                //     (`(i + u) * 2` and `(i + 1) + u` are silent — only the
                //     topmost mix warns). The warning lives at the `+`
                //     regardless of any outer comparison partner
                //     (`i + u < j` warns; `(i + u) > 0` warns at the `+`,
                //     not the `>`). `-`, `*`, `&` mixes are exempt.
                //
                // Peel Const; stay silent on Error/unknown/non-primitive.
                // The mix check runs BEFORE walking operands so a chained
                // arith sibling can suppress the inner add — `expr_type`
                // recursion is bottom-up and couldn't see the enclosing op.
                // `signedness_of` uses a diagnostics checkpoint so the
                // probe walk never double-emits; the real walk happens once
                // at the bottom of this arm.
                let signedness_of = |ck: &mut Self, e: &Expr| -> Option<bool> {
                    // Some(true) = signed, Some(false) = unsigned.
                    let mark = ck.diagnostics.len();
                    let ty = ck.expr_type(e);
                    ck.diagnostics.truncate(mark);
                    match ty.unwrap_const() {
                        TypeRepr::Primitive(p) if is_signed_int(*p) => Some(true),
                        TypeRepr::Primitive(p) if is_unsigned_int(*p) => Some(false),
                        _ => None,
                    }
                };
                let operand_binary_or_lit = |e: &Expr| {
                    matches!(
                        e.kind,
                        ExprKind::Binary { .. } | ExprKind::IntLit(_) | ExprKind::HexLit(_)
                    )
                };
                let mut emit_span: Option<Span> = None;
                match op {
                    BinOp::Add
                        if self.arith_suppress_add_mix == 0
                            && !operand_binary_or_lit(lhs)
                            && !operand_binary_or_lit(rhs) =>
                    {
                        let ls = signedness_of(self, lhs);
                        let rs = signedness_of(self, rhs);
                        if let (Some(ls), Some(rs)) = (ls, rs) {
                            if ls != rs {
                                emit_span = Some(expr.span);
                            }
                        }
                    }
                    BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                        if !subtree_has_int_literal(lhs) && !subtree_has_int_literal(rhs) {
                            let ls = signedness_of(self, lhs);
                            let rs = signedness_of(self, rhs);
                            if let (Some(ls), Some(rs)) = (ls, rs) {
                                if ls != rs {
                                    emit_span = Some(expr.span);
                                }
                            }
                        }
                    }
                    _ => {}
                }
                if let Some(span) = emit_span {
                    self.diagnostics.push(TypeDiagnostic {
                        span,
                        kind: TypeDiagnosticKind::SignedUnsignedMismatch,
                    });
                }
                // Walk operands exactly once with diagnostics live — the
                // signedness probes above checkpointed and rolled back.
                // While inside a non-relational arithmetic op, suppress any
                // inner Add-mix (game: only the topmost mix warns).
                let suppress = !matches!(
                    op,
                    BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq | BinOp::Eq | BinOp::NotEq
                );
                if suppress {
                    self.arith_suppress_add_mix += 1;
                }
                let _ = self.expr_type(lhs);
                let _ = self.expr_type(rhs);
                if suppress {
                    self.arith_suppress_add_mix -= 1;
                }
                TypeRepr::Error(String::new())
            }
            ExprKind::Unary { op, expr } => {
                let operand_ty = self.expr_type(expr);
                // AngelScript: `!` is bool-only. Game rejects non-bool
                // operands with "Illegal operation on this datatype"
                // (OP 1.29.5; tm-control-mcp AsyncDispatch.as:136).
                // Skip Error operands (already diagnosed upstream) and
                // unknown/workspace types we can't classify confidently.
                if matches!(op, UnaryOp::Not) && !operand_ty.is_error() {
                    // Strip `const` (and handle) wrappers: `!` on a
                    // `const bool` constant is legal (E++ uses
                    // `!ENABLE_OLD_CHECK_PLACING_ITEM_HELPER`).
                    let legal = matches!(
                        operand_ty.unwrap_const(),
                        TypeRepr::Primitive(PrimitiveType::Bool)
                    );
                    if !legal {
                        self.diagnostics.push(TypeDiagnostic {
                            span: expr.span,
                            kind: TypeDiagnosticKind::IllegalUnaryOperand {
                                op: "!".into(),
                                operand_type: operand_ty.display(),
                            },
                        });
                    }
                    return TypeRepr::Primitive(PrimitiveType::Bool);
                }
                operand_ty
            }
            ExprKind::Postfix { expr, .. } => self.expr_type(expr),
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
                    if Self::receiver_is_const(&obj_ty) && !matches!(elem, TypeRepr::Const(_)) {
                        return TypeRepr::Const(Box::new(elem.clone()));
                    }
                    return elem.clone();
                }
                // Dictionary-like and everything else: stay silent.
                TypeRepr::Error(String::new())
            }
            ExprKind::Cast {
                target_type,
                expr: inner,
            } => {
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
                // GH #44: `@` handle-assign needs a handle *slot* on the
                // left. An index into typed handle arrays (`T@[]`) and
                // dictionaries is such a slot (game-verified legal), but an
                // index into a handle-to-value-type (`Json::Value@` — the
                // value type has no handle form) is not an l-value:
                //   `@arr[0] = x` → game: "Expression is not an l-value".
                // Detect by base type name so we don't depend on how the
                // index expression itself typed (it silently yields Error).
                if let ExprKind::Index { object, .. } = &lhs.kind {
                    let obj_ty = self.expr_type(object);
                    let is_value_type_handle = matches!(&obj_ty, TypeRepr::Handle(_))
                        && Self::base_type_name(&obj_ty).is_some_and(|n| {
                            n == "Json::Value" || PrimitiveType::from_name(&n).is_some()
                        });
                    if is_value_type_handle {
                        self.diagnostics.push(TypeDiagnostic {
                            span: lhs.span,
                            kind: TypeDiagnosticKind::InvalidAssignmentTarget,
                        });
                        let _ = self.expr_type(rhs);
                        return TypeRepr::Error(String::new());
                    }
                }
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
                            detail: "left-hand side of @= is not a handle type".to_string(),
                        },
                    });
                }
                // RHS check: only fire when clearly not handle/null.
                // Accept Handle, Null, Error, Named (ambiguous).
                if matches!(rhs_ty, TypeRepr::Primitive(_) | TypeRepr::Void) {
                    self.diagnostics.push(TypeDiagnostic {
                        span: rhs.span,
                        kind: TypeDiagnosticKind::HandleValueMismatch {
                            detail: "right-hand side of @= must be a handle or null".to_string(),
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
                self.function_frame_starts.push(self.frames.len());
                self.push_frame();
                for p in params {
                    let ty = self.resolve_type_expr(&p.type_expr);
                    if let Some(name) = &p.name {
                        self.define_local(name.text(self.source).to_string(), ty, name.span);
                    }
                }
                self.check_function_body(body);
                self.pop_frame();
                self.function_frame_starts.pop();
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

/// True if `p` is any integer primitive (signed or unsigned).
fn is_integer(p: PrimitiveType) -> bool {
    is_signed_int(p) || is_unsigned_int(p)
}

/// True if the expression subtree contains an integer literal anywhere
/// (`IntLit` / `HexLit`). Used by the `Signed/Unsigned mismatch` rule:
/// the game compiler stays silent whenever either compared subtree is
/// "literal-tainted" (RemoteBuild probe 2026-08-17: `n > 0`, `0 < n`,
/// `arr.Length > 0` all silent).
fn subtree_has_int_literal(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::IntLit(_) | ExprKind::HexLit(_) => true,
        ExprKind::Binary { lhs, rhs, .. } => {
            subtree_has_int_literal(lhs) || subtree_has_int_literal(rhs)
        }
        ExprKind::Unary { expr, .. } | ExprKind::Postfix { expr, .. } => {
            subtree_has_int_literal(expr)
        }
        ExprKind::Call { callee, args } => {
            subtree_has_int_literal(callee)
                || args.iter().any(|a| subtree_has_int_literal(&a.value))
        }
        ExprKind::Member { object, .. } => subtree_has_int_literal(object),
        ExprKind::Index { object, index } => {
            subtree_has_int_literal(object) || subtree_has_int_literal(index)
        }
        ExprKind::Cast { expr, .. } => subtree_has_int_literal(expr),
        ExprKind::TypeConstruct { args, .. } => args.iter().any(subtree_has_int_literal),
        ExprKind::Is { expr, .. } => subtree_has_int_literal(expr),
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            subtree_has_int_literal(condition)
                || subtree_has_int_literal(then_expr)
                || subtree_has_int_literal(else_expr)
        }
        ExprKind::Assign { lhs, rhs, .. } => {
            subtree_has_int_literal(lhs) || subtree_has_int_literal(rhs)
        }
        _ => false,
    }
}

/// True if `p` is a signed integer primitive.
fn is_signed_int(p: PrimitiveType) -> bool {
    matches!(
        p,
        PrimitiveType::Int8 | PrimitiveType::Int16 | PrimitiveType::Int | PrimitiveType::Int64
    )
}

/// True if `p` is an unsigned integer primitive.
fn is_unsigned_int(p: PrimitiveType) -> bool {
    matches!(
        p,
        PrimitiveType::Uint8 | PrimitiveType::Uint16 | PrimitiveType::Uint | PrimitiveType::Uint64
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
fn resolve_overload<'a>(overloads: &'a [OverloadSig], arg_tys: &[TypeRepr]) -> OverloadMatch<'a> {
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

/// True when `ty` is bare `string` or `const string` (by value), not a
/// reference/handle/array/template wrapping of string.
fn is_string_by_value_type(ty: &TypeExpr) -> bool {
    match &ty.kind {
        TypeExprKind::Primitive(TokenKind::KwString) => true,
        TypeExprKind::Const(inner) => is_string_by_value_type(inner),
        _ => false,
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

    /// Like `check`, but loads the Openplanet Core + Nadeo fixtures so
    /// external method/free-function arity is exercised (B003).
    fn check_with_typedb(source: &str) -> Vec<TypeDiagnostic> {
        use crate::typedb::TypeIndex;
        let cp = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetCore.json");
        let np = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetNext.json");
        let idx = TypeIndex::load(&cp, &np).expect("typedb fixtures must load");
        let tokens = tokenize_filtered(source);
        let mut parser = Parser::new(&tokens, source);
        let file = parser.parse_file();
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        let syms = SymbolTable::extract_symbols(fid, source, &file);
        ws.set_file_symbols(fid, syms);
        let scope = GlobalScope::new(&ws, Some(&idx));
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

    /// GH #26: qualified unknown types with a non-engine leading segment get
    /// a plugin-export note appended to the message; bare names and
    /// engine-namespace names keep the plain message.
    #[test]
    fn unknown_type_export_note_heuristics() {
        // Bare name → plain message, no note.
        let bare = TypeDiagnosticKind::UnknownType("NotAType".into());
        assert_eq!(bare_message(&bare), "unknown type `NotAType`");

        // Engine namespace (Core API + Nadeo groups) → plain message.
        for engine in ["Math::NotAThing", "Game::CNope", "TrackMania::Nope"] {
            let k = TypeDiagnosticKind::UnknownType(engine.into());
            assert_eq!(bare_message(&k), format!("unknown type `{engine}`"));
        }

        // Non-engine qualified name → note mentions the plugin-export cause.
        let export_ns = TypeDiagnosticKind::UnknownType("MLFeed::PlayerCpInfo".into());
        let m = bare_message(&export_ns);
        assert!(m.starts_with("unknown type `MLFeed::PlayerCpInfo`"));
        assert!(m.contains("(note:"), "expected a note, got {m:?}");
        assert!(
            m.contains("plugin export"),
            "expected export hint, got {m:?}"
        );
        assert!(
            m.contains("--plugins-dir"),
            "expected --plugins-dir hint, got {m:?}"
        );
        // Single line — CLI/TUI render one line per diagnostic.
        assert!(!m.contains('\n'));
    }

    fn bare_message(kind: &TypeDiagnosticKind) -> String {
        TypeDiagnostic {
            span: Span::new(0, 0),
            kind: kind.clone(),
        }
        .message()
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

    /// GH #34: a method overload inherited from a parent class must count
    /// toward arity on a NAMESPACE-QUALIFIED call `Ns::Child::Method(2 args)`,
    /// which resolves to the parent's 2-arg overload even though the child
    /// only declares a 3-arg one. (Member-access `c.Method(..)` is already
    /// lenient; the qualified path was not.)
    #[test]
    fn inherited_method_overload_counts_for_arity() {
        let src = r#"
namespace MLFeed {
    class Base {
        void UpdateFrom(int a, int b) { }
    }
    class Child : Base {
        void UpdateFrom(int a, int b, bool c) { }
    }
}
void Main() {
    MLFeed::Child::UpdateFrom(1, 2);   // resolves to Base::UpdateFrom(int,int) — legal
}
"#;
        let diags = check(src);
        let arity: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert_eq!(
            arity.len(),
            0,
            "inherited 2-arg overload must satisfy the qualified 2-arg call; got {:?}",
            diags
        );
    }

    /// GH #34 follow-up (review Arm B): a call that exceeds EVERY overload's
    /// arity (own + inherited) must still diagnose. The augmented overload set
    /// must not silently swallow a genuinely-wrong call the pre-#34 single-
    /// overload path caught.
    #[test]
    fn inherited_overload_exceeding_all_arity_still_diagnoses() {
        let src = r#"
namespace MLFeed {
    class Base {
        void UpdateFrom(int a, int b) { }
    }
    class Child : Base {
        void UpdateFrom(int a, int b, bool c) { }
    }
}
void Main() {
    MLFeed::Child::UpdateFrom(1, 2, true, 99);   // 4 args: exceeds 2-arg AND 3-arg
}
"#;
        let diags = check(src);
        let arity: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert_eq!(
            arity.len(),
            1,
            "call exceeding all overloads (own + inherited) must diagnose; got {:?}",
            diags
        );
    }

    /// Control: a call matching ANY overload (incl. inherited) stays silent —
    /// the exceeds-all fix must not introduce false positives on valid calls.
    #[test]
    fn inherited_overload_matching_any_stays_silent() {
        let src = r#"
namespace MLFeed {
    class Base {
        void UpdateFrom(int a, int b) { }
    }
    class Child : Base {
        void UpdateFrom(int a, int b, bool c) { }
    }
}
void Main() {
    MLFeed::Child::UpdateFrom(1, 2);            // matches Base 2-arg
    MLFeed::Child::UpdateFrom(1, 2, true);      // matches Child 3-arg
}
"#;
        let diags = check(src);
        let arity: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert_eq!(
            arity.len(),
            0,
            "calls matching any overload must not be flagged; got {:?}",
            diags
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
        let diags = check("namespace Ns { class Foo {} void f() { Foo@ x; } }");
        assert!(diags.is_empty(), "expected no diagnostics, got {:?}", diags);
    }

    #[test]
    fn member_access_workspace_class_silenced() {
        // Using `this.field` inside a method resolves through
        // `base_type_name(this) → current class` + the in-memory
        // ClassCtx members, so `this.x` should not emit a diagnostic.
        let diags = check("class C { int x; void f() { int y = this.x; } }");
        assert!(diags.is_empty(), "expected no diagnostics, got {:?}", diags);
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
    fn property_setter_has_implicit_value_local() {
        let source = r#"
            bool flag {
                get { return false; }
                set { bool x = value; }
            }
        "#;
        let diags = check(source);
        let undef: Vec<_> = diags
            .iter()
            .filter(
                |d| matches!(&d.kind, TypeDiagnosticKind::UndefinedIdentifier(n) if n == "value"),
            )
            .collect();
        assert!(
            undef.is_empty(),
            "expected no undefined-ident for setter value, got {:?}",
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
        assert_eq!(
            missing.len(),
            1,
            "expected 1 MissingReturn, got {:?}",
            diags
        );
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
        assert!(
            missing.is_empty(),
            "expected no MissingReturn, got {:?}",
            diags
        );
    }

    #[test]
    fn void_function_without_return_ok() {
        let diags = check("void f() { }");
        let missing: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::MissingReturn { .. }))
            .collect();
        assert!(
            missing.is_empty(),
            "expected no MissingReturn, got {:?}",
            diags
        );
    }

    #[test]
    fn return_in_single_branch_if_does_not_suppress() {
        let diags = check("int f() { if (true) { return 1; } }");
        let missing: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::MissingReturn { .. }))
            .collect();
        assert_eq!(
            missing.len(),
            1,
            "expected 1 MissingReturn, got {:?}",
            diags
        );
    }

    #[test]
    fn try_catch_with_returns_in_both_branches_suppresses_missing_return() {
        let diags = check("int f() { try { return 1; } catch { return 2; } }");
        let missing: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::MissingReturn { .. }))
            .collect();
        assert!(
            missing.is_empty(),
            "expected no MissingReturn for try/catch with returns, got {:?}",
            diags
        );
    }

    #[test]
    fn switch_with_fallthrough_labels_and_default_terminates() {
        let diags =
            check("int f(int x) { switch (x) { case 0: case 1: return 1; default: return 2; } }");
        let missing: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::MissingReturn { .. }))
            .collect();
        assert!(
            missing.is_empty(),
            "expected no MissingReturn for fallthrough switch returns, got {:?}",
            diags
        );
    }

    #[test]
    fn switch_with_non_terminating_middle_case_still_reports_missing_return() {
        let diags = check(
            "int f(int x) { switch (x) { case 0: return 1; case 1: break; default: return 2; } }",
        );
        let missing: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::MissingReturn { .. }))
            .collect();
        assert_eq!(
            missing.len(),
            1,
            "expected MissingReturn when a switch case can break out, got {:?}",
            diags
        );
    }

    #[test]
    fn arg_count_match_ok() {
        let diags = check("void f(int a, int b) {} void main() { f(1, 2); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgCountMismatch, got {:?}",
            diags
        );
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
        assert!(
            bad.is_empty(),
            "expected no ArgCountMismatch, got {:?}",
            diags
        );
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
            .filter(
                |d| matches!(&d.kind, TypeDiagnosticKind::UndefinedIdentifier(n) if n == "super"),
            )
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
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch, got {:?}",
            diags
        );
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

    /// B001: named args must bind by parameter name, not position.
    /// `AddIndentedTooltip("x", w: 20.0)` skips the defaulted `pushFont`
    /// bool and should not produce ArgTypeMismatch(expected bool, got float).
    #[test]
    fn named_arg_skips_defaulted_param_ok() {
        let diags = check(
            r#"
            void AddIndentedTooltip(const string &in msg, bool pushFont = false, float w = -1.0) {}
            void Main() { AddIndentedTooltip("x", w: 20.0); }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "B001: named arg w: should bind to float param, got {:?}",
            diags
        );
    }

    /// B001: named arg with wrong type still errors against the named param.
    #[test]
    fn named_arg_type_mismatch_still_fires() {
        let diags = check(
            r#"
            void AddIndentedTooltip(const string &in msg, bool pushFont = false, float w = -1.0) {}
            void Main() { AddIndentedTooltip("x", w: true); }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ArgTypeMismatch for wrong named-arg type, got {:?}",
            diags
        );
        assert_eq!(
            bad[0].kind,
            TypeDiagnosticKind::ArgTypeMismatch {
                function_name: "AddIndentedTooltip".into(),
                param_index: 2,
                expected: "float".into(),
                got: "bool".into(),
            }
        );
    }

    /// B001: pure positional binding still works after CallArg change.
    #[test]
    fn named_arg_all_positional_still_checked() {
        let diags = check(
            r#"
            void AddIndentedTooltip(const string &in msg, bool pushFont = false, float w = -1.0) {}
            void Main() { AddIndentedTooltip("x", 20.0); }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "positional float into bool should still mismatch, got {:?}",
            diags
        );
        assert_eq!(
            bad[0].kind,
            TypeDiagnosticKind::ArgTypeMismatch {
                function_name: "AddIndentedTooltip".into(),
                param_index: 1,
                expected: "bool".into(),
                got: "float".into(),
            }
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
        assert_eq!(bad.len(), 1, "expected 1 ConstViolation, got {:?}", diags);
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
        let diags = check("class C { const int x; } void f() { C@ c; c.x = 6; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .collect();
        assert_eq!(bad.len(), 1, "expected 1 ConstViolation, got {:?}", diags);
    }

    #[test]
    fn const_compound_assign_fires() {
        let diags = check("void f() { const int x = 1; x += 2; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .collect();
        assert_eq!(bad.len(), 1, "expected 1 ConstViolation, got {:?}", diags);
    }

    #[test]
    fn handle_assign_to_const_fires() {
        // `C@ const a` — the handle itself is const, so `@a = null`
        // reassigning it must fire a ConstViolation. (Iter 38 / AC20
        // distinguishes this from `const C@` where the pointee is const
        // but the handle is mutable; see
        // `const_handle_not_const_contents` and
        // `handle_assign_to_handle_const_fires` below.)
        let diags = check("class C {} void f() { C@ const a = null; @a = null; }");
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
    fn const_handle_not_const_contents() {
        // AC20: `Foo@ const h` — the handle itself is const, but the
        // pointee is mutable. Assigning to a field through it must NOT
        // fire ConstViolation. Iter 32 dropped this test because the
        // parser collapsed both orderings; iter 38 reinstates it.
        let diags =
            check("class Foo { int field; } void f() { Foo@ const h = null; h.field = 5; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ConstViolation for `Foo@ const` field write, got {:?}",
            diags
        );
    }

    #[test]
    fn const_pointee_fires_on_field_write() {
        // AC20 dual: `const Foo@ h` — the pointee is const, the handle is
        // mutable. `h.field = 5` MUST fire ConstViolation.
        let diags =
            check("class Foo { int field; } void f() { const Foo@ h = null; h.field = 5; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ConstViolation for `const Foo@` field write, got {:?}",
            diags
        );
    }

    #[test]
    fn const_array_element_assign_fires() {
        // `const array<int>@ arr; arr[0] = 5;` — the receiver is const,
        // so indexing returns a `Const(int)` lvalue. Assigning into it
        // must fire a ConstViolation (iter 32).
        let diags = check("void f() { const array<int>@ arr = null; arr[0] = 5; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .collect();
        assert_eq!(bad.len(), 1, "expected 1 ConstViolation, got {:?}", diags);
    }

    #[test]
    fn const_array_element_read_is_fine() {
        // Pure reads of `const array<int>@ arr; int x = arr[0];` must
        // NOT fire ConstViolation — only assignment through the const
        // element should. (Iter 32 wraps the read in `Const(int)`, but
        // iter 24's const check only looks at assignment LHS.)
        let diags = check("void f() { const array<int>@ arr = null; int x = arr[0]; }");
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
        let diags =
            check("class Foo { int field; } void f() { const Foo@ x = null; x.field = 5; }");
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
    fn const_receiver_method_return_is_const() {
        // AC19: non-const method on a const receiver → return type
        // inherits `Const`. The method returns a mutable `int[]@`, but
        // because the receiver is `const Foo@`, the array handle's
        // contents become const, so `arr[0] = 5` must fire.
        let src = "class Foo { array<int>@ arr; array<int>@ get_arr() { return arr; } } \
                   void f() { const Foo@ h = null; h.get_arr()[0] = 5; }";
        let diags = check(src);
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ConstViolation from non-const method on const receiver, got {:?}",
            diags
        );
    }

    #[test]
    fn const_receiver_const_method_return_not_const() {
        // AC19: a method declared `const` promises not to mutate `this`,
        // so its return type does NOT inherit `Const` even when the
        // receiver is const. `arr[0] = 5` must NOT fire.
        let src = "class Foo { array<int>@ arr; array<int>@ get_arr() const { return arr; } } \
                   void f() { const Foo@ h = null; h.get_arr()[0] = 5; }";
        let diags = check(src);
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ConstViolation for const method on const receiver, got {:?}",
            diags
        );
    }

    #[test]
    fn non_const_receiver_method_return_not_const() {
        // AC19 sanity: with a mutable receiver, a non-const method's
        // return type stays unwrapped regardless.
        let src = "class Foo { array<int>@ arr; array<int>@ get_arr() { return arr; } } \
                   void f() { Foo@ h = null; h.get_arr()[0] = 5; }";
        let diags = check(src);
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ConstViolation on mutable receiver, got {:?}",
            diags
        );
    }

    #[test]
    fn non_const_member_receiver_not_const() {
        // Non-const receiver: `Foo f; f.field = 5;` must NOT fire.
        let diags = check("class Foo { int field; } void f() { Foo x; x.field = 5; }");
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
        let diags = check("void f(int a) {} void f(string a) {} void main() { f(1); }");
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
        let diags = check("void f(float a) {} void f(string a) {} void main() { f(1); }");
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
        let diags = check("void f(int a) {} void f(bool a) {} void main() { f(\"hi\"); }");
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
        let diags =
            check("void f(int a, float b) {} void f(float a, int b) {} void main() { f(1, 1); }");
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
        let diags = check("void f(int a) {} void f(int a, int b) {} void main() { f(1); }");
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
        let main = "class Foo : Base {} void use() { Foo f; int y = f.base_method(); }";
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
        let main = "class Foo : Base { string shared; } void use() { Foo f; string y = f.shared; }";
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
    fn method_uses_namespaced_inherited_method_cross_file() {
        let base =
            "namespace Editor { class NetworkSerializable { int get_count() { return 0; } } }";
        let main = "namespace Editor { class Child : NetworkSerializable { int wrap() { return get_count(); } } }";
        let diags = check_workspace(main, &[base]);
        let undef: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedIdentifier(_)))
            .collect();
        assert!(
            undef.is_empty(),
            "expected no UndefinedIdentifier on namespaced inherited method call, got {:?}",
            diags
        );
    }

    #[test]
    fn method_uses_member_from_second_base_same_file() {
        let diags = check(
            "class BaseA {} class BaseB { int get_count() { return 0; } } class Child : BaseA, BaseB { int wrap() { return get_count(); } }",
        );
        let undef: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedIdentifier(_)))
            .collect();
        assert!(
            undef.is_empty(),
            "expected no UndefinedIdentifier from second base, got {:?}",
            diags
        );
    }

    #[test]
    fn unresolved_bare_call_inside_mixin_class_is_silent() {
        let diags = check("mixin class RequiresHook { void Run() { AfterLoadedState(); } }");
        let undef: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedIdentifier(n) if n == "AfterLoadedState"))
            .collect();
        assert!(
            undef.is_empty(),
            "expected no UndefinedIdentifier for mixin requirement call, got {:?}",
            diags
        );
    }

    #[test]
    fn unresolved_bare_ident_inside_mixin_class_is_silent() {
        // GH #46: a mixin class references a member its consuming class
        // declares (here `tabs`, declared by the class that mixes in
        // `HasGroupMeta`). The game compiler accepts this — mixin bodies are
        // checked in the context of the consuming class.
        let diags = check(
            "class Tab { } \
             class TabGroup : HasGroupMeta { Tab@[] tabs; } \
             mixin class HasGroupMeta { \
                 bool Empty() { if (tabs.Length == 0) return true; return false; } \
             }",
        );
        let undef: Vec<_> = diags
            .iter()
            .filter(
                |d| matches!(&d.kind, TypeDiagnosticKind::UndefinedIdentifier(n) if n == "tabs"),
            )
            .collect();
        assert!(
            undef.is_empty(),
            "expected no UndefinedIdentifier for mixin requirement ident `tabs`, got {:?}",
            diags
        );
    }

    #[test]
    fn unresolved_bare_ident_outside_mixin_class_still_diagnoses() {
        // Control: the same pattern in a non-mixin class must still flag.
        let diags = check(
            "class TabGroup { bool Empty() { if (tabs.Length == 0) return true; return false; } }",
        );
        let undef: Vec<_> = diags
            .iter()
            .filter(
                |d| matches!(&d.kind, TypeDiagnosticKind::UndefinedIdentifier(n) if n == "tabs"),
            )
            .collect();
        assert_eq!(
            undef.len(),
            1,
            "expected exactly 1 UndefinedIdentifier for `tabs` in a plain class, got {:?}",
            diags
        );
    }

    #[test]
    fn constructor_uses_inherited_method_cross_file_with_namespace() {
        let base = "namespace Editor { class NetworkSerializable { NetworkSerializable@ ReadFromNetworkBuffer(MemoryBuffer@ buf) { return this; } } class ItemSpec : NetworkSerializable {} }";
        let main = "namespace Editor { class ItemSpecPriv : ItemSpec { ItemSpecPriv(MemoryBuffer@ buf) { ReadFromNetworkBuffer(buf); } } }";
        let diags = check_workspace(main, &[base]);
        let undef: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedIdentifier(n) if n == "ReadFromNetworkBuffer"))
            .collect();
        assert!(
            undef.is_empty(),
            "expected no UndefinedIdentifier for inherited constructor call, got {:?}",
            diags
        );
    }

    #[test]
    fn constructor_uses_inherited_method_with_realistic_itemspec_shape() {
        let base = "namespace Editor { shared class NetworkSerializable { NetworkSerializable@ ReadFromNetworkBuffer(MemoryBuffer@ buf) { return this; } } shared class ItemSpec : NetworkSerializable { ItemSpec() {} ItemSpec(MemoryBuffer@ buf) { ReadFromNetworkBuffer(buf); } } }";
        let main = "namespace Editor { class ItemSpecPriv : ItemSpec { ItemSpecPriv() { super(); } ItemSpecPriv(MemoryBuffer@ buf) { super(); ReadFromNetworkBuffer(buf); } } }";
        let diags = check_workspace(main, &[base]);
        let undef: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedIdentifier(n) if n == "ReadFromNetworkBuffer"))
            .collect();
        assert!(
            undef.is_empty(),
            "expected no UndefinedIdentifier for realistic ItemSpec inheritance, got {:?}",
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
        let diags = check("void ti(int n) {} void f() { array<int> a; ti(a[0]); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch for int elem → int param, got {:?}",
            diags
        );

        let diags = check("void tb(bool b) {} void f() { array<int> a; tb(a[0]); }");
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
        let diags = check("void tu(uint n) {} void f() { array<int> a; tu(a.Length); }");
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
        let diags = check("void tu(uint n) {} void f() { array<int> a; tu(a.length); }");
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
        let diags = check("void ti(int n) {} void f() { int[] a; ti(a[0]); }");
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
        let diags = check("void f() { dictionary d; d.Set(\"k\", 1); }");
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

    // ── B003: external method / free-function arity ──────────────────────

    #[test]
    fn external_string_indexof_one_arg_ok() {
        let diags = check_with_typedb(
            r#"void f() { string head; int open = 0; int ix = head.IndexOf("name="); }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgCountMismatch for 1-arg IndexOf, got {:?}",
            diags
        );
    }

    #[test]
    fn external_string_indexof_two_arg_fires() {
        // Game rejects string::IndexOf(string, int) — only the 1-arg form exists.
        let diags = check_with_typedb(
            r#"void f() { string head; int open = 0; int ix = head.IndexOf("name=", open); }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ArgCountMismatch for 2-arg IndexOf, got {:?}",
            diags
        );
        assert_eq!(
            bad[0].kind,
            TypeDiagnosticKind::ArgCountMismatch {
                function_name: "IndexOf".into(),
                expected_min: 1,
                expected_max: 1,
                got: 2,
            }
        );
    }

    #[test]
    fn external_string_indexof_literal_receiver_two_arg_fires() {
        let diags = check_with_typedb(r#"void f() { int ix = "abc".IndexOf("a", 0); }"#);
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected ArgCountMismatch on string-literal IndexOf, got {:?}",
            diags
        );
    }

    #[test]
    fn external_string_substr_overloads_accept_one_and_two_args() {
        // SubStr has 1-arg and 2-arg overloads — both must stay silent.
        let diags = check_with_typedb(
            r#"void f() { string s; string a = s.SubStr(0); string b = s.SubStr(0, 1); }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgCountMismatch for SubStr overloads, got {:?}",
            diags
        );
    }

    #[test]
    fn external_string_substr_zero_args_fires() {
        let diags = check_with_typedb(r#"void f() { string s; string a = s.SubStr(); }"#);
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected ArgCountMismatch for 0-arg SubStr, got {:?}",
            diags
        );
        assert_eq!(
            bad[0].kind,
            TypeDiagnosticKind::ArgCountMismatch {
                function_name: "SubStr".into(),
                expected_min: 1,
                expected_max: 2,
                got: 0,
            }
        );
    }

    #[test]
    fn external_ui_selectable_valid_arity_ok() {
        // UI::Selectable(label, selected, flags = ...) — 2 or 3 args OK.
        let diags = check_with_typedb(
            r#"void f() { bool a = UI::Selectable("x", true); bool b = UI::Selectable("x", true, 0); }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgCountMismatch for valid Selectable arity, got {:?}",
            diags
        );
    }

    #[test]
    fn external_ui_selectable_four_arg_fires() {
        // B005 foundation: 4-arg Selectable does not exist in Core.
        let diags =
            check_with_typedb(r#"void f() { bool a = UI::Selectable("x", true, 0, false); }"#);
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected ArgCountMismatch for 4-arg Selectable, got {:?}",
            diags
        );
        assert_eq!(
            bad[0].kind,
            TypeDiagnosticKind::ArgCountMismatch {
                function_name: "Selectable".into(),
                expected_min: 2,
                expected_max: 3,
                got: 4,
            }
        );
    }

    #[test]
    fn external_free_named_args_bind_by_name() {
        let diags = check_with_typedb(
            r#"
            void f() {
                UI::TableSetupColumn("Name", init_width_or_weight: 100.0);
                UI::Columns(2, border: "not bool");
            }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "TableSetupColumn must skip defaulted flags, and Columns::border must bind by name: {:?}",
            diags
        );
        assert_eq!(
            bad[0].kind,
            TypeDiagnosticKind::ArgTypeMismatch {
                function_name: "Columns".into(),
                param_index: 2,
                expected: "bool".into(),
                got: "string".into(),
            }
        );
    }

    #[test]
    fn external_member_named_args_bind_by_name() {
        let diags = check_with_typedb(
            r#"
            void f() {
                Context@ ctx;
                ctx.AssertTrue(message: true);
            }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "AssertTrue::message must bind by name, got {:?}",
            diags
        );
        assert_eq!(
            bad[0].kind,
            TypeDiagnosticKind::ArgTypeMismatch {
                function_name: "AssertTrue".into(),
                param_index: 1,
                expected: "string".into(),
                got: "bool".into(),
            }
        );
    }

    // ── B004: bare string param sanity warning ──────────────────────────────

    #[test]
    fn bare_string_param_warns() {
        let diags = check("void f(string x) {}");
        let warn: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::StringByValueParam { .. }))
            .collect();
        assert_eq!(
            warn.len(),
            1,
            "expected StringByValueParam warning, got {:?}",
            diags
        );
        assert_eq!(
            warn[0].kind,
            TypeDiagnosticKind::StringByValueParam {
                param_name: "x".into()
            }
        );
        assert_eq!(warn[0].severity(), TypeDiagnosticSeverity::Warning);
        assert!(
            warn[0].message().contains("const string &in x"),
            "message should mention preferred form, got {}",
            warn[0].message()
        );
        assert!(
            warn[0]
                .message()
                .contains("prefix the parameter name with an underscore to ignore this warning"),
            "message must match Openplanet underscore-suppress trailer, got {}",
            warn[0].message()
        );
        // Display impl mirrors message()
        assert_eq!(format!("{}", warn[0]), warn[0].message());
    }

    #[test]
    fn underscore_prefixed_string_param_silent() {
        // Game: void foo(string _x) does not emit the sanity warning.
        let diags = check("void f(string _x) {}");
        let warn: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::StringByValueParam { .. }))
            .collect();
        assert!(
            warn.is_empty(),
            "underscore-prefixed bare string param should not warn, got {:?}",
            diags
        );
    }

    #[test]
    fn const_string_in_param_silent() {
        let diags = check("void f(const string &in x) {}");
        let warn: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::StringByValueParam { .. }))
            .collect();
        assert!(
            warn.is_empty(),
            "const string &in should not warn, got {:?}",
            diags
        );
    }

    #[test]
    fn string_in_param_silent() {
        let diags = check("void f(string &in x) {}");
        let warn: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::StringByValueParam { .. }))
            .collect();
        assert!(
            warn.is_empty(),
            "string &in should not warn, got {:?}",
            diags
        );
    }

    #[test]
    fn other_types_unaffected_by_string_by_value_warn() {
        let diags = check("void f(int x, bool y, float z) {}");
        let warn: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::StringByValueParam { .. }))
            .collect();
        assert!(
            warn.is_empty(),
            "non-string params should not warn, got {:?}",
            diags
        );
    }

    #[test]
    fn const_string_by_value_also_warns() {
        // `const string` without &in is still a by-value copy.
        let diags = check("void f(const string x) {}");
        let warn: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::StringByValueParam { .. }))
            .collect();
        assert_eq!(
            warn.len(),
            1,
            "const string by value should warn, got {:?}",
            diags
        );
    }

    #[test]
    fn bare_string_method_param_warns() {
        let diags = check("class C { void m(string name) {} }");
        let warn: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::StringByValueParam { .. }))
            .collect();
        assert_eq!(
            warn.len(),
            1,
            "method bare string param should warn, got {:?}",
            diags
        );
        assert_eq!(
            warn[0].kind,
            TypeDiagnosticKind::StringByValueParam {
                param_name: "name".into()
            }
        );
    }

    // ── GH #44: @handle-assign into an indexed value-type slot ──────────

    #[test]
    fn handle_assign_into_json_index_is_not_an_lvalue() {
        // Game-compiler ground truth (matrix probe, 2026-08-17):
        //   `@arr[0] = tiny` (Json::Value@ receiver) → ERR not an l-value
        //   `@arr[1] = iv`   (same)                  → ERR not an l-value
        // while `arr[0] = tiny` (value-copy) is legal.
        let diags = check_with_typedb(
            r#"void f(Json::Value@ arr) {
                Json::Value@ tiny = Json::Object();
                @arr[0] = tiny;
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::InvalidAssignmentTarget))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected InvalidAssignmentTarget for @ into Json index, got {:?}",
            diags
        );
    }

    #[test]
    fn handle_assign_into_json_index_primitive_rhs_also_flags() {
        let diags = check_with_typedb(
            r#"void f(Json::Value@ arr) {
                int iv = 7;
                @arr[1] = iv;
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::InvalidAssignmentTarget))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected InvalidAssignmentTarget for @ into Json index (primitive rhs), got {:?}",
            diags
        );
    }

    #[test]
    fn handle_assign_legal_forms_stay_silent() {
        // Game-verified legal: typed handle array slot, dictionary slot,
        // plain handle ident, value-copy into Json index.
        let diags = check_with_typedb(
            r#"void f(Json::Value@ arr) {
                Json::Value@[] a;
                Json::Value@ h = Json::Object();
                a.InsertLast(h);
                @a[0] = h;
                dictionary d;
                @d["k"] = h;
                Json::Value@ g;
                @g = h;
                arr[0] = h;
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::InvalidAssignmentTarget
                        | TypeDiagnosticKind::HandleValueMismatch { .. }
                )
            })
            .collect();
        assert!(
            bad.is_empty(),
            "expected no assign diagnostics for game-legal forms, got {:?}",
            diags
        );
    }

    // ── GH #38: workspace class shadows typedb short-name collision ─────

    #[test]
    fn workspace_class_shadows_typedb_short_name_for_member_lookup() {
        // `Discord::Status` exists in the Core typedb with an empty member
        // list. A plugin-declared `Status` (in a namespace) must shadow the
        // engine type: the game compiler prefers the workspace declaration,
        // so member lookups on it must not emit UndefinedMember.
        let diags = check_with_typedb(
            r#"namespace Repro {
                enum StatusKind { A, B }
                class Status {
                    void Set(StatusKind k) { m2 = k; }
                    StatusKind get_Kind() const property { return m2; }
                    private StatusKind m2 = StatusKind::A;
                }
                Status g_Status;
                void Use() {
                    g_Status.Set(StatusKind::B);
                    StatusKind b = g_Status.Kind;
                }
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no UndefinedMember for workspace-shadowed `Status`, got {:?}",
            diags
        );
    }

    // ── B006: UndefinedMember on Nadeo types with trusted member lists ──

    #[test]
    fn nadeo_missing_member_on_populated_type_fires() {
        // CGameCtnCollection has a large non-empty member list and does
        // not declare CollectionName (that lives on CGameCtnChallenge).
        let diags = check_with_typedb(
            r#"void f() {
                CGameCtnCollection@ c;
                string n = c.CollectionName;
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected UndefinedMember for CollectionName on CGameCtnCollection, got {:?}",
            diags
        );
        match &bad[0].kind {
            TypeDiagnosticKind::UndefinedMember {
                object_type,
                member,
            } => {
                assert!(
                    object_type.ends_with("CGameCtnCollection"),
                    "object_type={object_type}"
                );
                assert_eq!(member, "CollectionName");
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    #[test]
    fn nadeo_existing_member_on_populated_type_silent() {
        // CollectionName is a real property of CGameCtnChallenge.
        let diags = check_with_typedb(
            r#"void f() {
                CGameCtnChallenge@ map;
                string n = map.CollectionName;
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no UndefinedMember for real CollectionName, got {:?}",
            diags
        );
    }

    #[test]
    fn nadeo_nested_collection_name_case_fires() {
        // Real bug report: map.Collection.CollectionName — Collection is
        // CGameCtnCollection@ which has no CollectionName member.
        let diags = check_with_typedb(
            r#"void f() {
                CGameCtnChallenge@ map;
                string n = string(map.Collection.CollectionName);
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::UndefinedMember { member, .. } if member == "CollectionName"
                )
            })
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected CollectionName UndefinedMember on nested Collection, got {:?}",
            diags
        );
    }

    #[test]
    fn nadeo_empty_member_list_stays_silent() {
        // CMwEngine has zero listed members in the Nadeo fixture — incomplete
        // metadata, so missing members must not flood diagnostics.
        let diags = check_with_typedb(
            r#"void f() {
                CMwEngine@ e;
                auto x = e.TotallyMissingMember;
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected silence for empty Nadeo member list, got {:?}",
            diags
        );
    }

    #[test]
    fn nadeo_inherited_member_via_parent_silent() {
        // IdName lives on CMwNod; CGameCtnCollection parents to CMwNod.
        let diags = check_with_typedb(
            r#"void f() {
                CGameCtnCollection@ c;
                string id = c.IdName;
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no UndefinedMember for inherited IdName, got {:?}",
            diags
        );
    }

    #[test]
    fn nadeo_missing_method_on_populated_type_fires() {
        let diags = check_with_typedb(
            r#"void f() {
                CGameCtnCollection@ c;
                c.DefinitelyNotAMethod();
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected UndefinedMember for missing Nadeo method, got {:?}",
            diags
        );
    }

    // ── B007: distinct enum types at external call sites ─────────────────

    #[test]
    fn is_convertible_distinct_named_enums_false() {
        // Distinct Named types are not interchangeable even if both are
        // int-backed enums in the game. is_convertible only equates equals.
        let a = TypeRepr::Named("CGameCtnBlock::ECardinalDirections".into());
        let b = TypeRepr::Named("CGameEditorPluginMap::ECardinalDirections".into());
        assert!(!is_convertible(&a, &b));
        assert!(is_convertible(&a, &a));
        // Numeric↔numeric still allowed.
        assert!(is_convertible(
            &TypeRepr::Primitive(PrimitiveType::Int),
            &TypeRepr::Primitive(PrimitiveType::Float)
        ));
    }

    #[test]
    fn external_remove_block_safe_cross_enum_fires() {
        // Real typedb case (Gizmo.as / B007): RemoveBlockSafe wants
        // CGameEditorPluginMap::ECardinalDirections, but CGameCtnBlock::Direction
        // is CGameCtnBlock::ECardinalDirections.
        let diags = check_with_typedb(
            r#"
            void f() {
                CGameEditorPluginMapMapType@ pmt;
                CGameCtnBlock@ targetBlock;
                int3 coord;
                pmt.RemoveBlockSafe(targetBlock.BlockInfo, coord, targetBlock.Direction);
            }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ArgTypeMismatch for cross-enum Direction, got {:?}",
            diags
        );
        match &bad[0].kind {
            TypeDiagnosticKind::ArgTypeMismatch {
                function_name,
                param_index,
                expected,
                got,
            } => {
                assert_eq!(function_name, "RemoveBlockSafe");
                assert_eq!(*param_index, 2);
                assert!(
                    expected.contains("CGameEditorPluginMap")
                        && expected.contains("ECardinalDirections"),
                    "expected plugin-map enum, got {expected}"
                );
                assert!(
                    got.contains("CGameCtnBlock") && got.contains("ECardinalDirections"),
                    "expected block enum, got {got}"
                );
            }
            other => panic!("unexpected kind: {:?}", other),
        }
    }

    #[test]
    fn external_remove_block_safe_cross_enum_local_fires() {
        // Synthetic: typed locals with distinct enum types (no property path).
        let diags = check_with_typedb(
            r#"
            void f() {
                CGameEditorPluginMap@ pmt;
                CGameCtnBlockInfo@ info;
                int3 coord;
                CGameCtnBlock::ECardinalDirections wrong;
                pmt.RemoveBlockSafe(info, coord, wrong);
            }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ArgTypeMismatch for cross-enum local, got {:?}",
            diags
        );
    }

    #[test]
    fn external_remove_block_safe_same_enum_silent() {
        // CGameCtnBlock::Dir is already CGameEditorPluginMap::ECardinalDirections.
        let diags = check_with_typedb(
            r#"
            void f() {
                CGameEditorPluginMapMapType@ pmt;
                CGameCtnBlock@ targetBlock;
                int3 coord;
                pmt.RemoveBlockSafe(targetBlock.BlockInfo, coord, targetBlock.Dir);
            }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch for same-enum Dir, got {:?}",
            diags
        );
    }

    #[test]
    fn external_remove_block_safe_matching_local_enum_silent() {
        let diags = check_with_typedb(
            r#"
            void f() {
                CGameEditorPluginMap@ pmt;
                CGameCtnBlockInfo@ info;
                int3 coord;
                CGameEditorPluginMap::ECardinalDirections dir;
                pmt.RemoveBlockSafe(info, coord, dir);
            }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch for matching local enum, got {:?}",
            diags
        );
    }

    /// better-totd FP: bare function names are valid `CoroutineFunc@` args to
    /// `startnew` (AngelScript function-pointer decay). Must not report
    /// ArgTypeMismatch against `CoroutineFunc`.
    #[test]
    fn startnew_bare_function_name_is_silent() {
        let diags = check_with_typedb(
            r#"
            void Worker() {}
            void Main() { startnew(Worker); }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::ArgTypeMismatch { function_name, .. }
                        if function_name == "startnew"
                )
            })
            .collect();
        assert!(
            bad.is_empty(),
            "expected no startnew ArgTypeMismatch for bare function, got {:?}",
            diags
        );
    }

    /// better-totd FP: global `vec4` variables must keep their type when
    /// passed to external APIs (not collapse to Named(varName)).
    #[test]
    fn global_vec4_var_push_style_color_is_silent() {
        let diags = check_with_typedb(
            r#"
            vec4 overviewTableRowBg = vec4(.2, .2, .2, .2);
            void f() {
                UI::PushStyleColor(UI::Col::TableRowBg, overviewTableRowBg);
            }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch for global vec4 arg, got {:?}",
            diags
        );
    }

    /// better-totd FP: `UI::Font@` globals are valid `PushFont` args.
    #[test]
    fn global_font_handle_push_font_is_silent() {
        let diags = check_with_typedb(
            r#"
            UI::Font@ g_BoldFont;
            void f() { UI::PushFont(g_BoldFont); }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch for UI::Font@ → PushFont, got {:?}",
            diags
        );
    }

    /// better-totd FP: array methods are generic in `T`; do not compare the
    /// element value against the unsubstituted placeholder type name `T`.
    #[test]
    fn array_insert_last_concrete_element_is_silent() {
        let diags = check_with_typedb(
            r#"
            class LazyMap {}
            void f() {
                array<LazyMap@> maps;
                LazyMap@ lm;
                maps.InsertLast(lm);
            }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch for array.InsertLast(T), got {:?}",
            diags
        );
    }

    /// better-totd FP: `tostring` accepts any value via `?` — enums/named
    /// types must not ArgTypeMismatch against the placeholder.
    #[test]
    fn tostring_any_type_param_is_silent() {
        let diags = check_with_typedb(
            r#"
            enum SortMethod { Date, Name, _LastNop }
            void f() {
                SortMethod sm = SortMethod::Date;
                string s = tostring(sm);
            }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::ArgTypeMismatch { function_name, .. }
                        if function_name == "tostring"
                )
            })
            .collect();
        assert!(
            bad.is_empty(),
            "expected no tostring ArgTypeMismatch for enum, got {:?}",
            diags
        );
    }

    /// better-totd FP: `Json::Value@` is valid for `Json::ToFile`'s value param.
    #[test]
    fn json_to_file_value_handle_is_silent() {
        let diags = check_with_typedb(
            r#"
            namespace AuthorTracker {
                Json::Value@ meta = null;
                void Save() { Json::ToFile("x.json", meta); }
            }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no ArgTypeMismatch for Json::ToFile Value@, got {:?}",
            diags
        );
    }

    /// tm-dashboard FP: game accepts `nvg::Font` even though Core typedb only
    /// documents LoadFont→int / FontFace(int). Must not UnknownType.
    #[test]
    fn nvg_font_type_is_known() {
        let diags = check_with_typedb(
            r#"
            nvg::Font g_font;
            void Main() {
                g_font = nvg::LoadFont("DroidSans.ttf", true);
                nvg::FontFace(g_font);
            }
            "#,
        );
        let unknown: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::UnknownType(n) if n.contains("nvg::Font") || n == "Font"
                )
            })
            .collect();
        assert!(
            unknown.is_empty(),
            "expected no UnknownType for nvg::Font, got {:?}",
            diags
        );
    }

    // GH #30: a bare ident that happens to match a field of a *sibling* class
    // in the same file must stay undefined inside another class's method.
    // Fields are stored as `ClassName::fieldName` and the workspace tail
    // match (ends_with "::name") used to leak them into bare-name lookup.
    #[test]
    fn sibling_class_field_does_not_silence_undefined_ident() {
        let diags = check_workspace(
            r#"
            class Widget {
                CGameItemModel@ item;
                void Frob() {
                    auto bad = cast<CGameItemModel>(nod);
                    if (bad is null) return;
                }
            }
            "#,
            &[r#"
            class TreeElem {
                CMwNod@ nod;
                void Use() { if (nod is null) return; }
            }
            "#],
        );
        let hits: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::UndefinedIdentifier(n) if n == "nod"
                )
            })
            .collect();
        assert!(
            hits.len() == 1,
            "expected exactly 1 UndefinedIdentifier for `nod` (Widget's use; TreeElem's own use must stay silent), got {:?}",
            diags
        );
    }

    // GH #23: an `auto` local resolves to the `auto` placeholder, which has
    // no real type. Comparing it against a Named param produced
    // `expected X, got auto` FPs. Until we infer from the initializer, the
    // arg check must silence `auto` args (same as unknown/error types).
    // GH #23 RED: when the initializer can't be inferred (workspace-local
    // class with no typedb footprint), `expr_type` yields the `auto`
    // placeholder and the arg check must not compare it against the param.
    #[test]
    fn auto_typed_arg_stays_silent() {
        let diags = check_with_typedb(
            r#"
            class Helper {}
            void Use() {
                auto h = Helper();
                Reflection::GetRefCount(h);
            }
            "#,
        );
        let hits: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            hits.is_empty(),
            "auto-typed arg must not fire ArgTypeMismatch, got {:?}",
            diags
        );
    }

    // GH #23 control: a fully-typed mismatch must still fire.
    #[test]
    fn fully_typed_mismatch_still_fires() {
        let diags = check_with_typedb(
            r#"
            void Use() {
                MwClassInfo@ info;
                Reflection::GetRefCount(info);
            }
            "#,
        );
        let hits: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            !hits.is_empty(),
            "int arg for UI::Font param must still fire ArgTypeMismatch, got {:?}",
            diags
        );
    }

    // GH #22: subclass args must be accepted where a base-class param is
    // expected (external typedb inheritance walk). `Dev::GetRefCount` takes
    // `CMwNod`; passing `CGameCtnAnchoredObject` (a descendant) is legal
    // in-game but the LSP reported ArgTypeMismatch.
    #[test]
    fn subclass_arg_accepted_for_base_param() {
        let diags = check_with_typedb(
            r#"
            void Use(CGameCtnAnchoredObject@ obj) {
                Reflection::GetRefCount(obj);
            }
            "#,
        );
        let hits: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            hits.is_empty(),
            "subclass arg for CMwNod param must not fire ArgTypeMismatch, got {:?}",
            diags
        );
    }

    // GH #22 control: unrelated Named pairs must still be rejected.
    // `MwClassInfo` has no typedb parent chain into CMwNod — passing it to
    // a CMwNod param is a genuine mismatch and must stay loud.
    #[test]
    fn unrelated_named_arg_still_rejected() {
        let diags = check_with_typedb(
            r#"
            void Use(MwClassInfo@ info) {
                Reflection::GetRefCount(info);
            }
            "#,
        );
        let hits: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            !hits.is_empty(),
            "string arg for CMwNod param must still fire ArgTypeMismatch, got {:?}",
            diags
        );
    }

    // GH #21: `MwAddRef` / `MwRelease` are AngelScript builtins on every
    // CMwNod-derived type (refcount management). The typedb doesn't declare
    // them, so the LSP flooded ~107 UndefinedMember FPs on
    // tm-editor-plus-plus. Game accepts these silently.
    #[test]
    fn mw_addref_release_on_mwnod_stays_silent() {
        let diags = check_with_typedb(
            r#"
            void Use(CMwNod@ nod, CGameCtnBlockInfo@ bi) {
                nod.MwAddRef();
                bi.MwAddRef();
                bi.MwRelease();
            }
            "#,
        );
        let hits: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { member, .. }
                    if member == "MwAddRef" || member == "MwRelease")
            })
            .collect();
        assert!(
            hits.is_empty(),
            "MwAddRef/MwRelease must not fire UndefinedMember, got {:?}",
            diags
        );
    }

    // Intake 2026-08-14 (tm-control-mcp log): unary `!` on a handle —
    // game rejects with "Illegal operation on this datatype" (OP 1.29.5,
    // AsyncDispatch.as:136 `!result.Get("success", false)`).
    #[test]
    fn unary_not_on_handle_is_diagnosed() {
        let diags = check(
            r#"
            class Node { int v; }
            void Use() {
                Node@ n = null;
                if (!n) { Print("no"); }
                Node n2;
                if (!n2) { Print("no2"); }   // value instance — also illegal
            }
            "#,
        );
        let hits: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::IllegalUnaryOperand { .. }))
            .collect();
        assert!(
            hits.len() == 2,
            "expected IllegalUnaryOperand on both `!n` (handle) and `!n2` (value instance), got {:?}",
            diags
        );
    }

    // Controls: legal `!` operands must stay silent.
    #[test]
    fn unary_not_on_bool_stays_silent() {
        let diags = check(
            r#"
            void Use() {
                bool b = true;
                bool c;
                if (!b) { c = true; }
                if (!!(b && true)) { c = false; }
            }
            "#,
        );
        assert!(
            diags.is_empty(),
            "legal bool `!` must stay silent, got {:?}",
            diags
        );
    }

    #[test]
    fn unary_not_on_const_bool_stays_silent() {
        // E++ uses `!CONSTANT` where the constant is `const bool` — legal.
        let diags = check(
            r#"
            const bool ENABLE_OLD_HELPER = false;
            void Use() {
                if (!ENABLE_OLD_HELPER) { return; }
            }
            "#,
        );
        assert!(
            diags.is_empty(),
            "const bool `!` must stay silent, got {:?}",
            diags
        );
    }

    // ── GH #37 slice 1: warning-parity implicit-conversion diagnostics ─────
    //
    // Game-compiler ground truth (live RemoteBuild probe 2026-08-17,
    // OpenplanetNext/TM2020, OP 1.29.x) — all four are WARNING severity:
    //   `int ms = 3.7;`            → `Implicit conversion of value is not exact`
    //   `float f = 1.5; int g = f;`→ `Float value truncated in implicit conversion to integer`
    //   `uint u = -1;`             → `Implicit conversion changed sign of value`
    //   `int i; uint u; i < u`     → `Signed/Unsigned mismatch`

    fn has_warn_kind(diags: &[TypeDiagnostic], pred: impl Fn(&TypeDiagnosticKind) -> bool) -> bool {
        diags
            .iter()
            .any(|d| pred(&d.kind) && matches!(d.severity(), TypeDiagnosticSeverity::Warning))
    }

    // ── FloatTruncation: literal variant ────────────────────────────────────

    #[test]
    fn float_truncation_literal_int_init_warns_not_exact() {
        let diags = check("void f() { int ms = 3.7; }");
        assert!(
            has_warn_kind(&diags, |k| matches!(
                k,
                TypeDiagnosticKind::FloatTruncation { literal: true }
            )),
            "expected FloatTruncation literal warning, got {:?}",
            diags
        );
        let msg = diags
            .iter()
            .find(|d| matches!(d.kind, TypeDiagnosticKind::FloatTruncation { .. }))
            .expect("warning present")
            .message();
        assert_eq!(msg, "Implicit conversion of value is not exact");
    }

    #[test]
    fn float_truncation_literal_uint_init_warns_not_exact() {
        let diags = check("void f() { uint ms = 3.7; }");
        assert!(
            has_warn_kind(&diags, |k| matches!(
                k,
                TypeDiagnosticKind::FloatTruncation { literal: true }
            )),
            "expected FloatTruncation literal warning for uint target, got {:?}",
            diags
        );
    }

    // ── FloatTruncation: non-literal variant ────────────────────────────────

    #[test]
    fn float_truncation_float_var_to_int_warns_truncated() {
        let diags = check("void f() { float x = 1.5; int g = x; }");
        assert!(
            has_warn_kind(&diags, |k| matches!(
                k,
                TypeDiagnosticKind::FloatTruncation { literal: false }
            )),
            "expected FloatTruncation non-literal warning, got {:?}",
            diags
        );
        let msg = diags
            .iter()
            .find(|d| matches!(d.kind, TypeDiagnosticKind::FloatTruncation { .. }))
            .expect("warning present")
            .message();
        assert_eq!(
            msg,
            "Float value truncated in implicit conversion to integer"
        );
    }

    // ── FloatTruncation: legal counterparts stay silent ─────────────────────

    #[test]
    fn float_truncation_exact_int_init_stays_silent() {
        let diags = check("void f() { int x = 3; uint u = 1; }");
        assert!(
            !diags
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::FloatTruncation { .. })),
            "int-from-int must not warn, got {:?}",
            diags
        );
    }

    #[test]
    fn float_truncation_float_init_stays_silent() {
        let diags = check("void f() { float x = 1.5; double d = 1.5; }");
        assert!(
            !diags
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::FloatTruncation { .. })),
            "float/double targets must not warn, got {:?}",
            diags
        );
    }

    // ── SignChange ──────────────────────────────────────────────────────────

    #[test]
    fn sign_change_negative_literal_into_uint_warns() {
        let diags = check("void f() { uint u = -1; }");
        assert!(
            has_warn_kind(&diags, |k| matches!(k, TypeDiagnosticKind::SignChange)),
            "expected SignChange warning, got {:?}",
            diags
        );
        let msg = diags
            .iter()
            .find(|d| matches!(d.kind, TypeDiagnosticKind::SignChange))
            .expect("warning present")
            .message();
        assert_eq!(msg, "Implicit conversion changed sign of value");
    }

    #[test]
    fn sign_change_positive_literal_into_uint_stays_silent() {
        let diags = check("void f() { uint u = 1; }");
        assert!(
            !diags
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::SignChange)),
            "positive literal into uint must not warn, got {:?}",
            diags
        );
    }

    #[test]
    fn sign_change_runtime_int_into_uint_stays_silent() {
        // The game does NOT warn for `uint u = i;` — runtime sign changes at
        // assignment are not this class (RemoteBuild probe; only
        // compile-time-known negatives warn).
        let diags = check("void f() { int i = -5; uint u = i; }");
        assert!(
            !diags
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::SignChange)),
            "runtime int→uint must not warn, got {:?}",
            diags
        );
    }

    // ── SignedUnsignedMismatch ──────────────────────────────────────────────

    fn sum_diags(source: &str) -> Vec<TypeDiagnostic> {
        check(source)
            .into_iter()
            .filter(|d| matches!(d.kind, TypeDiagnosticKind::SignedUnsignedMismatch))
            .collect()
    }

    fn assert_sum_silent(source: &str) {
        let diags = check(source);
        assert!(
            !diags
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::SignedUnsignedMismatch)),
            "expected no SignedUnsignedMismatch in {source:?}, got {diags:?}"
        );
    }

    // Game-verified semantics (RemoteBuild granularity probes 2026-08-17):
    // warn iff (a) a relational op `<`/`<=`/`>`/`>=` (NOT `==`/`!=`)
    // directly compares a pure signed int with a pure unsigned int with no
    // integer literal in either subtree, or (b) a `+` add mixes int and
    // uint with no literal in the mix subtree and no enclosing arithmetic
    // op. `-`/`*`/`&` mixes are silent; literal-tainted subtrees are silent.

    // ── Probe cells: direct relational comparisons ─────────────────────

    #[test]
    fn sum_direct_relational_var_vs_var_warns() {
        // Cell: `int i; uint u; i < u` → WARN.
        let diags = sum_diags("void f() { int i = 1; uint u = 2; bool b = i < u; }");
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one warning, got {diags:?}"
        );
        assert_eq!(diags[0].message(), "Signed/Unsigned mismatch");
    }

    #[test]
    fn sum_direct_relational_each_op_warns() {
        for op in ["<", "<=", ">", ">="] {
            let src = format!("void f() {{ int i = 1; uint u = 2; bool b = i {op} u; }}");
            let diags = sum_diags(&src);
            assert_eq!(diags.len(), 1, "op `{op}` must warn once, got {diags:?}");
        }
        // Reversed operand order warns too.
        let diags = sum_diags("void f() { int i = 1; uint u = 2; bool b = u < i; }");
        assert_eq!(diags.len(), 1, "u < i must warn, got {diags:?}");
    }

    #[test]
    fn sum_uint_vs_int_literal_silent() {
        // Cells: `uint n; n > 0` and `n > 1` → SILENT (literal operand).
        assert_sum_silent("void f() { uint n = 1; bool b = n > 0; }");
        assert_sum_silent("void f() { uint n = 1; bool b = n > 1; }");
    }

    #[test]
    fn sum_literal_left_of_uint_silent() {
        // Cell: `uint n; 0 < n` → SILENT (literal on the left).
        assert_sum_silent("void f() { uint n = 1; bool b = 0 < n; }");
    }

    #[test]
    fn sum_array_length_vs_literal_silent() {
        // Cell: `arr.Length > 0` (.Length is uint, vs literal) → SILENT.
        assert_sum_silent("void f() { array<int> arr; bool b = arr.Length > 0; }");
    }

    #[test]
    fn sum_equality_mixed_silent() {
        // Cell: `int i; uint u; i == u` → SILENT (equality, not relational).
        assert_sum_silent("void f() { int i = 1; uint u = 2; bool b = i == u; }");
        assert_sum_silent("void f() { int i = 1; uint u = 2; bool b = i != u; }");
    }

    #[test]
    fn sum_same_signedness_relational_silent() {
        assert_sum_silent(
            "void f() { int i = 1; int j = 2; uint u = 3; uint v = 4; bool b = i < j; bool c = u < v; }",
        );
    }

    #[test]
    fn sum_unknown_operand_silent() {
        // Conservative silence: an unknown/Error operand must not warn
        // (the undefined-ident error is emitted separately).
        assert_sum_silent("void f() { int i = 1; bool b = i < nope; }");
    }

    // ── Probe cells: arithmetic mixes ──────────────────────────────────

    #[test]
    fn sum_add_mix_in_relational_warns_at_add() {
        // Cell: `int i; uint u; (i + u) > 0` → WARN, span at the `+`.
        let src = "void f() { int i = 1; uint u = 2; bool b = i + u > 0; }";
        let diags = sum_diags(src);
        assert_eq!(diags.len(), 1, "expected one warning, got {diags:?}");
        let plus = src.find('+').unwrap() as u32;
        assert!(
            diags[0].span.start <= plus && plus < diags[0].span.end,
            "warning span {:?} must cover the `+` at {plus}",
            diags[0].span
        );
    }

    #[test]
    fn sum_add_mix_vs_int_var_warns() {
        // Probe T3: `i + u < j` (signed var partner) → WARN at the `+`.
        let diags = sum_diags("void f() { int i = 1; uint u = 2; int j = 3; bool b = i + u < j; }");
        assert_eq!(diags.len(), 1, "expected one warning, got {diags:?}");
    }

    #[test]
    fn sum_add_mix_vs_uint_var_warns() {
        // Probe T3: `i + u < v` (unsigned var partner) → WARN at the `+`.
        let diags =
            sum_diags("void f() { int i = 1; uint u = 2; uint v = 4; bool b = i + u < v; }");
        assert_eq!(diags.len(), 1, "expected one warning, got {diags:?}");
    }

    #[test]
    fn sum_sub_mix_relational_silent() {
        // Cell: `int i; uint u; (i - u) < i` → SILENT (subtraction exempt).
        assert_sum_silent("void f() { int i = 1; uint u = 2; bool b = i - u > 0; }");
        assert_sum_silent("void f() { int i = 1; uint u = 2; bool b = (i - u) < i; }");
        assert_sum_silent("void f() { int i = 1; uint u = 2; bool b = (u - i) < i; }");
    }

    #[test]
    fn sum_mul_and_bitand_mix_silent() {
        // Probe T2: `i * u > 0` and `(i & u) > 0` → SILENT.
        assert_sum_silent("void f() { int i = 1; uint u = 2; bool b = i * u > 0; }");
        assert_sum_silent("void f() { int i = 1; uint u = 2; bool b = (i & u) > 0; }");
    }

    #[test]
    fn sum_add_literal_taint_silent() {
        // Probe T2: `i + 1 < u` → SILENT (literal taints the add subtree).
        assert_sum_silent("void f() { int i = 1; uint u = 2; bool b = i + 1 < u; }");
        // Probe T3: `(i + 1) + u < j` → SILENT (taint poisons the whole chain).
        assert_sum_silent("void f() { int i = 1; uint u = 2; int j = 3; bool b = i + 1 + u < j; }");
    }

    #[test]
    fn sum_add_mix_under_enclosing_arith_silent() {
        // Probe T3: `(i + u) * 2 > j` → SILENT (mix not topmost).
        assert_sum_silent(
            "void f() { int i = 1; uint u = 2; int j = 3; bool b = (i + u) * 2 > j; }",
        );
    }

    #[test]
    fn sum_add_mix_inside_call_arg_warns() {
        // Extension: the mix warning fires wherever the `+` mix appears,
        // not only under relational ops (the game's warning lives at the
        // `+`, independent of any comparison).
        let diags = sum_diags("void g(int x) {}\nvoid f() { int i = 1; uint u = 2; g(i + u); }");
        assert_eq!(diags.len(), 1, "expected one warning, got {diags:?}");
    }

    // ── GH #37 slice 3: duplicate top-level function declarations ───────────

    fn dupfn_diags(source: &str) -> Vec<TypeDiagnostic> {
        check(source)
            .into_iter()
            .filter(|d| matches!(d.kind, TypeDiagnosticKind::DuplicateFunction { .. }))
            .collect()
    }

    #[test]
    fn duplicate_function_exact_fires_once_at_second_decl() {
        let src = "void DupFn(int a) {}\n\nvoid DupFn(int a) {}\n";
        let diags = dupfn_diags(src);
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic, got {diags:?}"
        );
        assert_eq!(
            diags[0].message(),
            "A function with the same name and parameters already exists"
        );
        assert_eq!(diags[0].severity(), TypeDiagnosticSeverity::Error);
        // Span points at the second declaration (its start).
        let second_start = src.rfind("void DupFn(int a) {}").unwrap() as u32;
        assert_eq!(diags[0].span.start, second_start);
    }

    #[test]
    fn duplicate_function_overload_different_param_types_silent() {
        let src = "void F(int a) {}\nvoid F(string s) {}\n";
        let diags = dupfn_diags(src);
        assert!(
            diags.is_empty(),
            "overloads must stay silent, got {diags:?}"
        );
    }

    #[test]
    fn duplicate_function_overload_different_arity_silent() {
        let src = "void F(int a) {}\nvoid F(int a, int b) {}\n";
        let diags = dupfn_diags(src);
        assert!(
            diags.is_empty(),
            "overloads must stay silent, got {diags:?}"
        );
    }

    #[test]
    fn duplicate_function_cross_namespace_silent() {
        let src = "namespace A { void F(int a) {} }\nnamespace B { void F(int a) {} }\n";
        let diags = dupfn_diags(src);
        assert!(
            diags.is_empty(),
            "same name in different namespaces must stay silent, got {diags:?}"
        );
    }

    #[test]
    fn duplicate_function_namespaced_vs_global_silent() {
        let src = "void F(int a) {}\nnamespace A { void F(int a) {} }\n";
        let diags = dupfn_diags(src);
        assert!(
            diags.is_empty(),
            "global vs namespaced function must stay silent, got {diags:?}"
        );
    }

    #[test]
    fn duplicate_function_inside_same_namespace_fires() {
        let src = "namespace A {\nvoid F(int a) {}\nvoid F(int a) {}\n}\n";
        let diags = dupfn_diags(src);
        assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
    }

    #[test]
    fn duplicate_function_three_way_fires_twice() {
        let src = "void G(int a) {}\nvoid G(int a) {}\nvoid G(int a) {}\n";
        let diags = dupfn_diags(src);
        assert_eq!(
            diags.len(),
            2,
            "2nd and 3rd decls must both fire, got {diags:?}"
        );
    }

    #[test]
    fn duplicate_function_methods_not_flagged() {
        let src = "class C {\n  void M(int a) {}\n  void M(int a) {}\n}\n";
        let diags = dupfn_diags(src);
        assert!(
            diags.is_empty(),
            "methods are out of scope for this rule, got {diags:?}"
        );
    }
}

#[cfg(test)]
mod query_tests {
    use super::*;
    use crate::symbols::SymbolTable;

    fn check_with_recording(src: &str) -> Checker<'static> {
        let src: &'static str = Box::leak(src.to_string().into_boxed_str());
        let mut table = SymbolTable::new();
        let fid = table.allocate_file_id();
        let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
        let syms = SymbolTable::extract_symbols(fid, analysis.masked_source(), &analysis.file);
        table.set_file_symbols(fid, syms);
        let table: &'static SymbolTable = Box::leak(Box::new(table));
        let scope: &'static GlobalScope<'static> =
            Box::leak(Box::new(GlobalScope::new(table, None)));
        let mut c = Checker::new(src, scope).with_type_recording();
        c.check_file(&analysis.file);
        c
    }

    #[test]
    fn type_at_span_resolves_local_initializer() {
        let c = check_with_recording("void main() { int x = 42; }\n");
        // `42` starts at offset 22.
        let ty = c.type_at_span(22).expect("42 recorded");
        assert!(matches!(ty, TypeRepr::Primitive(PrimitiveType::Int)));
    }

    #[test]
    fn type_at_span_resolves_ident_local() {
        let c = check_with_recording("void main() { int x = 1; int y = x; }\n");
        // Second statement's `x` initializer starts at offset 33.
        let ty = c.type_at_span(33).expect("x recorded");
        assert!(matches!(ty, TypeRepr::Primitive(PrimitiveType::Int)));
    }

    #[test]
    fn recording_off_by_default() {
        let src: &'static str =
            Box::leak("void main() { int x = 1; }\n".to_string().into_boxed_str());
        let table: &'static SymbolTable = Box::leak(Box::new(SymbolTable::new()));
        let scope: &'static GlobalScope<'static> =
            Box::leak(Box::new(GlobalScope::new(table, None)));
        let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
        let mut c = Checker::new(src, scope);
        c.check_file(&analysis.file);
        assert!(c.recorded_expr_types().is_empty());
    }
}

#[cfg(test)]
mod warn_flow_tests {
    use super::*;
    use crate::symbols::SymbolTable;

    // ── GH #37: VariableShadow + UnreachableCode warnings ───────────────────

    fn check(source: &str) -> Vec<TypeDiagnostic> {
        let tokens = crate::lexer::tokenize_filtered(source);
        let mut parser = crate::parser::Parser::new(&tokens, source);
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

    fn shadows(diags: &[TypeDiagnostic]) -> Vec<&TypeDiagnostic> {
        diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::VariableShadow { .. }))
            .collect()
    }

    fn unreachables(diags: &[TypeDiagnostic]) -> Vec<&TypeDiagnostic> {
        diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UnreachableCode))
            .collect()
    }

    /// Game probe 2026-08-17: inner-block `int x` hides outer `int x` →
    /// WARN at the INNER declarator with the game's exact wording.
    #[test]
    fn variable_shadow_inner_block_warns() {
        let diags = check("void f() { int x = 1; if (true) { int x = 2; x++; } x++; }");
        let s = shadows(&diags);
        assert_eq!(s.len(), 1, "expected 1 shadow warning, got {:?}", diags);
        assert_eq!(
            s[0].kind,
            TypeDiagnosticKind::VariableShadow { name: "x".into() }
        );
        assert_eq!(
            s[0].message(),
            "Variable 'x' hides another variable of same name in outer scope"
        );
        assert_eq!(
            s[0].severity(),
            TypeDiagnosticSeverity::Warning,
            "shadow must be a warning"
        );
        // Span points at the INNER declarator name.
        let src = "void f() { int x = 1; if (true) { int x = 2; x++; } x++; }";
        let inner_off = src.rfind("x = 2").unwrap() as u32;
        assert_eq!(
            s[0].span.start, inner_off,
            "span must be the inner declarator"
        );
    }

    /// Params live in the outermost function frame: a local shadowing a
    /// param warns via the same mechanism.
    #[test]
    fn variable_shadow_of_param_warns() {
        let diags = check("void f(int x) { if (true) { int x = 2; x++; } }");
        let s = shadows(&diags);
        assert_eq!(s.len(), 1, "expected param shadow warning, got {:?}", diags);
        assert_eq!(
            s[0].kind,
            TypeDiagnosticKind::VariableShadow { name: "x".into() }
        );
    }

    #[test]
    fn variable_shadow_distinct_names_silent() {
        let diags = check("void f() { int x = 1; if (true) { int y = 2; y++; } x++; }");
        assert!(shadows(&diags).is_empty(), "got {:?}", diags);
    }

    /// Local shadowing a CLASS MEMBER is a common legal pattern — the game
    /// message is about "outer scope" locals; stay conservative and silent.
    #[test]
    fn variable_shadow_of_class_member_silent() {
        let src = r#"
class C {
    int x;
    void m() { int x = 1; x++; }
}
"#;
        let diags = check(src);
        assert!(shadows(&diags).is_empty(), "got {:?}", diags);
    }

    /// Local shadowing a GLOBAL stays silent (local-vs-local only).
    #[test]
    fn variable_shadow_of_global_silent() {
        let diags = check("int g; void f() { int g = 1; g++; }");
        assert!(shadows(&diags).is_empty(), "got {:?}", diags);
    }

    /// Shadowing applies uniformly inside class METHODS (shared walker).
    #[test]
    fn variable_shadow_in_method_warns() {
        let src = r#"
class C {
    void m() { int v = 1; if (true) { int v = 2; v++; } }
}
"#;
        let diags = check(src);
        assert_eq!(shadows(&diags).len(), 1, "got {:?}", diags);
    }

    /// A lambda is its own function: its locals must not "hide" the outer
    /// function's locals (and the outer frame must not leak in).
    #[test]
    fn variable_shadow_lambda_does_not_hide_outer_local() {
        let src = "void f() { int x = 1; auto cb = function() { int x = 2; x++; }; x++; }";
        let diags = check(src);
        assert!(shadows(&diags).is_empty(), "got {:?}", diags);
    }

    /// …but shadowing INSIDE the lambda still warns.
    #[test]
    fn variable_shadow_inside_lambda_warns() {
        let src = "void f() { auto cb = function() { int x = 1; { int x = 2; x++; } }; }";
        let diags = check(src);
        assert_eq!(shadows(&diags).len(), 1, "got {:?}", diags);
    }

    /// Sibling blocks don't share scope: same name in two if-branches is
    /// legal and silent.
    #[test]
    fn variable_shadow_sibling_blocks_silent() {
        let src = "void f() { if (true) { int x = 1; x++; } else { int x = 2; x++; } }";
        let diags = check(src);
        assert!(shadows(&diags).is_empty(), "got {:?}", diags);
    }

    /// Game probe 2026-08-17: statement after `return` → WARN once at the
    /// FIRST unreachable statement.
    #[test]
    fn unreachable_after_return_warns_once() {
        let diags = check("int F() { return 1; int y = 2; return y; }");
        let u = unreachables(&diags);
        assert_eq!(
            u.len(),
            1,
            "expected 1 unreachable warning, got {:?}",
            diags
        );
        assert_eq!(u[0].message(), "Unreachable code");
        assert_eq!(u[0].severity(), TypeDiagnosticSeverity::Warning);
        let src = "int F() { return 1; int y = 2; return y; }";
        let off = src.find("int y").unwrap() as u32;
        assert_eq!(
            u[0].span.start, off,
            "span must be the first unreachable statement"
        );
    }

    #[test]
    fn mid_block_return_suppresses_missing_return_with_dead_tail() {
        // GH #37 review note: `stmts_terminate` intentionally moved to
        // any-position semantics when UnreachableCode landed. Consequence:
        // `int F() { return 1; int y = 2; }` (dead tail, no tail return)
        // now reports ONLY UnreachableCode and NOT MissingReturn — matching
        // the game (a return path exists). Pin the new behavior.
        let diags = check("int F() { return 1; int y = 2; }");
        assert!(
            !diags
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::MissingReturn { .. })),
            "mid-block return must suppress MissingReturn, got {:?}",
            diags
        );
        assert_eq!(
            unreachables(&diags).len(),
            1,
            "dead tail still warns UnreachableCode, got {:?}",
            diags
        );
    }

    #[test]
    fn unreachable_after_break_warns() {
        let src = "void f() { while (true) { break; int y = 2; } }";
        let diags = check(src);
        assert_eq!(unreachables(&diags).len(), 1, "got {:?}", diags);
    }

    #[test]
    fn unreachable_after_continue_warns() {
        let src = "void f() { for (int i = 0; i < 3; i++) { continue; i++; } }";
        let diags = check(src);
        assert_eq!(unreachables(&diags).len(), 1, "got {:?}", diags);
    }

    /// if-else where BOTH branches return terminates the block suffix.
    #[test]
    fn unreachable_after_both_branch_return_warns() {
        let src = "int f(bool b) { if (b) { return 1; } else { return 2; } int y = 3; return y; }";
        let diags = check(src);
        assert_eq!(unreachables(&diags).len(), 1, "got {:?}", diags);
    }

    /// Code after a NON-terminating if is reachable — silent.
    #[test]
    fn reachable_after_single_branch_return_silent() {
        let src = "int f(bool b) { if (b) { return 1; } int y = 2; return y; }";
        let diags = check(src);
        assert!(unreachables(&diags).is_empty(), "got {:?}", diags);
    }

    /// A break inside a nested if does NOT terminate the outer loop body:
    /// statements after it are still reachable.
    #[test]
    fn reachable_after_break_inside_if_silent() {
        let src = "void f() { while (true) { if (true) { break; } int y = 2; y++; } }";
        let diags = check(src);
        assert!(unreachables(&diags).is_empty(), "got {:?}", diags);
    }

    /// Loop body reachable on later iterations even though it ends in
    /// break/continue — and statements BEFORE the break are fine.
    #[test]
    fn loop_body_before_break_silent() {
        let src = "void f() { while (true) { int y = 2; y++; break; } }";
        let diags = check(src);
        assert!(unreachables(&diags).is_empty(), "got {:?}", diags);
    }

    /// Normal straight-line code: no warning.
    #[test]
    fn normal_sequence_silent() {
        let diags = check("void f() { int x = 1; x++; }");
        assert!(unreachables(&diags).is_empty(), "got {:?}", diags);
    }

    /// Unreachable detection applies in METHOD bodies too (shared walker).
    #[test]
    fn unreachable_in_method_warns() {
        let src = "class C { int m() { return 1; int y = 2; return y; } }";
        let diags = check(src);
        assert_eq!(unreachables(&diags).len(), 1, "got {:?}", diags);
    }

    /// An unreachable statement nested deeper still warns once at its own
    /// block level.
    #[test]
    fn unreachable_nested_block_warns() {
        let src = "void f() { { return; } { int y = 1; } }";
        let diags = check(src);
        // The block `{ return; }` terminates; the following sibling block
        // is the first unreachable statement.
        assert_eq!(unreachables(&diags).len(), 1, "got {:?}", diags);
    }

    /// MissingReturn must still work after stmts_terminate became
    /// any-position: `{ int x = 1; }` never returns → still diagnosed.
    #[test]
    fn missing_return_still_diagnosed() {
        let diags = check("int f() { int x = 1; }");
        assert!(
            diags
                .iter()
                .any(|d| matches!(&d.kind, TypeDiagnosticKind::MissingReturn { .. })),
            "got {:?}",
            diags
        );
    }
}
