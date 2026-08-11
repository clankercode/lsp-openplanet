//! Type-checking scaffolding.
//!
//! This module is the entry point for the (in-progress) AngelScript static
//! type checker. It currently provides:
//!
//! * [`repr`] — canonical `TypeRepr` / `PrimitiveType` value types.
//! * [`global_scope`] — a merged read-only view of workspace + external
//!   symbol sources for lookup.
//! * [`call_site`] — arg binding + unique-arity overload pick (shared truth
//!   with signature help via `GlobalScope::callables_*`).
//! * [`resolver`] — turns parser `TypeExpr`s into resolved `TypeRepr`s,
//!   emitting diagnostics for unknown names.
//!
//! Full expression typing lives in [`checker`].

pub mod builtins;
pub mod call_site;
pub mod checker;
pub mod global_scope;
pub mod repr;
pub mod resolver;
pub mod workspace;

pub use call_site::{bind_arg, unique_overload_for_argc, ArgBind};
pub use checker::{Checker, TypeDiagnostic, TypeDiagnosticKind, TypeDiagnosticSeverity};
pub use global_scope::{GlobalScope, OverloadSig};
pub use repr::{PrimitiveType, TypeRepr};
pub use resolver::{ResolveDiagnostic, TypeResolver};
pub use workspace::build_plugin_symbol_table;
