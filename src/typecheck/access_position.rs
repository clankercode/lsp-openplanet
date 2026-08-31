//! Read/write classification of an access site.
//!
//! AngelScript exposes a `get_X` / `set_X` method pair as a virtual
//! property `X`, so *which* accessor an occurrence of `X` means depends on
//! the position it is written in: `obj.X` reads, `obj.X = v` writes, and a
//! compound assign `obj.X += v` does both — which is why the engine demands
//! BOTH accessors for one (the second line of the `ReadOnlyProperty`
//! diagnostic says exactly that).
//!
//! The checker already drew that distinction, inline, as the `compound`
//! flag it hands `check_readonly_prop_assign`. This module is that same
//! rule named once, so navigation can resolve `obj.X` to the accessor the
//! position actually calls for (B103) instead of re-deriving the rule from
//! the token stream — a token-level scan for a following `=` misreads
//! `obj.X == v`, `obj.X[0] = v` and `obj.X.y = v` alike.

use crate::lexer::Span;
use crate::parser::ast::{AssignOp, Expr, ExprKind, SourceFile};

/// What an occurrence of a virtual property does to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessPosition {
    /// `obj.X` in a value position — needs `get_X`.
    Read,
    /// `obj.X = v` — needs `set_X`.
    Write,
    /// `obj.X += v` — reads and then writes, so it needs BOTH accessors.
    ReadWrite,
}

impl AccessPosition {
    /// The position an assignment's left-hand side is in. `=` only writes;
    /// every compound operator (`+=`, `|=`, …) means `a = a OP b`, so it
    /// reads first. This is the rule `check_readonly_prop_assign` encodes
    /// as its `compound` argument.
    pub fn of_assign_op(op: AssignOp) -> Self {
        if matches!(op, AssignOp::Assign) {
            Self::Write
        } else {
            Self::ReadWrite
        }
    }

    /// True when the occurrence reads the property (`get_X` applies).
    pub fn reads(self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }

    /// True when the occurrence writes the property (`set_X` applies).
    pub fn writes(self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }

    /// Whether this is the compound case the `ReadOnlyProperty` diagnostic
    /// renders a second line for.
    pub fn is_compound(self) -> bool {
        matches!(self, Self::ReadWrite)
    }
}

/// Position of the access whose *name* token occupies `name_span`.
///
/// Mirrors `check_readonly_prop_assign`'s structural test exactly: an
/// access is a write only when it IS an assignment's left-hand side, not
/// merely contained in one. So `obj.X[0] = v` and `obj.X.y = v` READ
/// `obj.X` and write something reached through it, and `obj.X == v` is a
/// comparison — all three stay [`AccessPosition::Read`].
///
/// Defaults to `Read`, which is both the common case and the safe one for a
/// malformed or otherwise unrepresented site. The complete expression walk
/// includes declaration initializers, property accessor bodies, and parameter
/// defaults in addition to ordinary executable bodies.
pub fn access_position_at(file: &SourceFile, name_span: Span) -> AccessPosition {
    let mut found = AccessPosition::Read;
    crate::parser::walk::walk_file_exprs_complete(file, &mut |expr: &Expr| {
        let ExprKind::Assign { op, lhs, .. } = &expr.kind else {
            return;
        };
        let target = match &lhs.kind {
            ExprKind::Member { member, .. } => Some(member.span),
            ExprKind::NamespaceAccess { path } => path.segments.last().map(|s| s.span),
            // A bare name: a local, a global, or a namespace-level virtual
            // property written from inside its own namespace.
            ExprKind::Ident(ident) => Some(ident.span),
            _ => None,
        };
        if target.is_some_and(|s| s.start == name_span.start && s.end == name_span.end) {
            found = AccessPosition::of_assign_op(*op);
        }
    });
    found
}
