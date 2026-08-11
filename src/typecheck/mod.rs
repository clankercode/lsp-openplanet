//! Type-checking scaffolding.
//!
//! This module is the entry point for the (in-progress) AngelScript static
//! type checker. It currently provides:
//!
//! * [`repr`] — canonical `TypeRepr` / `PrimitiveType` value types.
//! * [`global_scope`] — a merged read-only view of workspace + external
//!   symbol sources for lookup.
//! * [`resolver`] — turns parser `TypeExpr`s into resolved `TypeRepr`s,
//!   emitting diagnostics for unknown names.
//!
//! Expression type derivation, overload resolution, implicit conversions,
//! and const-correctness checking are *not* yet implemented — those will
//! layer on top of this once the scaffolding is in place.

pub mod builtins;
pub mod checker;
pub mod global_scope;
pub mod repr;
pub mod resolver;
pub mod workspace;

pub use checker::{Checker, TypeDiagnostic, TypeDiagnosticKind, TypeDiagnosticSeverity};
pub use global_scope::GlobalScope;
pub use repr::{PrimitiveType, TypeRepr};
pub use resolver::{ResolveDiagnostic, TypeResolver};
pub use workspace::build_plugin_symbol_table;
