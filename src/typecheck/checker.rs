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

use super::access_position::AccessPosition;
use super::builtins;
use super::call_site::{self, ArgBind};
use super::global_scope::{
    qualified_virtual_name, ExternalTypeName, FuncdefSig, GlobalScope, OverloadSig,
};
use super::repr::{param_mode_from_text, ParamMode, PrimitiveType, TypeRepr};
use super::resolver::TypeResolver;
use crate::lexer::token::TokenKind;
use crate::lexer::Span;
use crate::parser::ast::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeDiagnosticSeverity {
    Error,
    Warning,
}

/// Where a [`TypeDiagnosticKind::NoMatchingSymbol`] hint came from.
///
/// Both variants carry a fully qualified name and both read identically in
/// the message, but a consumer that turns the hint into an EDIT must be able
/// to tell them apart, and parsing the rendered prose to do it is forbidden
/// (see `code_actions::fixes::removed_api` for why). The distinction is
/// therefore structural.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolHint {
    /// The curated relocation table ([`removed_api_hint`]): a successor name
    /// verified against the live game, with the call shape unchanged. Exact.
    ///
    /// CURATED, NOT COMPUTED, and it cannot become computed. Half its rows
    /// (`Draw::*`) name a namespace that is absent from EVERY typedb
    /// snapshot the repo holds — 1.29.5 already lacks it — so there is no
    /// pair of dumps whose diff could produce them. The rows that a diff
    /// could reach are also not safe to derive: on 1.29.5 `Join` resolves in
    /// two namespaces (`Path::Join`, `string::Join`), so a same-simple-name
    /// search across the new dump is a coin flip between `Path::Join` and
    /// `Text::Join`, and only the much stronger "added in exactly this
    /// adjacent delta" filter disambiguates — which needs both neighbouring
    /// snapshots, precisely what the `Draw` rows do not have. And the CORE
    /// dump carries no usable deprecation metadata: a removed symbol simply
    /// vanishes. (The NADEO dump is different — B158 found structured,
    /// joinable member notes in `NadeoType.d`. The old claim covered "the
    /// dumps" and was only ever measured on Core.)
    /// `Net::SecureSocket` settles it from the third side — its
    /// real migration is `Net::Socket` plus a `Connect(secure: true)`
    /// argument, a shape change no name map can express, and it is knowable
    /// only from prose in the install's `Scripts/Compatibility.as`.
    ///
    /// # Core enum-value doc strings: MEASURED AND REJECTED (B134)
    ///
    /// A subset of Core enum-value `d` strings read as migration hints
    /// ("Please use Borders instead"), and they are the only
    /// deprecation-shaped text in that dump. B134 recorded them as a LEAD and
    /// asked for the population to be measured before anyone matched on them.
    /// Measured by `scripts/measure_enum_doc_migrations.py`, which is kept so
    /// a future dump can re-run it rather than re-derive this:
    ///
    /// | | 1.29.5 | 1.29.14 |
    /// |---|---|---|
    /// | enum values with a doc string | 206 | 208 |
    /// | ...containing a migration cue | 14 | 15 |
    /// | genuinely deprecated | 4 | 4 |
    /// | **the REPLACEMENT, flagged as deprecated** | **4** | **4** |
    /// | ordinary prose, not a migration | 6 | 7 |
    ///
    /// **The information is not recoverable, and the reason is structural
    /// rather than statistical.** A doc string is attached to a VALUE, and a
    /// deprecated flag and its replacement are ALIASES SHARING ONE NUMERIC
    /// VALUE, so both names carry the identical string:
    ///
    /// ```text
    /// UI::ChildFlags::Border   v=1  "Please use Borders instead, as Border is deprecated"
    /// UI::ChildFlags::Borders  v=1  "Please use Borders instead, as Border is deprecated"
    /// ```
    ///
    /// `Borders` is the name you are being told to ADOPT, and its own
    /// documentation says it is deprecated. All four migration pairs have this
    /// shape, so a matcher would warn on exactly the symbol the user should
    /// migrate TO — the polarity this crate treats as the expensive kind of
    /// wrong. Parsing the English harder does not help: the prose names the
    /// REPLACEMENT, never which of the two aliases was actually written.
    ///
    /// The stability question B134 asked has a different answer than it
    /// expected. Deprecation wording is STABLE — 0 of the shared cue hits
    /// changed between the two versions. What drifts is ordinary prose moving
    /// INTO the cue set: `UI::InputTextFlags::EnterReturnsTrue` was never
    /// deprecated, and gained a cue when upstream rewrote "Consider looking at
    /// the X function" as "Consider using X instead!". One engine bump, one
    /// new false positive, on a 206-string population. The failure mode is
    /// precision decay, not recall decay.
    ///
    /// So: **do not build a matcher on Core enum prose.** For deprecation data
    /// that can be trusted, use the Nadeo dump's structured member notes
    /// (B158) — 259 migration hints with a join key and a named replacement.
    Relocation(String),
    /// The nearest name under the namespace by edit distance (<= 2). A
    /// GUESS, offered to catch typos; it may name the wrong symbol.
    NearName(String),
}

impl SymbolHint {
    /// The qualified name being suggested, whichever producer suggested it.
    pub fn name(&self) -> &str {
        match self {
            Self::Relocation(name) | Self::NearName(name) => name,
        }
    }

    /// True for the curated table: an exact successor, safe to apply
    /// without the user reading it. A near-name guess is not.
    pub fn is_exact(&self) -> bool {
        matches!(self, Self::Relocation(_))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeDiagnosticKind {
    UnknownType(String),
    UndefinedIdentifier(String),
    /// B053: a bare enum-value name that two or more GLOBALLY VISIBLE engine
    /// enums carry. The game refuses to pick one — `Found multiple matching
    /// enum values` — where openplanet-lsp used to take the first hit and say
    /// nothing. `candidates` are the enum names, sorted, so the message can
    /// show the qualification that fixes it.
    AmbiguousEnumValue {
        name: String,
        candidates: Vec<String>,
    },
    UndefinedMember {
        object_type: String,
        member: String,
    },
    /// Removed-API usage on the member path (GH #27/#28 family). The
    /// member name compiles clean today but is a deprecated no-op at
    /// runtime (e.g. `awaitable.WithRunContext`).
    ///
    /// `hint` is the human sentence that goes in the message. `replacement`
    /// is the same migration as DATA, for the quick fix (B036): `Some(name)`
    /// only where swapping the member span alone is a correct migration —
    /// i.e. the member was renamed and the call keeps its shape. `None`
    /// wherever the migration moves the receiver, reorders arguments, or
    /// otherwise changes the call, because a span swap would then produce
    /// code that compiles and silently does the wrong thing. Never parse
    /// `hint` to recover this: prose is for humans, the edit needs data.
    RemovedApiUsage {
        member: String,
        hint: String,
        replacement: Option<String>,
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
    /// Game parity (2026-08-23, spike-live-add-kinematic-ao HatDump.as:60):
    /// invoking a non-const method on a `const T &in` receiver fails the
    /// game compile — `No matching signatures to 'T::M() const'`. Only
    /// workspace-local classes have reliable const metadata (external
    /// typedb methods don't carry a const flag), so the rule is scoped
    /// there. Const-method calls and non-const receivers are legal.
    NonConstMethodOnConstReceiver {
        method: String,
        object_type: String,
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
    /// Game parity (todo-cover-cases 2026-08-25): a second local of the
    /// same name in the SAME function frame. Distinct from
    /// [`Self::VariableShadow`] (inner block). Game wording:
    /// `'cp0' is already declared`. The first binding stays in scope so
    /// later member access uses the original type.
    AlreadyDeclared {
        name: String,
    },
    /// Game parity (GH #37): statement(s) after a terminating statement
    /// (return/break/continue, or an if-else where both branches terminate)
    /// inside the same block. The game warns `Unreachable code` once per
    /// run at the first unreachable statement.
    UnreachableCode,
    /// GH #49: no implicit conversion exists between distinct math value
    /// types (`int3 size = map.Size;` — nat3 → int3). Game wording:
    /// `Can't implicitly convert from 'const nat3' to 'int3'.`
    ///
    /// B171: the engine's TRAILING `&` on the target is CONDITIONAL and we
    /// deliberately do not reproduce it. Measured over 10 conversion messages
    /// (`docs/probes/b171-conversion-target-spelling.tsv`), the target carries
    /// `&` only when all three of: the source is non-const, the target type is
    /// not a primitive, and the slot is an assignable variable rather than a
    /// return. Two of those — source constness and slot kind — are not
    /// available here, so emitting `&` unconditionally (B160) was wrong for
    /// every conversion from a const source, which is what an engine property
    /// read such as `map.Size` is. The unsuffixed form is byte-exact with the
    /// engine for const sources, return slots and primitive targets.
    ///
    /// The trailing full stop IS unconditional: all 10 measured messages carry
    /// it, as did the 22 B160 measured. That half of B160 stands.
    NoImplicitConversion {
        from: String,
        to: String,
    },
    /// GH #47 / #49: no overload (function or value-type constructor)
    /// matches the argument types. Game wording:
    /// `No matching signatures to 'UI::InputText(const string, string, const bool)'`.
    NoMatchingSignatures {
        text: String,
    },
    /// GH #52 (repro 2): ternary branches with no unifiable common type.
    /// Game wording (RemoteBuild probes 2026-08-18), e.g. `c ? "" : wstr`
    /// (string vs wstring), `c ? 1 : true`, `c ? nat3v : int3v`:
    /// `Can't find unambiguous implicit conversion to make both
    /// expressions have the same type`.
    TernaryNoCommonType,
    /// GH #28 / #27 / #58: a free-function call `Ns::Fn(...)` the game
    /// rejects with `No matching symbol 'Ns::Fn'`. Four high-confidence
    /// triggers. Typedb-driven: (a) `Ns` is a KNOWN API namespace (appears
    /// as functions[].ns in the typedb — e.g. `UI` with 237 fns on 1.29.5)
    /// but `Ns::Fn` is not among its functions; (b) `Ns` is absent from
    /// the typedb entirely AND is a known removed/game API namespace
    /// (relocation table, e.g. `Draw` → `Display`). Workspace-driven
    /// (GH #58): (c) `Ns` is a namespace declared in a resolved
    /// dependency's export surface — exactly the files the game compiles
    /// into a consumer — and nothing named `Fn` sits under it; (d) `Ns` is
    /// a namespace declared in the plugin's own module with the same
    /// result. (c) and (d) share one implementation because the pooled
    /// table IS own-module + resolved dep exports; both require the
    /// dependency closure to be complete (see
    /// `workspace_ns_no_matching_symbol` for the full fence list).
    /// `hint` carries the known relocation (GH #27) or the nearest
    /// candidate name under the namespace.
    NoMatchingSymbol {
        qualified: String,
        hint: Option<SymbolHint>,
    },
    /// Get-only virtual property write (todo-cover-cases-next.txt,
    /// spike-live-add-kinematic-ao): `iso4.tx` has `get_tx()` but no
    /// `set_tx`. Game wording: `The property has no set accessor` (plus a
    /// second line for compound assigns: `Compound assignments with
    /// property accessors require both get and set accessors`).
    ReadOnlyProperty {
        property: String,
        compound: bool,
    },
    /// Value-assign through a const handle (todo-cover-cases-next.txt,
    /// spike-live-add-kinematic-ao): `auto ty = Reflection::GetType(…)`
    /// infers `const MwClassInfo@`; `ty = …` compiles to an assign through
    /// the reference, which the game rejects with
    /// `Reference is read-only` (Scene.as 258:28, OP 1.29.5). The `@ty =`
    /// handle rebind stays legal — the handle slot is mutable.
    ReadOnlyReference,
    /// B094: `class C : I` does not provide a method `I` declares.
    ///
    /// The engine's message is reproduced verbatim, including the expected
    /// signature, because measurement showed the engine reports a
    /// same-name-DIFFERENT-SIGNATURE method under this same message rather
    /// than as a distinct mismatch error — so the signature is the entire
    /// information content, and it is what tells a user whether the method
    /// is absent or merely declared wrong. Game-probed OP 1.29.5,
    /// 2026-08-31; see `typecheck::interface_impl`.
    MissingInterfaceImplementation {
        /// `void I::M(const string&in)` — engine spelling.
        signature: String,
    },
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
            TypeDiagnosticKind::AmbiguousEnumValue { name, candidates } => format!(
                "ambiguous enum value `{}`: {} enums define it ({}); qualify it, e.g. `{}::{}`",
                name,
                candidates.len(),
                candidates.join(", "),
                candidates.first().map(String::as_str).unwrap_or(""),
                name,
            ),
            TypeDiagnosticKind::UndefinedMember {
                object_type,
                member,
            } => format!("type `{}` has no member `{}`", object_type, member),
            // `replacement` is for the quick fix, never for the message: the
            // published text must not move (fixture baselines depend on it).
            TypeDiagnosticKind::RemovedApiUsage { member, hint, .. } => format!(
                // Match the runtime deprecation text shape (OP 1.29.5
                // ScriptRuntime): "`awaitable.WithRunContext()` is no longer
                // valid. Use Meta::StartWithRunContext instead."
                "`awaitable.{member}()` is no longer valid. Use `{hint}` instead"
            ),
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
            TypeDiagnosticKind::NonConstMethodOnConstReceiver { method, object_type } => format!(
                // Match the game compiler's failure class ("No matching
                // signatures to 'T::M() const'") while naming the fix.
                "no matching signatures to '{}::{}() const' — add a trailing `const` to the method or take a non-const reference",
                object_type, method
            ),
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
            TypeDiagnosticKind::AlreadyDeclared { name } => {
                format!("'{name}' is already declared")
            }
            TypeDiagnosticKind::UnreachableCode => "Unreachable code".to_string(),
            TypeDiagnosticKind::NoImplicitConversion { from, to } => {
                format!("Can't implicitly convert from '{from}' to '{to}'.")
            }
            TypeDiagnosticKind::NoMatchingSignatures { text } => {
                format!("No matching signatures to '{text}'")
            }
            TypeDiagnosticKind::TernaryNoCommonType => {
                // Exact game-compiler wording (RemoteBuild probe 2026-08-18).
                "Can't find unambiguous implicit conversion to make both expressions have the same type".to_string()
            }
            TypeDiagnosticKind::NoMatchingSymbol { qualified, hint } => {
                // Game wording: `No matching symbol 'Draw::GetWidth'` (OP
                // 1.29.5). The hint appends the known relocation (GH #27).
                let mut m = format!("No matching symbol '{qualified}'");
                if let Some(h) = hint {
                    let h = h.name();
                    m.push_str(&format!(" — use `{h}` instead"));
                }
                m
            }
            TypeDiagnosticKind::ReadOnlyProperty { property, compound } => {
                // Game wording (OP 1.29.5, Tools.as 242:16). Compound
                // assigns get the second line too.
                let mut m = format!("The property '{property}' has no set accessor");
                if *compound {
                    m.push_str(
                        "; compound assignments with property accessors require both get and set accessors",
                    );
                }
                m
            }
            TypeDiagnosticKind::ReadOnlyReference => {
                // Game wording (OP 1.29.5, Scene.as 258:28).
                "Reference is read-only".to_string()
            }
            TypeDiagnosticKind::MissingInterfaceImplementation { signature } => {
                // Game wording, verbatim (OP 1.29.5, probed 2026-08-31).
                format!("Missing implementation of '{}'", signature)
            }
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
            | "Tests"
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

/// What a [`LocalDeclRecord`] binds. Lambda parameters are deliberately
/// absent: their signature is imposed by the funcdef the lambda is passed
/// to, so an unused one is not the author's choice to fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalDeclKind {
    /// A `VarDecl` statement inside a function body (block, loop init,
    /// switch case, lambda body — any depth).
    Local,
    /// A named parameter of a function/method declaration *with a body*.
    Param,
}

/// One local/parameter binding seen while checking, recorded for the
/// unused-locals analysis (GH #55) when
/// [`Checker::with_local_decl_recording`] is on.
///
/// The checker stays policy-free: it records every binding it walks and
/// the context the analysis needs to apply exemptions
/// (`crate::analysis::unused::locals`). Synthetic bindings with no source
/// span of their own (the `value` of a property `set` accessor) are never
/// recorded.
#[derive(Debug, Clone)]
pub struct LocalDeclRecord {
    /// Identifier as written.
    pub name: String,
    /// Span of the declared identifier.
    pub span: Span,
    pub kind: LocalDeclKind,
    /// Pooled-symbol-table name of the enclosing function: bare for a
    /// free function, `Ns::F` inside a namespace, `Ns::Class::method` for
    /// a method. Bindings inside a lambda are attributed to the function
    /// that hosts the lambda.
    pub function: String,
    /// True when the enclosing function is a free function at file top
    /// level (no namespace, no class) — the population the Openplanet
    /// callback exemption applies to.
    pub function_is_top_level_free: bool,
    /// True when the enclosing function carries `override`.
    pub function_is_override: bool,
    /// True when the enclosing function is a member of a `mixin class`.
    /// Mixin members splice into consuming classes by declaration, so
    /// their signature is imposed by whatever consumes them.
    pub function_owner_is_mixin: bool,
    /// True when the enclosing function's body holds ZERO statements
    /// (`{}` or `{ /* comment */ }`). A deliberate no-op stub still has
    /// callers passing arguments, so its parameters are not the author's
    /// to remove. `{ return; }` and `{ throw(...); }` are one statement
    /// each and do NOT qualify.
    pub function_body_is_empty: bool,
    /// End offset of the enclosing function body. Upper bound of the
    /// "mentioned again after its declaration" scan.
    pub function_body_end: u32,
}

/// Enclosing-function context, pushed while recording local declarations.
#[derive(Debug, Clone)]
struct LocalDeclFnCtx {
    function: String,
    is_top_level_free: bool,
    is_override: bool,
    owner_is_mixin: bool,
    body_is_empty: bool,
    body_end: u32,
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
    /// Workspace-qualified names recorded as *used* while checking this
    /// file — the write side of the unused-declaration analysis. `None`
    /// (the default) disables recording entirely so normal check passes
    /// pay nothing; `with_usage_recording()` opts in. Names follow the
    /// pooled symbol-table convention (bare for globals, qualified for
    /// namespace members, `Class::member` for class members). Names that
    /// resolve only against the external typedb are recorded too —
    /// harmless, since no workspace symbol exists to suppress.
    used_names: Option<std::collections::HashSet<String>>,
    /// Local/parameter bindings walked in this file — the declaration
    /// side of the unused-locals analysis (GH #55). `None` (the default)
    /// disables recording so normal check passes pay nothing;
    /// `with_local_decl_recording()` opts in.
    local_decls: Option<Vec<LocalDeclRecord>>,
    /// Enclosing-function context for `local_decls`, innermost last.
    /// Only maintained while recording, and only for declarations that
    /// have a body (a bodyless declaration binds nothing).
    local_decl_fn_stack: Vec<LocalDeclFnCtx>,
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
            function_frame_starts: Vec::new(),
            diagnostics: Vec::new(),
            expr_types: std::collections::HashMap::new(),
            span_expr_types: std::collections::HashMap::new(),
            record_types: false,
            seen_function_sigs: std::collections::HashSet::new(),
            arith_suppress_add_mix: 0,
            used_names: None,
            local_decls: None,
            local_decl_fn_stack: Vec::new(),
        }
    }

    /// Enable span→type recording for the query surface (GH #42).
    /// Diagnostics are still collected; callers that only want queries
    /// simply ignore them.
    pub fn with_type_recording(mut self) -> Self {
        self.record_types = true;
        self
    }

    /// Enable workspace usage recording for the unused-declaration
    /// analysis: every name that resolves against the workspace (or the
    /// external typedb) while checking is collected and exposed via
    /// [`Checker::take_used_names`]. Normal check passes keep the default
    /// (recording off) and pay nothing.
    pub fn with_usage_recording(mut self) -> Self {
        self.used_names = Some(std::collections::HashSet::new());
        self
    }

    /// Drain the recorded used-name set (requires
    /// `with_usage_recording`). Empty when recording was never enabled.
    pub fn take_used_names(&mut self) -> std::collections::HashSet<String> {
        self.used_names.take().unwrap_or_default()
    }

    /// Enable local/parameter declaration recording for the unused-locals
    /// analysis (GH #55). Independent of `with_usage_recording`, though
    /// the analysis turns both on in a single pass.
    pub fn with_local_decl_recording(mut self) -> Self {
        self.local_decls = Some(Vec::new());
        self
    }

    /// Drain the recorded local/parameter declarations (requires
    /// `with_local_decl_recording`). Empty when recording was never on.
    pub fn take_local_decls(&mut self) -> Vec<LocalDeclRecord> {
        self.local_decls.take().unwrap_or_default()
    }

    /// Record one local/parameter binding. No-op unless recording is on
    /// and an enclosing function context exists (a body-less declaration
    /// pushes no context, so its parameters are never recorded).
    fn record_local_decl(&mut self, name: &str, span: Span, kind: LocalDeclKind) {
        if self.local_decls.is_none() {
            return;
        }
        let Some(ctx) = self.local_decl_fn_stack.last() else {
            return;
        };
        let record = LocalDeclRecord {
            name: name.to_string(),
            span,
            kind,
            function: ctx.function.clone(),
            function_is_top_level_free: ctx.is_top_level_free,
            function_is_override: ctx.is_override,
            function_owner_is_mixin: ctx.owner_is_mixin,
            function_body_is_empty: ctx.body_is_empty,
            function_body_end: ctx.body_end,
        };
        if let Some(out) = &mut self.local_decls {
            out.push(record);
        }
    }

    /// Pooled-table name of a function declared in the current
    /// namespace/class context (`Ns::Class::method`, bare for a free
    /// function). `ClassCtx::name` is already namespace-qualified.
    fn qualified_function_name(&self, simple: &str) -> String {
        if let Some(cls) = self.class_stack.last() {
            return format!("{}::{}", cls.name, simple);
        }
        if self.namespace_stack.is_empty() {
            simple.to_string()
        } else {
            format!("{}::{}", self.namespace_stack.join("::"), simple)
        }
    }

    /// Record a single workspace-qualified name as used (no-op unless
    /// usage recording is on). Every `::`-prefix of the name is recorded
    /// too: using `Foo::StaticMethod` uses the class `Foo`, and using
    /// `Color::Red` uses the enum type `Color`.
    fn record_use(&mut self, name: &str) {
        if let Some(set) = &mut self.used_names {
            set.insert(name.to_string());
            let mut start = 0;
            while let Some(off) = name[start..].find("::") {
                set.insert(name[..start + off].to_string());
                start += off + 2;
            }
        }
    }

    /// Record every named type inside a resolved `TypeRepr` as used —
    /// type-position references (`Foo@ x`, `array<Foo>`, `funcdef`
    /// signatures, base classes) all count as uses of `Foo`.
    fn record_type_repr_uses(&mut self, repr: &TypeRepr) {
        if self.used_names.is_none() {
            return;
        }
        let mut names = Vec::new();
        fn collect(repr: &TypeRepr, names: &mut Vec<String>) {
            match repr {
                TypeRepr::Named(n) => names.push(n.clone()),
                TypeRepr::Funcdef(n) => names.push(n.clone()),
                TypeRepr::Generic { base, args } => {
                    names.push(base.clone());
                    for a in args {
                        collect(a, names);
                    }
                }
                TypeRepr::Handle(inner) | TypeRepr::Const(inner) | TypeRepr::Array(inner) => {
                    collect(inner, names);
                }
                _ => {}
            }
        }
        collect(repr, &mut names);
        for n in names {
            self.record_use(&n);
        }
    }

    /// Record `name`, `get_name`, and `set_name` (virtual-property
    /// accessor convention — `lookup_class_member` matches all three).
    fn record_value_use_variants(&mut self, name: &str) {
        if self.used_names.is_none() {
            return;
        }
        self.record_use(name);
        // B061: the accessor prefix belongs on the LAST segment, not the
        // front of the whole path. `format!("get_{}", "Ns::P")` produced
        // `get_Ns::P`, a name nothing can ever match, so a qualified
        // property read never credited `Ns::get_P` and the accessor was
        // reported unused however many times it was read.
        //
        // The namespace ladder needs the same treatment for the other
        // spelling: a bare `P` written inside `namespace Ns` records
        // `get_P`, and `is_used` deliberately refuses to trust a bare tail
        // as a suffix match, so it cannot reach `Ns::get_P` either.
        // Recording every enclosing depth over-marks rather than
        // under-marks, which is this module's stated preference — a false
        // "unused" is far worse than a missed warning.
        for spelling in self.value_use_spellings(name) {
            self.record_use(&qualified_virtual_name(&spelling, "get_"));
            self.record_use(&qualified_virtual_name(&spelling, "set_"));
        }
    }

    /// Every name under which a written value reference could have been
    /// declared: as written, then qualified by each enclosing namespace
    /// depth, innermost outward.
    fn value_use_spellings(&self, name: &str) -> Vec<String> {
        let mut out = vec![name.to_string()];
        for depth in (1..=self.namespace_stack.len()).rev() {
            out.push(format!(
                "{}::{}",
                self.namespace_stack[..depth].join("::"),
                name
            ));
        }
        out
    }

    /// Record member usage for an explicit receiver whose base class name
    /// is `type_name` (`obj.member`): marks `Type::member` used on every
    /// class in the inheritance chain that declares it (workspace-wide,
    /// so cross-file parents and dep-export bases are covered). Recording
    /// all declaring ancestors over-marks rather than under-marks — a
    /// false "unused" is far worse than a missed warning.
    fn record_member_use_from_type(&mut self, type_name: &str, member: &str) {
        if self.used_names.is_none() {
            return;
        }
        let normalized = self.resolve_recording_class_name(type_name);
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::from([normalized]);
        let mut hops = 0usize;
        while let Some(class) = queue.pop_front() {
            if !visited.insert(class.clone()) {
                continue;
            }
            hops += 1;
            if hops > 64 {
                break;
            }
            for candidate in [
                member,
                &format!("get_{}", member),
                &format!("set_{}", member),
            ] {
                let qualified = format!("{}::{}", class, candidate);
                if !self.scope.lookup_workspace(&qualified).is_empty() {
                    self.record_use(&qualified);
                }
            }
            for parent in self.scope.workspace_class_parents(&class) {
                queue.push_back(self.scope.normalize_class_name(&parent));
            }
        }
    }

    /// Record member usage resolved through implicit `this` (bare name
    /// inside a class body). Same chain-walking discipline as
    /// [`Self::record_member_use_from_type`], starting at the innermost
    /// enclosing class.
    fn record_implicit_member_use(&mut self, member: &str) {
        if self.used_names.is_none() {
            return;
        }
        let Some(cls) = self.current_class() else {
            return;
        };
        let start = cls.name.clone();
        self.record_member_use_from_type(&start, member);
    }

    /// Resolve a written type name to its pooled-table form for usage
    /// recording, preferring WORKSPACE hits with the enclosing namespace
    /// restored (`Status` inside `namespace Repro` → `Repro::Status`).
    /// Unlike `GlobalScope::normalize_class_name`, workspace candidates
    /// are tried BEFORE the external typedb — a plugin's own class that
    /// shares a short name with an engine type must record against the
    /// plugin's decl (GH #38 shadowing semantics).
    fn resolve_recording_class_name(&self, type_name: &str) -> String {
        if type_name.contains("::") {
            if !self.scope.lookup_workspace_members(type_name).is_empty()
                || !self.scope.lookup_workspace(type_name).is_empty()
            {
                return type_name.to_string();
            }
            return self.scope.normalize_class_name(type_name);
        }
        for depth in (1..=self.namespace_stack.len()).rev() {
            let candidate = format!(
                "{}::{}",
                self.namespace_stack[..depth].join("::"),
                type_name
            );
            if !self.scope.lookup_workspace_members(&candidate).is_empty()
                || !self.scope.lookup_workspace(&candidate).is_empty()
            {
                return candidate;
            }
        }
        if !self.scope.lookup_workspace_members(type_name).is_empty()
            || !self.scope.lookup_workspace(type_name).is_empty()
        {
            return type_name.to_string();
        }
        self.scope.normalize_class_name(type_name)
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

    /// B119: the parents of `class`, from whichever surface knows it.
    /// Same-file classes come from `file_classes`; anything else from the
    /// pooled workspace symbol.
    ///
    /// B114 note: `file_classes` SURVIVES the sidecar retirement and this is
    /// a legitimate use of it. It carries resolved `TypeRepr`s and the parent
    /// list for the file being checked — structure, not a declaration
    /// MODIFIER — so it is not a second source of truth for anything the
    /// pool answers.
    fn class_parents_for_const(&self, class: &str) -> Vec<String> {
        if let Some((parents, _)) = self.file_classes.get(class) {
            return parents.clone();
        }
        self.scope.workspace_class_parents(class)
    }

    /// B119: is `method` const, as found on `class`'s ANCESTORS?
    ///
    /// Starts at the PARENTS, never at `class` itself, so a method the class
    /// declares is left entirely to the own-class arms and can never be
    /// diagnosed twice. Breadth-first, so a nearer ancestor's override wins
    /// over a more distant declaration — which is the same precedence
    /// `cross_file_derived_override_decides_constness` pins one level down.
    ///
    /// `None` means no ancestor declares the name in the WORKSPACE. That is
    /// also the answer for an engine/typedb ancestor, because the lookup here
    /// reads the workspace half only — `workspace_own_method_is_const` reads
    /// the pooled workspace symbols. Same provenance rule B116 established
    /// for the own-class cross-file arm, applied one inheritance level out.
    ///
    /// NOT because the dumps lack the data. B124 corrected that: 301 of 1002
    /// Core method decls end in ` const`, and the old
    /// "no const-qualifier metadata yet" comment was stale — it had been
    /// steering decisions, including part of B116's provenance argument. The
    /// engine population is excluded here by CHOICE of lookup, not by absence
    /// of metadata, and consuming it is B130's job precisely because it opens
    /// a false-positive surface that needs its own variant sweep.
    ///
    /// Measured against the game (`scripts/probe_const_receiver.py`): the
    /// engine rejects an inherited NON-const method on a const receiver at
    /// depths 1, 2 and 4 and for every receiver spelling, and accepts an
    /// inherited CONST one. So the walk is the engine's own rule, not a
    /// tightening chosen here.
    ///
    /// # B114: one lookup per ancestor, not two
    ///
    /// As written, this asked each ancestor twice: `file_has_method` +
    /// `file_const_methods` for a SAME-FILE ancestor, then
    /// `workspace_own_method_is_const` for anything else. Retiring those two
    /// AST sidecars collapses the pair into the pooled call alone, which
    /// already spans both cases because the file being checked is itself in
    /// the pool.
    ///
    /// The collapse is behaviour-preserving, and the reason is specific
    /// rather than general: `index_file_classes` inserted BOTH the direct
    /// method name and the accessor's STRIPPED property name into both maps
    /// under the same key, so the sidecar pair was already answering the
    /// AGGREGATED question — "is the direct method, or `get_`/`set_` of that
    /// name, const?". `workspace_own_method_is_const` aggregates exactly
    /// those three spellings. For `class A { void M() {} int get_M() const {} }`
    /// both surfaces answer `Some(true)`.
    ///
    /// So the ported helper reads the AGGREGATED answer deliberately, not the
    /// direct spelling only. Reading direct-only would have been a change,
    /// not a port: it would diverge from what this walk returned for a
    /// same-file ancestor, and it would make the two halves of B119's own
    /// walk disagree — the precise thing its provenance argument sets out to
    /// avoid. It therefore inherits the known false negative recorded on
    /// `workspace_own_method_is_const`, one inheritance level out; splitting
    /// the call spelling from the property spelling remains a separate,
    /// separately measurable improvement.
    fn inherited_method_is_const(&self, class: &str, method: &str) -> Option<bool> {
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        visited.insert(class.to_string());
        let mut queue: std::collections::VecDeque<String> =
            self.class_parents_for_const(class).into_iter().collect();
        let mut depth_guard = 0usize;
        while let Some(ancestor) = queue.pop_front() {
            depth_guard += 1;
            if depth_guard > 64 {
                // A cyclic `class A : B` / `class B : A` is invalid, but the
                // checker must not hang on invalid input.
                break;
            }
            if !visited.insert(ancestor.clone()) {
                continue;
            }
            if let Some(is_const) = self.scope.workspace_own_method_is_const(&ancestor, method) {
                return Some(is_const);
            }
            for p in self.class_parents_for_const(&ancestor) {
                queue.push_back(p);
            }
        }
        None
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
                                // B114: the trailing `const` and the
                                // method-vs-field discrimination used to be
                                // recorded here, into `file_const_methods`
                                // and `file_class_methods`. Both are gone.
                                // They were a per-file AST answer to a
                                // question the pool can answer for any file,
                                // and once B116 added a pooled path beside
                                // them the codebase held TWO answers to
                                // "is this method const" — the drift this
                                // ticket exists to prevent.
                                // `GlobalScope::workspace_own_method_is_const`
                                // is now the only one.
                                //
                                // What remains here is purely the RETURN-TYPE
                                // member list, which is legitimately
                                // file-local: it carries resolved `TypeRepr`s
                                // for the file being checked, not a modifier.
                                //
                                // AngelScript virtual-property accessors:
                                // `get_X()`/`set_X()` expose member `X`, so
                                // register the PROPERTY name as present too
                                // (the accessor itself is also kept). This is
                                // what lets `q.f0` resolve when the class
                                // only declares `get_f0`/`set_f0` (E++
                                // GameQuat, ClickFlags real-world shapes).
                                if let Some(prop) = method_name
                                    .strip_prefix("get_")
                                    .or_else(|| method_name.strip_prefix("set_"))
                                {
                                    if !prop.is_empty() {
                                        members.push((prop.to_string(), ret.clone()));
                                    }
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
        // Same-frame redecl (todo-cover-cases 2026-08-25): game
        // `'name' is already declared`. Do not replace the first
        // binding — later member access must keep the original type.
        if self
            .frames
            .last()
            .is_some_and(|f| f.locals.iter().any(|l| l.name == name))
        {
            self.diagnostics.push(TypeDiagnostic {
                span,
                kind: TypeDiagnosticKind::AlreadyDeclared { name },
            });
            return;
        }
        // GH #37: local-vs-local shadowing inside the SAME function —
        // warn when an enclosing frame of this function already holds the
        // name (params live in the outermost function frame, so a local
        // shadowing a param warns too). Class members and globals never
        // participate.
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
        // Type-position reference: every named type inside the resolved
        // repr counts as a use (unused-decl analysis).
        self.record_type_repr_uses(&repr);
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
            Item::Typedef(td) => {
                let _ = self.resolve_type_expr(&td.target_type);
            }
            Item::VarDecl(var) => self.check_var_decl_global(var),
            Item::Property(prop) => {
                let prop_ty = self.resolve_type_expr(&prop.type_expr);
                let prop_name = prop.name.text(self.source).to_string();
                if let Some(body) = &prop.getter {
                    self.check_property_accessor_body(&prop_name, body, None);
                }
                if let Some((_, body)) = &prop.setter {
                    self.check_property_accessor_body(&prop_name, body, Some((prop_ty, prop.span)));
                }
            }
            Item::Import(_) | Item::Error(_) => {}
        }
    }

    fn check_class_decl(&mut self, cls: &ClassDecl) {
        for base in &cls.base_classes {
            // Base-class references are type uses (unused-decl) — recorded
            // here so even a fully-unreferenced class contributes them.
            let repr = self.resolve_type_expr(base);
            self.record_type_repr_uses(&repr);
        }

        // Build the class context up front so every method sees the full
        // set of sibling members (including ones declared after it).
        let simple_class_name = cls.name.text(self.source).to_string();
        let class_name = if self.namespace_stack.is_empty() {
            simple_class_name.clone()
        } else {
            format!("{}::{}", self.namespace_stack.join("::"), simple_class_name)
        };
        // B094: interface methods this class declares but does not provide.
        // One diagnostic per missing method, spanned at the class NAME —
        // that is where the engine points for a plain class, and it is the
        // anchor B095's "implement missing methods" action needs.
        for missing in
            crate::typecheck::interface_impl::missing_interface_methods(&class_name, self.scope)
        {
            self.diagnostics.push(TypeDiagnostic {
                span: cls.name.span,
                kind: TypeDiagnosticKind::MissingInterfaceImplementation {
                    signature: missing.signature,
                },
            });
        }

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
            name: class_name.clone(),
            is_mixin: cls.is_mixin,
            members,
        });

        // Mixin classes are consumed structurally: their members splice
        // into consuming classes by declaration, not by name reference.
        // The unused-decl analysis must never warn on them — mark the
        // mixin class and every member used.
        if cls.is_mixin {
            self.record_use(&class_name);
            for member in &cls.members {
                let mname = match member {
                    ClassMember::Field(var) => var
                        .declarators
                        .iter()
                        .map(|d| d.name.text(self.source).to_string())
                        .collect::<Vec<_>>(),
                    ClassMember::Method(f)
                    | ClassMember::Constructor(f)
                    | ClassMember::Destructor(f) => vec![f.name.text(self.source).to_string()],
                    ClassMember::Property(prop) => vec![prop.name.text(self.source).to_string()],
                };
                for n in mname {
                    self.record_use(&format!("{}::{}", class_name, n));
                }
            }
        }

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
                let ty = self.resolve_type_expr(&var.type_expr);
                self.record_type_repr_uses(&ty);
                for d in &var.declarators {
                    if let Some(init) = &d.init {
                        let init_ty = self.expr_type(init);
                        self.check_funcdef_binding(&ty, init, &init_ty);
                        self.check_enum_conversion(&ty, init, &init_ty);
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
                let prop_name = prop.name.text(self.source).to_string();
                if let Some(body) = &prop.getter {
                    self.check_property_accessor_body(&prop_name, body, None);
                }
                if let Some((_, body)) = &prop.setter {
                    self.check_property_accessor_body(&prop_name, body, Some((prop_ty, prop.span)));
                }
            }
        }
    }

    /// Check one virtual-property accessor under the same local-declaration
    /// context as an ordinary function body. The setter's synthetic `value`
    /// is defined for type checking only; it has no source declaration and is
    /// intentionally never passed to `record_local_decl`.
    fn check_property_accessor_body(
        &mut self,
        property_name: &str,
        body: &FunctionBody,
        setter_value: Option<(TypeRepr, Span)>,
    ) {
        self.push_frame();
        if let Some((ty, span)) = setter_value {
            self.define_local("value".to_string(), ty, span);
        }
        let recording_ctx = self.local_decls.is_some();
        if recording_ctx {
            self.local_decl_fn_stack.push(LocalDeclFnCtx {
                function: self.qualified_function_name(property_name),
                is_top_level_free: false,
                is_override: false,
                owner_is_mixin: self.class_stack.last().is_some_and(|c| c.is_mixin),
                body_is_empty: body.stmts.is_empty(),
                body_end: body.span.end,
            });
        }
        self.check_function_body(body);
        if recording_ctx {
            self.local_decl_fn_stack.pop();
        }
        self.pop_frame();
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
        // A function's return type and parameter types are uses of the
        // named types inside them (unused-decl analysis) — recording here
        // covers *unreferenced* functions too, whose bodies never run.
        self.record_type_repr_uses(&ret_ty);
        self.return_type_stack.push(ret_ty.clone());
        self.function_frame_starts.push(self.frames.len());
        self.push_frame();
        // Unused-locals recording (GH #55): only declarations WITH a body
        // bind anything, so a bodyless declaration pushes no context and
        // its parameters are never recorded.
        let recording_ctx = self.local_decls.is_some() && func.body.is_some();
        if recording_ctx {
            let simple = func.name.text(self.source).to_string();
            let ctx = LocalDeclFnCtx {
                function: self.qualified_function_name(&simple),
                is_top_level_free: self.class_stack.is_empty() && self.namespace_stack.is_empty(),
                is_override: func.is_override,
                owner_is_mixin: self.class_stack.last().is_some_and(|c| c.is_mixin),
                body_is_empty: func.body.as_ref().is_some_and(|b| b.stmts.is_empty()),
                body_end: func.body.as_ref().map_or(func.span.end, |b| b.span.end),
            };
            self.local_decl_fn_stack.push(ctx);
        }
        for p in &func.params {
            self.warn_string_by_value_param(p);
            let ty = self.resolve_type_expr(&p.type_expr);
            self.record_type_repr_uses(&ty);
            if let Some(name) = &p.name {
                self.define_local(name.text(self.source).to_string(), ty, name.span);
                self.record_local_decl(name.text(self.source), name.span, LocalDeclKind::Param);
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
        if recording_ctx {
            self.local_decl_fn_stack.pop();
        }
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
        let ty = self.resolve_type_expr(&var.type_expr);
        // Global variable types count as type uses even when the variable
        // itself is never referenced (unused-decl analysis).
        self.record_type_repr_uses(&ty);
        for d in &var.declarators {
            if let Some(init) = &d.init {
                let init_ty = self.expr_type(init);
                self.check_funcdef_binding(&ty, init, &init_ty);
                self.check_enum_conversion(&ty, init, &init_ty);
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
                    self.check_funcdef_binding(&declared_ty, init, &init_ty);
                    self.check_enum_conversion(&declared_ty, init, &init_ty);
                }
                declared_ty.clone()
            };
            self.define_local(d.name.text(self.source).to_string(), local_ty, d.name.span);
            self.record_local_decl(d.name.text(self.source), d.name.span, LocalDeclKind::Local);
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
        // GH #49: distinct math value types (nat3/int3/vec*/iso*/quat) have
        // no implicit conversion — the game rejects the initializer with
        // `Can't implicitly convert from 'const nat3' to 'int3&'.`
        if let (TypeRepr::Named(target_n), TypeRepr::Named(source_n)) = (target, source) {
            if is_math_value_type(target_n) && is_math_value_type(source_n) && target_n != source_n
            {
                self.diagnostics.push(TypeDiagnostic {
                    span: init.span,
                    kind: TypeDiagnosticKind::NoImplicitConversion {
                        from: init_ty.display(),
                        to: target_n.to_string(),
                    },
                });
                return;
            }
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

    /// B071: type a qualified enum-value reference (`LocalOne::X`,
    /// `VirtualKey::A`, `UI::Plot::Marker::Circle`) as its ENUM, and report a
    /// member the enum never declared.
    ///
    /// Before this, `E::V` typed as `Error` whatever `V` was, and that single
    /// fact is why the whole ticket was silent in both directions at once: an
    /// undeclared member looked exactly like a declared one, and an
    /// enum-typed initializer looked like an untypeable expression, so every
    /// conversion check downstream bailed on the `Error` before it could
    /// compare anything.
    ///
    /// Scoped to RESOLVING the reference. It deliberately does not decide
    /// whether the enum may be assigned to whatever it is being assigned to —
    /// that is [`Self::check_enum_conversion`] — and keeping the two apart is
    /// what keeps cross-enum ARITHMETIC legal. `Binary` discards its operand
    /// types, so two different enums in one expression never reach a
    /// comparison; live probe says `LocalOne::X + LocalTwo::Y` compiles
    /// (both decay to `int`), so a rule that fired on "two enum types in one
    /// expression" would be reporting an error on code the game accepts.
    fn enum_member_type(&mut self, qual: &str, span: Span) -> TypeRepr {
        let Some((prefix, member)) = qual.rsplit_once("::") else {
            return TypeRepr::Error(String::new());
        };
        // Innermost enclosing namespace first, matching the value-use walk
        // just above: inside `namespace N`, a written `Inner::X` names
        // `N::Inner::X`.
        for depth in (0..=self.namespace_stack.len()).rev() {
            let spelling = if depth == 0 {
                prefix.to_string()
            } else {
                format!("{}::{}", self.namespace_stack[..depth].join("::"), prefix)
            };
            // `None` is "cannot enumerate the members", never "has none" —
            // an anonymous engine enum (B016) lands there and must stay
            // silent rather than have all of its members called misspelt.
            let Some(members) = self.scope.enum_member_names(&spelling) else {
                continue;
            };
            if members.iter().any(|m| m == member) {
                return TypeRepr::Named(spelling);
            }
            // Near-name hint, exactly like the namespace-member path: purely
            // additive, and it never gates the diagnostic.
            let hint = members
                .iter()
                .filter(|m| edit_distance(m, member) <= 2)
                .min_by_key(|m| edit_distance(m, member))
                .map(|m| SymbolHint::NearName(format!("{spelling}::{m}")));
            self.diagnostics.push(TypeDiagnostic {
                span,
                kind: TypeDiagnosticKind::NoMatchingSymbol {
                    qualified: qual.to_string(),
                    hint,
                },
            });
            return TypeRepr::Error(String::new());
        }
        TypeRepr::Error(String::new())
    }

    /// B071: assignment / initialisation compatibility for ENUM targets, at
    /// every position a value is bound to a declared type.
    ///
    /// A SIBLING of [`Self::check_init_conversion`], not an extension of it.
    /// That method is reached from exactly one of the four initializer
    /// positions — `check_var_decl_local`; globals, class fields and
    /// assignments never see it — and that asymmetry is precisely what B076
    /// turned out to be, one ticket earlier. This is called from all four
    /// plus plain assignment, and the `position_*` tests in
    /// `tests/b071_enum_type_mismatch.rs` are the fence that keeps it so.
    ///
    /// Two rules, each measured rather than reasoned (RemoteBuild probe, one
    /// compilation unit per fixture — never batched, B068):
    ///
    /// * enum -> a DIFFERENT enum is rejected:
    ///   `Can't implicitly convert from 'LocalOne' to 'LocalTwo&'.`
    /// * `int` -> enum is rejected:
    ///   `Can't implicitly convert from 'int' to 'VirtualKey&'.`
    ///
    /// # The engine's wording moved, the rule did not (B148)
    ///
    /// Both messages above gained a trailing `&` on the TARGET type between
    /// the 1.29.5-era measurement and 1.29.14: the engine names the assignment
    /// target as a reference. `docs/probes/b071-enum-conversion.tsv` records
    /// three rows as WORDING-CHANGED and ZERO as behaviour-changed, so the two
    /// rules are unaltered and nothing about this function's logic follows
    /// from the difference.
    ///
    /// B160 resolved the product decision in favour of exact engine spelling:
    /// our renderer now includes both the target-reference `&` and the final
    /// full stop. The 15 exact-string assertions in
    /// `b071_enum_type_mismatch` and `cli_check_tests` pin that choice.
    ///
    /// B155/B160 keep the relationship CHECKED rather than merely recorded:
    /// `tests/b155_engine_wording_divergence.rs` derives the engine's spelling
    /// from the probe corpora (22 of 22 measured messages carry BOTH marks,
    /// across b071 and b144 when B160 landed). B160 inverted the old drop
    /// transform: it adds both marks to the former local spelling, must
    /// reproduce a measured engine row character-for-character, and then
    /// requires the live renderer to equal that row.
    ///
    /// The converse of the second is LEGAL — `int i = LocalOne::X;` compiles,
    /// because an enum value decays to `int`. That asymmetry is the trap: a
    /// rule phrased as "enums and ints are interchangeable" gets one of the
    /// two backwards whichever way it guesses. Nothing here looks at a
    /// non-enum target, so the legal direction is silent by construction and
    /// not by a carve-out that a later edit could drop.
    fn check_enum_conversion(&mut self, declared: &TypeRepr, init: &Expr, init_ty: &TypeRepr) {
        let target = declared.unwrap_const();
        let source = init_ty.unwrap_const();
        let TypeRepr::Named(target_n) = target else {
            return;
        };
        if !self.scope.has_enum(target_n) {
            return;
        }
        let from = match source {
            // A different enum. `named_types_equivalent` canonicalizes, so
            // the two accepted spellings of ONE enum — `VirtualKey` and the
            // ancestor walk's `UI::VirtualKey` (B052) — compare equal and
            // stay silent.
            TypeRepr::Named(source_n)
                if self.scope.has_enum(source_n)
                    && !self.named_types_equivalent(source_n, target_n) =>
            {
                source_n.clone()
            }
            // An integer. Floats and bools are deliberately left alone: no
            // probe covers them, and this ticket's measured population is
            // the integer family.
            TypeRepr::Primitive(p) if is_integer(*p) => source.display(),
            _ => return,
        };
        self.diagnostics.push(TypeDiagnostic {
            span: init.span,
            kind: TypeDiagnosticKind::NoImplicitConversion {
                from,
                to: target_n.clone(),
            },
        });
    }

    /// GH #49 constructor form: the int/nat vector families have NO
    /// cross-type constructor — `int3(nat3_value)` is rejected by the game
    /// with `No matching signatures to 'int3(const nat3&)'`. Narrow rule,
    /// fleet-probed: only the nat↔int same-dimension family is flagged.
    /// Cross-type ctors that DO exist elsewhere (e.g. `quat(const vec3&)`,
    /// `quat(const mat3&)`) must stay legal. GH #54 indexed the typedb's
    /// `behaviors`, so a general "no single-arg ctor is registered" rule
    /// could drive this check — deliberately not taken, because it would
    /// start rejecting pairs no game probe has ruled on.
    fn check_math_value_ctor(&mut self, target: &TypeRepr, arg_tys: &[TypeRepr], span: Span) {
        let TypeRepr::Named(target_n) = target else {
            return;
        };
        if arg_tys.len() != 1 {
            return;
        }
        let Some(target_dim) = int_nat_family(target_n) else {
            return;
        };
        let source = arg_tys[0].unwrap_const();
        let TypeRepr::Named(source_n) = source else {
            return;
        };
        if let Some(source_dim) = int_nat_family(source_n) {
            if source_n != target_n && source_dim == target_dim {
                self.diagnostics.push(TypeDiagnostic {
                    span,
                    kind: TypeDiagnosticKind::NoMatchingSignatures {
                        text: format!("{target_n}(const {source_n}&)"),
                    },
                });
            }
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
    /// B121: can a METHOD's return value alias the receiver it was called
    /// on? Only an aliasing return may inherit the receiver's constness.
    ///
    /// A by-value return is a fresh object — `const string &in s` then
    /// `s.Replace(..)` hands back a NEW string that is perfectly
    /// assignable — so constness must not travel through it. A handle
    /// return MAY hand back a view into the receiver (`@member`), and
    /// nothing available says whether it does, so it is treated as
    /// aliasing: that is the conservative direction for a rule whose
    /// whole job is to notice writes through a const view.
    ///
    /// This is a SHAPE proxy and it is deliberately partial. It is sound
    /// in one direction only: a by-value return provably cannot alias, a
    /// handle return merely might. `string::Split` returns `string[]@` —
    /// a fresh array behind a handle — and no metadata in the dumps
    /// distinguishes it from `Json::Value::opIndex`, which returns a
    /// genuine view. See B121 for the measurement.
    ///
    /// NOT used for FIELD access, which is a different question with a
    /// different answer: a field read genuinely IS a view into the
    /// receiver whatever its shape, so `member_access_type` keeps
    /// propagating unconditionally.
    fn method_return_may_alias_receiver(t: &TypeRepr) -> bool {
        matches!(t, TypeRepr::Handle(_))
    }

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
        // Removed-API member names on engine types (GH #27/#28 family).
        // `awaitable.WithRunContext` compiles clean but is a silent
        // runtime no-op: the run context is discarded (game evidence
        // 2026-08-23, DipsItemExperiment / MLFeedRaceData). Checked
        // BEFORE any member lookup so it fires even where the typedb
        // lacks member metadata — but only for the engine type.
        if type_name == "awaitable"
            && member_name == "WithRunContext"
            && self.scope.is_external_type(&type_name)
        {
            self.diagnostics.push(TypeDiagnostic {
                span,
                kind: TypeDiagnosticKind::RemovedApiUsage {
                    member: member_name.clone(),
                    hint: "Meta::StartWithRunContext".to_string(),
                    // No mechanical replacement: `Meta::StartWithRunContext`
                    // is a free function taking the coroutine as an
                    // argument, so migrating `startnew(W).WithRunContext(c)`
                    // moves the receiver into the call. Swapping the member
                    // span would emit `Meta::StartWithRunContext(c)` and
                    // silently drop the coroutine. The migration is offered
                    // as a CALL RESTRUCTURE instead, by the
                    // `with_run_context` fix family (B048), which reads the
                    // shape off the AST — this field stays `None` because
                    // it means "a span swap is correct", and here it is not.
                    replacement: None,
                },
            });
            return TypeRepr::Error(String::new());
        }
        // Same-file workspace classes: prefer the in-memory `file_classes`
        // index because it preserves full `TypeRepr` (including `Const`)
        // that `scope.lookup_member_type` strips to `Error("")` for
        // workspace hits. This is the const-wrapper preservation path
        // (iter 24) — do NOT reorder this below any other lookup.
        if let Some((parents, members)) = self.file_classes.get(&type_name) {
            for (mname, t) in members {
                if mname == &member_name {
                    return Self::apply_receiver_const(t.clone(), receiver_const);
                }
            }
            // Game parity (2026-08-23): the game compiler rejects unknown
            // members on script classes. Fire only for classes with NO
            // base classes — an unresolved parent could carry the member
            // (cross-file bases, mixin consumers list the mixin in their
            // bases: GH #46), and arrays/dictionaries/unresolvable
            // receivers all stay silent (FP-hostile). Classes WITH bases
            // fall through to the cross-file lookups below, which know
            // how to resolve inherited members.
            if parents.is_empty() {
                self.diagnostics.push(TypeDiagnostic {
                    span,
                    kind: TypeDiagnosticKind::UndefinedMember {
                        object_type: type_name.clone(),
                        member: member_name.clone(),
                    },
                });
                return TypeRepr::Error(String::new());
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
        // B081: the mint, not merely a guard. Its `Some` is what
        // `injected_method` below takes, so the shadow check and the use it
        // protects can no longer be separated by an edit.
        let Some(external_receiver) = self.scope.external_type_name(&type_name) else {
            return TypeRepr::Error(String::new());
        };
        // Nadeo-sourced types can have incomplete member metadata. When the
        // typedb lists zero properties/methods for the type, a failed
        // lookup may just mean the DB is incomplete — stay silent. When
        // the type has a non-empty member list, trust it and emit
        // UndefinedMember for missing names (B006).
        if self.scope.is_nadeo_type(&type_name) && !self.scope.nadeo_member_list_trusted(&type_name)
        {
            return TypeRepr::Error(String::new());
        }
        // Openplanet-injected methods (GH #21, B059): registered by the
        // runtime, never present in the Nadeo dump, so every lookup above
        // missed. This is the NON-call path (`p.GetCurrentEntityID` with no
        // argument list), which yields no value in AngelScript — silence is
        // all that's owed here. The call site is where the return type
        // matters; see `injected_method_return`.
        if self
            .injected_method(external_receiver, &member_name)
            .is_some()
        {
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

    /// The Openplanet-injected method `member_name` names on a receiver of
    /// type `type_name`, if any (GH #21, B059).
    ///
    /// These are methods the Openplanet runtime registers on engine classes
    /// but the Nadeo dump never lists, so every typedb lookup misses them
    /// and B006's trusted-member-list rule fires `UndefinedMember` on code
    /// the game compiles. Consulted at both member-access and call sites,
    /// which is why the data lives in one table rather than in `matches!`
    /// arms at each.
    /// B081: takes an [`ExternalTypeName`] rather than a `&str`, so the shadow
    /// check cannot be separated from the use it guards.
    ///
    /// Both callers were already correct, but only because an
    /// `is_external_type` guard fifteen-odd lines earlier — written for a
    /// different purpose, "only emit when the type is a known external type" —
    /// happened to dominate them. Measured, not assumed: a fixture declaring
    /// `class CSmPlayer {}` and calling `p.MwAddRef()` reports
    /// `type 'CSmPlayer' has no member 'MwAddRef'` on master. Correct by
    /// coincidence of control flow, invisible to a reader here, and one
    /// reordering away from the defect B063 shipped on the completion side.
    /// The guard is now the mint, so the two are one expression.
    fn injected_method(
        &self,
        receiver: ExternalTypeName<'_>,
        member_name: &str,
    ) -> Option<&'static builtins::InjectedMethod> {
        builtins::injected_methods_named(member_name).find(|m| match &m.receivers {
            builtins::InjectedReceiver::EveryEngineClass => {
                self.scope.external_type_derives_from(receiver, "CMwNod")
            }
            builtins::InjectedReceiver::Exact(names) => names
                .iter()
                .any(|n| self.scope.external_type_is(receiver, n)),
        })
    }

    /// Value an injected method's CALL yields.
    ///
    /// `void` rows carry no value. GH #21 has always returned the silence
    /// sentinel for `MwAddRef`/`MwRelease`, and `TypeRepr::Void` takes part
    /// in conversion rules the sentinel does not, so promoting them is a
    /// separate behaviour change. Every other row must return its REAL type:
    /// a sentinel would suppress the false positive while typing
    /// `auto entId = player.GetCurrentEntityID()` wrong, which is the worse
    /// and much less visible bug.
    fn injected_method_return(m: &builtins::InjectedMethod) -> TypeRepr {
        match TypeRepr::parse_type_string(m.return_type) {
            TypeRepr::Void => TypeRepr::Error(String::new()),
            t => t,
        }
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

    /// B100: the free-function OVERLOAD-SET entry point that the
    /// Ident-callee and `NamespaceAccess`-callee paths never had.
    ///
    /// `lookup_function_return` refuses as soon as two workspace symbols
    /// share a name — the deliberate "ambiguous, say nothing" arm — and
    /// every free-function route into
    /// [`Self::resolve_workspace_function_call`] was gated behind it. So a
    /// workspace free function with two or more overloads fell through to
    /// the `has_global_ident` arm, which suppresses the undefined-identifier
    /// diagnostic and only walks the arguments for their own errors:
    /// nothing compared them to any signature, not arity, not types, not
    /// funcdef slots. Workspace METHODS never had the hole, because they
    /// produce real sets via `lookup_method_overloads_with_inheritance`.
    ///
    /// B084 fixed the CREDITING half of this same arm; this is the CHECKING
    /// half. One restriction, two consequences.
    ///
    /// **Nothing new is decided here.** The set goes to the SAME resolver
    /// the single-overload and method paths use, so a free-function set
    /// answers with that resolver's existing rules rather than a second copy
    /// of them — including its deliberate silence on a TYPE-driven no-match,
    /// which is pinned in `tests/b100_free_function_overload_set.rs` against
    /// the method path rather than left to be rediscovered.
    ///
    /// The guard asks `method_overloads_including_inherited` — the very call
    /// the resolver makes — rather than a second lookup that could disagree
    /// with it about what the set contains.
    ///
    /// Credits and types the call EXACTLY as the `has_global_ident` arm it
    /// stands in front of: same `record_value_use_variants`, same
    /// `TypeRepr::Error` sentinel. Returning the resolver's own type instead
    /// would be a second, unmeasured change riding along on this one — the
    /// unique-overload winner's return type would start flowing into member
    /// chains and hover, which is not what this ticket measured.
    fn try_workspace_overload_set_call(
        &mut self,
        display_name: &str,
        qualified: &str,
        args: &[CallArg],
        callee_span: Span,
    ) -> Option<TypeRepr> {
        if self.method_overloads_including_inherited(qualified).len() < 2 {
            return None;
        }
        self.record_value_use_variants(qualified);
        self.resolve_workspace_function_call(
            display_name,
            qualified,
            args,
            callee_span,
            TypeRepr::Error(String::new()),
        );
        Some(TypeRepr::Error(String::new()))
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
                    self.check_external_call_arg_types(
                        display_name,
                        qualified,
                        args,
                        &external,
                        None,
                    );
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
                let arg_tys = self.type_args_once(args);
                if let Some(params) = self.scope.lookup_function_params(qualified) {
                    self.check_arg_types_against_params(display_name, args, &arg_tys, &params);
                }
                // B089: the funcdef-slot check the external call path has had
                // since B023. `lookup_function_params` returns the declared
                // param TEXT, which is what `slot_funcdef` needs, but the
                // primitive check above discards everything that is not a
                // primitive — a funcdef-typed parameter fell straight through
                // it.
                self.report_workspace_funcdef_slot_failure(qualified, args, &arg_tys, &overloads);
                fallback_ret
            }
            _ => {
                // 2+ overloads: walk args once, run real resolution.
                // Named-arg overload resolution is not implemented yet —
                // bind types in call order (positional) for matching.
                let arg_tys = self.type_args_once(args);
                // B089: run BEFORE `resolve_overload`, and independently of
                // its verdict — the set-level rule is "every arity-compatible
                // candidate is rejected by a funcdef slot", which is a
                // different question from whether any candidate wins on
                // primitive conversions.
                self.report_workspace_funcdef_slot_failure(qualified, args, &arg_tys, &overloads);
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

    /// If `qualified` names a funcdef, type-check `Ns::Name(fn)` /
    /// `Name(fn)` as a constructor and return `TypeRepr::Funcdef`.
    fn try_funcdef_ctor(
        &mut self,
        qualified: &str,
        args: &[CallArg],
        span: Span,
    ) -> Option<TypeRepr> {
        // GH #38 / B073: this runs BEFORE the workspace type constructor
        // (`call_type`'s `has_type` branch below it), and `lookup_funcdef`
        // falls through a workspace declaration to the typedb — so
        // `CoroutineFunc()` against a plugin's own `class CoroutineFunc` was
        // classified as Core funcdef construction and rejected for taking 0
        // of 1 arguments. A false positive on a line the game accepts. The
        // plugin's own declaration wins, and declining here hands the call
        // to the type-constructor path that should have had it.
        if self.scope.workspace_shadows_funcdef(qualified) {
            return None;
        }
        if self.scope.lookup_funcdef(qualified).is_none() {
            return None;
        }
        let arg_refs: Vec<&Expr> = args.iter().map(|a| &a.value).collect();
        Some(self.check_funcdef_construct(qualified, &arg_refs, span))
    }

    fn funcdef_name_of(&self, ty: &TypeRepr) -> Option<String> {
        match ty {
            TypeRepr::Funcdef(n) => Some(n.clone()),
            TypeRepr::Named(n) if self.scope.lookup_funcdef(n).is_some() => Some(n.clone()),
            TypeRepr::Handle(inner) | TypeRepr::Const(inner) => self.funcdef_name_of(inner),
            _ => None,
        }
    }

    /// `Funcdef(qual)` / `Name(fn)` constructor: one compatible function
    /// argument, result is the funcdef type (handle-compatible with `@`).
    fn check_funcdef_construct(&mut self, qualified: &str, args: &[&Expr], span: Span) -> TypeRepr {
        let Some(fd) = self.scope.lookup_funcdef(qualified) else {
            return TypeRepr::Error(String::new());
        };
        self.record_use(qualified);
        if args.len() != 1 {
            self.diagnostics.push(TypeDiagnostic {
                span,
                kind: TypeDiagnosticKind::ArgCountMismatch {
                    function_name: qualified.to_string(),
                    expected_min: 1,
                    expected_max: 1,
                    got: args.len(),
                },
            });
            for a in args {
                let _ = self.expr_type(a);
            }
            return TypeRepr::Funcdef(qualified.to_string());
        }
        let arg_ty = self.expr_type(args[0]);
        if !fd.return_type.is_empty() || !fd.param_types.is_empty() {
            self.check_funcdef_ctor_arg(&fd, args[0], &arg_ty);
        }
        TypeRepr::Funcdef(qualified.to_string())
    }

    fn check_funcdef_ctor_arg(&mut self, fd: &FuncdefSig, arg: &Expr, arg_ty: &TypeRepr) {
        if self.arg_binds_to_funcdef(fd, arg, arg_ty) {
            return;
        }
        let sigs = self.lookup_arg_function_sigs(arg, arg_ty);
        let clearly_not_fn = self.is_clearly_not_a_function(arg_ty) && sigs.is_empty();
        if clearly_not_fn || !sigs.is_empty() {
            self.diagnostics.push(TypeDiagnostic {
                span: arg.span,
                kind: TypeDiagnosticKind::ArgTypeMismatch {
                    function_name: fd.qualified.clone(),
                    param_index: 0,
                    expected: render_funcdef_sig(fd),
                    got: render_func_or_type(&sigs, arg_ty),
                },
            });
        }
    }

    /// B023: resolve a candidate parameter whose type names a funcdef (slot
    /// text like `UI::InputTextCallback@` / `CoroutineFunc`) to its
    /// signature. `None` for every non-funcdef param shape.
    fn slot_funcdef(&self, param_text: &str) -> Option<FuncdefSig> {
        let name = match Self::peel_const_handle(&TypeRepr::parse_type_string(param_text.trim())) {
            TypeRepr::Named(n) | TypeRepr::Funcdef(n) => n.clone(),
            _ => return None,
        };
        self.scope.lookup_funcdef(&name)
    }

    /// B102: resolve `array<T>`'s TEMPLATE-MEMBER funcdef slot by
    /// substituting the receiver's element type.
    ///
    /// `void Sort(T[]::less&in, uint startAt = 0, uint count = uint(-1))` is
    /// the only slot in the typedb whose expected signature is not fixed: the
    /// comparator's parameters ARE the array's element type, so the signature
    /// has to be computed per call site. Every other funcdef check in this
    /// file compares against one declared signature.
    ///
    /// SYNTHESISED, because there is nothing to look up. `T[]::less` is
    /// typeid 67108888, referenced by `Sort`'s argument list and DECLARED
    /// NOWHERE, and the template parameter `T` (67112305) dangles the same
    /// way -- re-measured against the 1.29.5 and 1.29.14 fixture dumps and
    /// this machine's live 1.29.14 install, all three identical, so it is a
    /// stable dump gap rather than a version artefact. Only
    /// `bool less_nonconst(T&in a, T&in b)` is declared; `SortNonConst` is
    /// given the same synthesised treatment rather than read from that
    /// declaration, so the two slots cannot drift apart.
    ///
    /// WHAT THIS DELIBERATELY DOES NOT DECIDE, AND WHY THAT IS NOW A CHOICE
    /// RATHER THAN AN ABSENCE. Const-ness is neutral by CONSTRUCTION rather
    /// than by a carve-out a later edit could drop: `funcdef_types_compatible`
    /// peels `const` before comparing. What is left is wrong under EVERY
    /// reading of the missing declaration -- arity, return type, element type,
    /// parameter mode. The mode half is not a guess either: B027 measured
    /// `Can't create delegate` on any plain/&in/&out/&inout mismatch, and this
    /// reuses that comparison rather than adding a second one.
    ///
    /// B102 recorded the const axis as UNMEASURED because B097 blocked game
    /// probing repo-wide. B097 is fixed and B144 MEASURED IT (2026-09-01,
    /// OP 1.29.14, `docs/probes/b144-const-handle-funcdef.tsv`). The engine
    /// prints its own slot signature whenever a bind fails, so it answers
    /// directly rather than by inference:
    ///
    /// ```text
    /// Where 'less' is 'bool int[]::less(const int&in, const int&in)'
    /// Where 'less_nonconst' is 'bool int[]::less_nonconst(int&in, int&in)'
    /// Where 'less' is 'bool Item@[]::less(const Item@const&in, const Item@const&in)'
    /// ```
    ///
    /// Two things follow that this synthesis does NOT yet act on.
    ///
    /// * The engine is EXACT on const-ness, both directions: `int &in` at
    ///   `Sort` is rejected, and `const int &in` at `SortNonConst` is
    ///   rejected. The neutrality above is therefore a KNOWN false negative,
    ///   not an open question. Tightening it fires on all four const
    ///   positions at once -- including the LEADING one B110 guards -- with
    ///   the opposite risk polarity to B144, so it wants its own ticket and
    ///   its own fleet sweep.
    /// * For a HANDLE element type the engine's comparator carries BOTH
    ///   consts (`const T@ const &in`), where `elem.display()` below emits a
    ///   bare `T@`. That difference is invisible today because the comparison
    ///   peels const, and it is left alone for the same reason: changing it
    ///   changes no verdict, so it belongs with the tightening rather than
    ///   ahead of it.
    fn synth_template_member_funcdef(
        &self,
        param_text: &str,
        elem: Option<&TypeRepr>,
    ) -> Option<FuncdefSig> {
        let elem = elem?;
        if elem.is_error() {
            return None;
        }
        // Strip the SLOT's own decoration: `&in`/`const` here describe how the
        // comparator HANDLE is passed to `Sort`, and say nothing about the
        // comparator's own parameters.
        let bare = param_text
            .trim()
            .trim_start_matches("const ")
            .split('&')
            .next()?
            .trim();
        // Matched in full rather than by a `::less` tail: a workspace type
        // could end that way, and this must fire only for the engine's two
        // array comparator slots.
        if !matches!(bare, "T[]::less" | "T[]::less_nonconst") {
            return None;
        }
        // B152: the engine's own spelling, measured rather than guessed. A
        // deliberate mismatch makes the compiler print the comparator it
        // wants, and it depends on whether the ELEMENT is a handle:
        //
        //     bool int[]::less(const int&in, const int&in)
        //     bool Item@[]::less(const Item@const&in, const Item@const&in)
        //     bool int[]::less_nonconst(int&in, int&in)
        //
        // Measured for `int`, `uint`, `string`, `vec3`, an enum and a value
        // class on the plain side, and for `C@`, `I@`, `Json::Value@`,
        // `int[]@` and `dictionary@` on the handle side — eleven element
        // types in one run, `docs/probes/b144-const-handle-funcdef.tsv`
        // (2026-09-01, OP 1.29.14).
        //
        // The `const` here is load-bearing only once the comparison is
        // const-EXACT (B152); before that it was invisible, which is exactly
        // why B144 left it alone rather than changing it blind.
        let elem_text = elem.display();
        let param = if bare == "T[]::less_nonconst" {
            elem_text
        } else if matches!(elem, TypeRepr::Handle(_)) {
            format!("const {elem_text} const")
        } else {
            format!("const {elem_text}")
        };
        Some(FuncdefSig {
            qualified: bare.to_string(),
            return_type: "bool".to_string(),
            param_types: vec![param.clone(), param],
            param_modes: vec![ParamMode::In, ParamMode::In],
        })
    }

    /// [`Self::slot_funcdef`], falling back to the template-member
    /// substitution above. `elem` is the receiver's array element type and is
    /// `None` at every call site that is not an array method, so the
    /// substitution can only fire where there is a `T` to substitute.
    fn slot_funcdef_templated(
        &self,
        param_text: &str,
        elem: Option<&TypeRepr>,
    ) -> Option<FuncdefSig> {
        self.slot_funcdef(param_text)
            .or_else(|| self.synth_template_member_funcdef(param_text, elem))
    }

    /// B089: [`Self::slot_funcdef`], resolved in the DECLARING SCOPE of
    /// `owner_qualified`.
    ///
    /// Workspace parameter text is the source spelling, so a funcdef declared
    /// in `namespace NS` and used by a function in the same namespace is
    /// stored as `Maker@`, not `NS::Maker@`. `lookup_funcdef` takes a
    /// qualified name and does no ancestor walk, so that slot resolved to
    /// nothing and the parameter was treated as a non-funcdef — silent in
    /// exactly the way that reads as "checked, and fine". It is the
    /// namespaced half of this ticket's generalisation and it passed the
    /// qualified spelling of the same test.
    ///
    /// External parameter text is already fully qualified, which is why the
    /// typedb path does not need this.
    fn slot_funcdef_in_scope(&self, param_text: &str, owner_qualified: &str) -> Option<FuncdefSig> {
        let name = match Self::peel_const_handle(&TypeRepr::parse_type_string(param_text.trim())) {
            TypeRepr::Named(n) | TypeRepr::Funcdef(n) => n.clone(),
            _ => return None,
        };
        // GH #38 / B073: the plugin's own declaration wins, so a parameter
        // written with a name the workspace declares is NOT a funcdef slot,
        // whatever the typedb has under that name. `lookup_funcdef` walks
        // workspace symbols but only RETURNS on a real funcdef and then falls
        // through to the engine, so without this a plugin class named
        // `CoroutineFunc` turns `void g(CoroutineFunc@ p)` into an engine
        // funcdef slot and rejects a perfectly good argument.
        //
        // `check_funcdef_binding` guards B076's four declaration positions
        // with exactly this test. B073 left `slot_funcdef` itself unguarded
        // on a REACHABILITY argument — no site could then reach it while
        // shadowing — and wrote
        // `a_plugin_type_named_after_an_engine_funcdef_behaves_the_same_at_a_call_slot`
        // to go red when that expired. B089 expired it: running the slot
        // check over workspace calls is exactly the site that argument said
        // did not exist. The test did its job and this is the guard it asked
        // for. It lives here rather than in `slot_funcdef` so the external
        // path's own reachability argument is left as B073 measured it.
        if self
            .scope
            .workspace_shadows_funcdef(&self.resolve_value_type_name(&name))
        {
            return None;
        }
        if let Some(fd) = self.slot_funcdef(param_text) {
            return Some(fd);
        }
        // A written qualifier is the author's own answer about scope; walking
        // past it would resolve a name they did not write.
        if name.contains("::") {
            return None;
        }
        let mut owner = owner_qualified;
        while let Some((prefix, _)) = owner.rsplit_once("::") {
            if let Some(fd) = self.scope.lookup_funcdef(&format!("{prefix}::{name}")) {
                return Some(fd);
            }
            owner = prefix;
        }
        None
    }

    /// B076: the funcdef bind check, reached from a DECLARATION or an
    /// ASSIGNMENT rather than only from a constructor argument.
    ///
    /// Nothing here re-implements the comparison, because nothing was wrong
    /// with it. [`Self::func_matches_funcdef`] already compares the return
    /// type FIRST and then every parameter, and
    /// [`Self::arg_fails_funcdef_slot`] already carries B023's
    /// false-positive guards — a workspace type the typedb cannot see stays
    /// neutral, and `null` binds any funcdef handle. Both were only ever
    /// reached from an argument position.
    ///
    /// That is the whole defect: `Maker@ b = fn` accepted a mismatched
    /// return type AND a mismatched parameter list, while `Maker(fn)`
    /// rejected both, because only the second reached this predicate. The
    /// ticket described it as a missing return-type comparison; measured
    /// per position, the comparison exists and the call did not.
    fn check_funcdef_binding(&mut self, declared: &TypeRepr, init: &Expr, init_ty: &TypeRepr) {
        // GH #38 shadowing, and it has to be checked HERE rather than left to
        // `arg_value_funcdef`. That helper guards only its `Named` arm; a
        // declared type that resolves to `Funcdef(_)` goes straight to
        // `lookup_funcdef` and picks up the ENGINE funcdef. A declared type is
        // written in the plugin's own source, so the plugin's declaration wins
        // whichever way the name resolved.
        //
        // Measured, not anticipated: without this,
        // `typedef uint CoroutineFuncUserdataInt64;
        //  CoroutineFuncUserdataInt64 shadowAlias = 1;`
        // is reported as a bad delegate bind — a false positive that turned the
        // B060 fixture red. Pinned by the two shadowing tests in
        // `tests/b076_funcdef_return_type.rs`, which must run WITH a typedb or
        // they pass without reaching this line.
        if let TypeRepr::Named(n) | TypeRepr::Funcdef(n) = Self::peel_const_handle(declared) {
            if self
                .scope
                .workspace_shadows_funcdef(&self.resolve_value_type_name(n))
            {
                return;
            }
        }
        let Some(fd) = self.arg_value_funcdef(declared) else {
            return;
        };
        if self.arg_fails_funcdef_slot(&fd, init, init_ty) {
            let from = self.funcdef_binding_source_display(init, init_ty);
            self.diagnostics.push(TypeDiagnostic {
                span: init.span,
                kind: TypeDiagnosticKind::NoImplicitConversion {
                    from,
                    to: declared.display(),
                },
            });
        }
    }

    /// Render the bound thing as a SIGNATURE when it resolves to exactly one
    /// function.
    ///
    /// Without this the message reads `Can't implicitly convert from 'G' to
    /// 'F@&'.`, naming an identifier where a type belongs and hiding the very
    /// thing that differs. `from 'int()' to 'F@&'.` shows the mismatch and
    /// matches how the engine words it. Falls back to the type display when
    /// the reference is overloaded or does not resolve, since then there is
    /// no single signature to name.
    fn funcdef_binding_source_display(&self, init: &Expr, init_ty: &TypeRepr) -> String {
        let sigs = self.lookup_arg_function_sigs(init, init_ty);
        match sigs.as_slice() {
            [sig] => format!("{}({})", sig.return_type, sig.param_types.join(", ")),
            _ => init_ty.display(),
        }
    }

    /// B023: concrete evidence that `arg` cannot bind the funcdef-typed
    /// slot `fd` — the same bind test as the ctor path. A resolvable
    /// function matching no signature, a funcdef-typed value of an
    /// incompatible type, or a provably non-function value all fail.
    /// Named/unknown-shape args (workspace classes the typedb can't see)
    /// stay neutral — the GH #47 FP guard — and `null` binds any funcdef
    /// handle.
    fn arg_fails_funcdef_slot(&self, fd: &FuncdefSig, arg: &Expr, arg_ty: &TypeRepr) -> bool {
        // B091: a class method does not implicitly decay to a funcdef handle,
        // so a bare method reference fails the slot NO MATTER WHAT its
        // signature is — `startnew(this.Tick)` needs
        // `startnew(CoroutineFunc(this.Tick))` even though `void()` is
        // exactly what the slot wants. Ahead of the signature comparison
        // below precisely because that comparison would say yes.
        if self.arg_is_unwrapped_method_reference(arg) {
            return true;
        }
        let sigs = self.lookup_arg_function_sigs(arg, arg_ty);
        if !sigs.is_empty() {
            return !sigs.iter().any(|sig| self.func_matches_funcdef(sig, fd));
        }
        // A funcdef-typed value (variable, property, ctor result) with no
        // resolvable overloads compares by its own funcdef signature.
        if let Some(arg_fd) = self.arg_value_funcdef(arg_ty) {
            return !self.func_matches_funcdef(&Self::funcdef_overload_sig(&arg_fd), fd);
        }
        // A handle to a class or interface THIS WORKSPACE declares is
        // provably not a callable, so it cannot bind a funcdef slot.
        // `is_clearly_not_a_function` leaves every `Named` neutral to
        // protect GH #47's guard for shapes the checker cannot see — but a
        // type declared right here is not one of those. Without this,
        // `startnew(SomeClassProperty)` goes silent: master caught it only
        // through the `get_` widening B060 removes, and never caught the
        // plain-global spelling at all.
        if self.arg_is_known_non_callable(arg_ty) {
            return true;
        }
        !matches!(Self::peel_const_handle(arg_ty), TypeRepr::Null)
            && self.is_clearly_not_a_function(arg_ty)
    }

    /// True when `arg_ty` names a type the checker KNOWS and that type is
    /// not a funcdef — a class, interface or enum, from the workspace or the
    /// typedb. Such a value can never be a function reference.
    ///
    /// Knowing the type is the whole point: `is_clearly_not_a_function`
    /// leaves every `Named` neutral to protect GH #47's guard for shapes the
    /// checker cannot see, and a name it can resolve is not one of those.
    /// Restricting this to WORKSPACE classes (round 5) left a Core enum
    /// property — `Plot::Marker` inside `namespace UI` — unresolved and
    /// therefore neutral, losing a diagnostic master reports.
    fn arg_is_known_non_callable(&self, arg_ty: &TypeRepr) -> bool {
        let TypeRepr::Named(n) = Self::peel_const_handle(arg_ty) else {
            return false;
        };
        let candidate = self.resolve_value_type_name(n);
        self.scope.names_a_known_non_callable_type(&candidate)
    }

    /// The funcdef a VALUE-typed arg denotes, if any.
    ///
    /// `Cb@` reaches the checker as `Funcdef(Cb)` down the member path but
    /// as `Named(Cb)` down the global/property path, and the two mean the
    /// same thing. B060 made this matter: once the `X` -> `get_X` widening
    /// stopped standing in for a property read, `startnew(BadHandler)`
    /// (`BadCb@ get_BadHandler()`) had nothing left to compare against and
    /// went silent on the `Named` side while still erroring on the member
    /// side. Recognising both keeps the two paths on one answer.
    fn arg_value_funcdef(&self, arg_ty: &TypeRepr) -> Option<FuncdefSig> {
        match Self::peel_const_handle(arg_ty) {
            TypeRepr::Funcdef(n) => self.scope.lookup_funcdef(n),
            // GH #38 shadowing: `lookup_funcdef` falls through to the typedb
            // when the workspace symbol of that name is not a funcdef, so a
            // plugin class / interface / enum / `typedef` named after a Core
            // funcdef would resolve to the ENGINE funcdef and its values
            // would be rejected as function-shaped at an ordinary value
            // slot. The plugin's own declaration wins.
            //
            // The ladder matters as much as the predicate: a value's type
            // text is stored as WRITTEN, so a class declared
            // `Nz::CoroutineFunc` reaches here as a bare `CoroutineFunc`
            // from inside `namespace Nz` and an exact-name guard misses it.
            TypeRepr::Named(n) => {
                let candidate = self.resolve_value_type_name(n);
                if self.scope.workspace_shadows_funcdef(&candidate) {
                    return None;
                }
                self.scope.lookup_funcdef(&candidate)
            }
            _ => None,
        }
    }

    /// Resolve a value's written type name in lexical order — innermost
    /// enclosing namespace outwards, then the name as written — and return
    /// the ONE candidate the rest of the decision uses.
    ///
    /// Resolving once matters: testing `Ns::X` for shadowing and then
    /// looking `X` up as a funcdef answers two questions about two different
    /// declarations, which is how a global `class CoroutineFunc` could mask
    /// a legitimate `Ns::CoroutineFunc` funcdef (review, round 3).
    fn resolve_value_type_name(&self, name: &str) -> String {
        // A QUALIFIED spelling resolves as written before any namespace
        // fallback — `B::X` inside `namespace A` means `B::X`, and only then
        // the relative `A::B::X`. This mirrors `resolver.rs`, and getting it
        // backwards cuts both ways: omitting it let a same-tail `A::B::X`
        // funcdef stand in for the global `B::X` class and swallow a real
        // diagnostic (round 5), while testing only WORKSPACE declarations
        // let a nested `A::UI::InputTextCallback` stand in for the CORE
        // `UI::InputTextCallback` and invent one (round 6). The written name
        // wins if it denotes a type the checker knows AT ALL — typedb
        // included. A BARE name still takes the ladder first, because there
        // the inner scope legitimately shadows the outer one.
        if name.contains("::") && self.scope.names_a_known_type(name) {
            return name.to_string();
        }
        for depth in (1..=self.namespace_stack.len()).rev() {
            let candidate = format!("{}::{}", self.namespace_stack[..depth].join("::"), name);
            if self.scope.names_a_known_type(&candidate) {
                return candidate;
            }
        }
        name.to_string()
    }

    /// A funcdef signature as an `OverloadSig` for the shared bind test.
    fn funcdef_overload_sig(fd: &FuncdefSig) -> OverloadSig {
        OverloadSig {
            param_names: Vec::new(),
            param_types: fd.param_types.clone(),
            param_out: fd
                .param_modes
                .iter()
                .map(|m| *m == ParamMode::Out)
                .collect(),
            param_modes: fd.param_modes.clone(),
            min_args: fd.param_types.len(),
            return_type: fd.return_type.clone(),
        }
    }

    /// B023: the arg is a resolvable function (free fn, method reference,
    /// namespace fn) or a funcdef-typed value — a shape that can only ever
    /// bind a funcdef parameter.
    fn arg_is_function_shaped(&self, arg: &Expr, arg_ty: &TypeRepr) -> bool {
        if !self.lookup_arg_function_sigs(arg, arg_ty).is_empty() {
            return true;
        }
        self.arg_value_funcdef(arg_ty).is_some()
    }

    fn arg_binds_to_funcdef(&self, fd: &FuncdefSig, arg: &Expr, arg_ty: &TypeRepr) -> bool {
        let peeled = Self::peel_const_handle(arg_ty);
        match peeled {
            TypeRepr::Funcdef(n) | TypeRepr::Named(n) => {
                if self.named_types_equivalent(n, &fd.qualified)
                    || self.scope.lookup_funcdef(n).is_some_and(|other| {
                        other.qualified == fd.qualified
                            || self.named_types_equivalent(&other.qualified, &fd.qualified)
                    })
                {
                    return true;
                }
            }
            _ => {}
        }
        self.lookup_arg_function_sigs(arg, arg_ty)
            .iter()
            .any(|sig| self.func_matches_funcdef(sig, fd))
    }

    /// Pure receiver typing for a B025 method reference (`recv.Method` at a
    /// funcdef ctor arg): resolves the receiver's type WITHOUT recording uses
    /// or emitting diagnostics — the caller already walked the full arg once.
    /// `None` when the receiver's class can't be determined statically.
    fn method_ref_receiver_type(&self, expr: &Expr) -> Option<TypeRepr> {
        match &expr.kind {
            ExprKind::This => self
                .current_class()
                .map(|cls| TypeRepr::Named(cls.name.clone())),
            ExprKind::Ident(name) => {
                let n = name.text(self.source);
                // Mirror `expr_type`'s Ident precedence (B025 review):
                // locals, then implicit-`this` class members (members shadow
                // globals), then namespace-prefixed globals, then bare
                // globals — the same expression must not be typed two ways
                // within one diagnostic decision.
                if let Some(local) = self.lookup_local(n) {
                    return Some(local.ty.clone());
                }
                if let Some(ty) = self.lookup_class_member(n) {
                    return Some(ty);
                }
                if let Some(cls) = self.current_class() {
                    if let Some(ty) = self.scope.workspace_class_member(&cls.name, n) {
                        return Some(ty);
                    }
                }
                for depth in (1..=self.namespace_stack.len()).rev() {
                    let ns = self.namespace_stack[..depth].join("::");
                    let qualified = format!("{}::{}", ns, n);
                    if let Some(ty) = self.scope.lookup_global_value_type(&qualified) {
                        return Some(ty);
                    }
                }
                self.scope.lookup_global_value_type(n)
            }
            // `box.inner.M` — the receiver of the method reference is itself
            // a member access; its type is that member's type.
            ExprKind::Member {
                object,
                member: name,
            } => {
                let base = Self::base_type_name(&self.method_ref_receiver_type(object)?)?;
                self.scope
                    .workspace_class_member(&base, name.text(self.source))
            }
            // `GetN().Disconnect` / `this.Make().Go` / `Make().Go` in its
            // namespace — a factory call's receiver type is the callee's
            // return type (workspace or typedb).
            ExprKind::Call { callee, .. } => match &callee.kind {
                // The callee is itself a member access: the Member rule
                // below resolves the method's return type.
                ExprKind::Member { .. } => return self.method_ref_receiver_type(callee),
                ExprKind::NamespaceAccess { path } => {
                    return self
                        .scope
                        .lookup_function_return(&path.to_string(self.source));
                }
                ExprKind::Ident(ident) => {
                    let name = ident.text(self.source);
                    for depth in (1..=self.namespace_stack.len()).rev() {
                        let ns = self.namespace_stack[..depth].join("::");
                        if let Some(t) = self
                            .scope
                            .lookup_function_return(&format!("{}::{}", ns, name))
                        {
                            return Some(t);
                        }
                    }
                    return self.scope.lookup_function_return(name);
                }
                _ => return None,
            },
            // `cast<N>(h).Disconnect` — the cast's target type.
            ExprKind::Cast { target_type, .. } => {
                let mut te = target_type;
                while let TypeExprKind::Handle(inner) = &te.kind {
                    te = inner;
                }
                match &te.kind {
                    TypeExprKind::Named(qn) => Some(TypeRepr::Named(qn.to_string(self.source))),
                    _ => None,
                }
            }
            // `edges[i].Disconnect` / `box.ns[0].Go` — the element type of
            // the indexed receiver.
            ExprKind::Index { object, .. } => self
                .method_ref_receiver_type(object)?
                .array_element_type()
                .cloned(),
            _ => None,
        }
    }

    /// The function signatures `arg` can denote when read as a FUNCTION
    /// REFERENCE.
    ///
    /// B060: every lookup here is EXACT — the `X` -> `get_X` / `set_X`
    /// virtual-property widening must not apply. A property read (`Off`,
    /// `c.Amt`, `Ns::Slot`) is a value of the accessor's RETURN type, not a
    /// reference to the accessor; widening made it function-shaped, so it
    /// could not bind a `uint`/`float` parameter and killed the whole
    /// overload set (`Text::Format("0x%04x", Off)`). Writing `get_Off`
    /// still resolves, because that IS the function's name.
    fn lookup_arg_function_sigs(&self, arg: &Expr, arg_ty: &TypeRepr) -> Vec<OverloadSig> {
        let mut out = Vec::new();
        match &arg.kind {
            ExprKind::Ident(ident) => {
                let name = ident.text(self.source);
                // Locals shadow methods and global functions (expr_type's
                // Ident precedence): a shadowing local must never resolve
                // to the same-named function's signatures (B023 review —
                // a local funcdef handle named like an incompatible
                // global fn was mis-typed as the fn and falsely
                // rejected). A local funcdef handle contributes its own
                // signature; any other local contributes none.
                if let Some(local) = self.lookup_local(name) {
                    if let TypeRepr::Funcdef(n) = Self::peel_const_handle(&local.ty) {
                        if let Some(fd) = self.scope.lookup_funcdef(n) {
                            out.push(Self::funcdef_overload_sig(&fd));
                        }
                    }
                    return out;
                }
                if let Some(cls) = self.current_class() {
                    out.extend(
                        self.scope
                            .lookup_method_overloads_exact_with_inheritance(&cls.name, name),
                    );
                }
                out.extend(self.scope.lookup_function_overloads_exact(name));
                for depth in (1..=self.namespace_stack.len()).rev() {
                    let ns = self.namespace_stack[..depth].join("::");
                    out.extend(
                        self.scope
                            .lookup_function_overloads_exact(&format!("{ns}::{name}")),
                    );
                }
            }
            ExprKind::NamespaceAccess { path } => {
                let qual = path.to_string(self.source);
                out.extend(self.scope.lookup_function_overloads_exact(&qual));
            }
            ExprKind::Member { object, member } => {
                out.extend(self.method_ref_sigs(object, member));
            }
            _ => {}
        }
        if let TypeRepr::Funcdef(n) = Self::peel_const_handle(arg_ty) {
            if out.is_empty() {
                out.extend(self.scope.lookup_function_overloads_exact(n));
            }
        }
        out
    }

    /// B108: each argument resolved for the `No matching signatures`
    /// message, so a callable names its SIGNATURE rather than its return
    /// type.
    ///
    /// Reuses [`Self::funcdef_binding_source_display`], which is what already
    /// produces `Can't implicitly convert from 'void()' to 'CoroutineFunc@&'.`
    /// at the DECLARATION position — the same argument shape, one message
    /// away. Deliberately not `render_func_or_type`, which B108 names:
    /// that one emits `"{} ({})"` with a space and takes `sigs.first()`,
    /// silently choosing among overloads. Every assertion-level pin in the
    /// tree spells this `void()`, so reusing the other renderer would have
    /// introduced a second spelling of one idea.
    ///
    /// Called only on the ERROR path. Resolving every argument of every call
    /// eagerly would put a scope lookup per argument on the hot path to buy
    /// nothing — the text is needed only when a diagnostic is actually
    /// emitted.
    fn rendered_args(&self, args: &[CallArg], arg_tys: &[TypeRepr]) -> Vec<RenderedArg> {
        args.iter()
            .zip(arg_tys)
            .map(|(arg, ty)| RenderedArg {
                text: if Self::type_already_names_the_callable(ty) {
                    ty.display()
                } else {
                    self.funcdef_binding_source_display(&arg.value, ty)
                },
                is_literal: arg_is_const_literal(&arg.value),
            })
            .collect()
    }

    /// True when the argument's TYPE already denotes the callable, so
    /// rendering its signature instead would DISCARD information.
    ///
    /// The B108 defect is narrower than "a callable prints badly": it is a
    /// callable flattened to its RETURN type. That happens to a method
    /// reference or a bare function name, whose `expr_type` is the return
    /// type — `this.Tick` types as `void`. It does NOT happen to a
    /// funcdef-typed VALUE: `StringLess@ badHandle` types as `StringLess@`,
    /// which is a real name the author wrote and can look up, and is the
    /// more useful half of the message. Substituting
    /// `bool(const string &in, const string &in)` there would trade a name
    /// for a shape the reader can already see in the declaration.
    ///
    /// Measured, not reasoned: widening to every argument turned B102's
    /// `array::Sort(StringLess@)` into the signature form and its test
    /// caught it. That pin is load-bearing — B102 records that it is what
    /// makes the sibling fixture's correct-handle row falsifiable — so this
    /// is the suite refusing an over-reach, not a fixture needing an update.
    fn type_already_names_the_callable(ty: &TypeRepr) -> bool {
        matches!(Self::peel_const_handle(ty), TypeRepr::Funcdef(_))
    }

    /// B025: the overload signatures a MEMBER ACCESS denotes when read as a
    /// method reference (`this.M`, `c.M`, `g.M`, `arr[i].M`, `box.inner.M`).
    ///
    /// Empty when the member is not a method of the receiver's class — a
    /// field read, a virtual property, an unresolvable receiver — which is
    /// what makes it usable as the "is this a method reference?" predicate
    /// [`Self::arg_is_unwrapped_method_reference`] needs. B091 needs the
    /// question and B025 needs the answer, and they must not be able to
    /// disagree, so there is one resolution and both read it.
    fn method_ref_sigs(&self, object: &Expr, member: &Ident) -> Vec<OverloadSig> {
        // Resolve the receiver's class and collect the method's overload
        // signatures instead of trusting the member access's own type (that
        // is the method's RETURN type).
        let Some(cls_name) = self
            .method_ref_receiver_type(object)
            .as_ref()
            .and_then(Self::base_type_name)
        else {
            return Vec::new();
        };
        let name = member.text(self.source);

        // B150: a BARE `obj.Name` is a PROPERTY READ, not a method
        // reference, whenever the class also exposes `get_Name`. AngelScript
        // resolves the member through the virtual property there, which is
        // exactly B060's rule ("a property read is a value of the getter's
        // return type") applied at the member-REFERENCE seam rather than
        // only at the type seam.
        //
        // The shipped typedb makes this concrete and asymmetric. The eight
        // `Mw*` engine containers register BOTH `uint Length() const` and
        // `uint get_Length() const`; script `array` / `string` / `wstring`
        // register only the accessor; `vec2`/`vec3`/`vec4`/`quat` register
        // only the method. Because a real `Length` METHOD exists on the
        // containers, this function used to hand B023 a callable, so
        // `Math::Min(uint(20), buf.Length)` — which the game compiles — was
        // rejected outright as "a function can never bind a non-funcdef
        // parameter", and B108 then spelled the argument `uint()`. One cause,
        // two symptoms: the false rejection and the misleading rendering.
        //
        // Keyed on the ACCESSOR existing, not on the member's name, so it
        // covers workspace classes and any future engine type on the same
        // footing. `vec3::Length` has no `get_Length`, so it stays a method
        // reference and keeps B108's `float()` spelling. An explicit
        // `obj.get_Length` cannot enter this arm either — there is no
        // `get_get_Length` — so the accessor spelling is untouched.
        let accessor = format!("get_{name}");
        if !self
            .scope
            .lookup_method_overloads_exact_with_inheritance(&cls_name, &accessor)
            .is_empty()
            || !self.scope.callables_method(&cls_name, &accessor).is_empty()
        {
            return Vec::new();
        }

        let sigs = self
            .scope
            .lookup_method_overloads_exact_with_inheritance(&cls_name, name);
        if !sigs.is_empty() {
            return sigs;
        }
        // Engine-typed receivers (`r.Cancel` on `Net::HttpRequest@`): bind
        // the typedb's method overloads the same way the member-access path
        // does.
        self.scope.callables_method(&cls_name, name)
    }

    /// B091: `arg` is a BARE class-method reference at a funcdef slot —
    /// `startnew(this.Tick)` where the engine requires
    /// `startnew(CoroutineFunc(this.Tick))`.
    ///
    /// A class method does not implicitly decay to a funcdef handle the way
    /// a free function does; the funcdef CONSTRUCTOR is the required
    /// method-to-funcdef conversion. That is not a new claim: it is the
    /// premise B025 already stated in prose and relied on, measured by the
    /// same RemoteBuild probe (2026-08-29, OP 1.29.5) that produced the
    /// b025 fixture's legal/illegal halves — the ctor spellings there
    /// compile and the engine's word for a bad one is `Can't create
    /// delegate`. Every consumer of that premise so far read it in the
    /// direction "a method reference IS callable, so do not reject the
    /// ctor"; nothing read it in the direction "so a BARE one cannot bind".
    /// The signature happened to match, so the slot check said yes.
    ///
    /// Deliberately keyed on "the member resolves to a METHOD", not on "the
    /// argument is a member access": a funcdef-typed FIELD read (`h.cb`) is
    /// a value and binds the slot exactly like a local would.
    /// [`Self::method_ref_sigs`] draws that line, and it is the same line
    /// B025 draws when it decides the expression is a callable at all.
    ///
    /// A BARE IDENTIFIER naming a method of the enclosing class
    /// (`startnew(Tick)` inside the class) is left NEUTRAL. It is the same
    /// language rule, but the identifier arm of
    /// [`Self::lookup_arg_function_sigs`] merges class methods with
    /// same-named free functions and namespace functions into one candidate
    /// list, so rejecting on "a method is among the candidates" would reject
    /// a legal free-function decay whenever a method shares its name. B097
    /// blocks the probe that would settle whether the engine even accepts
    /// that spelling, so the neutral behaviour is chosen by construction
    /// rather than guessed at — the same call B102 made on const-ness.
    fn arg_is_unwrapped_method_reference(&self, arg: &Expr) -> bool {
        match &arg.kind {
            ExprKind::Member { object, member } => !self.method_ref_sigs(object, member).is_empty(),
            _ => false,
        }
    }

    fn func_matches_funcdef(&self, sig: &OverloadSig, fd: &FuncdefSig) -> bool {
        if sig.param_types.len() != fd.param_types.len() {
            return false;
        }
        if !self.funcdef_types_compatible(
            &TypeRepr::parse_type_string(&sig.return_type),
            &TypeRepr::parse_type_string(&fd.return_type),
        ) {
            return false;
        }
        sig.param_types
            .iter()
            .zip(&fd.param_types)
            .enumerate()
            .all(|(i, (got, expected))| {
                if !self.funcdef_types_compatible(
                    &TypeRepr::parse_type_string(got),
                    &TypeRepr::parse_type_string(expected),
                ) {
                    return false;
                }
                // Delegate construction requires EXACT parameter passing
                // modes (game probe 2026-08-29, OP 1.29.5: `Can't create
                // delegate` on any plain/&in/&out/&inout mismatch). The
                // signature's `param_modes` is authoritative; fall back to
                // the param text (then the `&out` flag) for safety.
                let got_mode = sig.param_modes.get(i).copied().unwrap_or_else(|| {
                    match param_mode_from_text(got) {
                        ParamMode::Value if sig.param_out.get(i).copied().unwrap_or(false) => {
                            ParamMode::Out
                        }
                        mode => mode,
                    }
                });
                let expected_mode = fd
                    .param_modes
                    .get(i)
                    .copied()
                    .unwrap_or_else(|| param_mode_from_text(expected));
                got_mode == expected_mode
            })
    }

    /// B152: the CONST SHAPE of a type — the thing a funcdef signature match
    /// compares exactly, and the only axis this ticket tightens.
    ///
    /// AngelScript spells two independent qualifiers with one keyword. A
    /// LEADING `const` makes the pointed-to OBJECT read-only; a TRAILING one
    /// makes the HANDLE read-only; a value type has only the one position.
    /// So a handle parameter has FOUR spellings, and the engine tells them
    /// apart at a funcdef slot in both directions — measured over every
    /// position, mode and the return type
    /// (`docs/probes/b144-const-handle-funcdef.tsv`, OP 1.29.14).
    ///
    /// `is_handle` is carried but is NOT itself matched — see
    /// [`Self::const_shapes_compatible`].
    fn const_shape(ty: &TypeRepr) -> (bool, bool, bool) {
        let mut outer_const = false;
        let mut cur = ty;
        while let TypeRepr::Const(inner) = cur {
            outer_const = true;
            cur = inner;
        }
        match cur {
            TypeRepr::Handle(pointee) => {
                let mut p = pointee.as_ref();
                let mut pointee_const = false;
                while let TypeRepr::Const(inner) = p {
                    pointee_const = true;
                    p = inner;
                }
                (true, pointee_const, outer_const)
            }
            _ => (false, false, outer_const),
        }
    }

    /// B152: do two types agree on `const`?
    ///
    /// EXACT when both sides are handles, because that is the only case where
    /// the two const positions are distinguishable and it is the case the
    /// engine was measured on.
    ///
    /// DELIBERATELY WEAKER when either side is not a handle, and this is the
    /// most important line in the change. The `@` is not reliable across the
    /// seam: the Core database declares `CoroutineFuncUserdata`'s parameter
    /// as `ref`, while every plugin that uses it writes `ref@` and the game
    /// compiles them. An instrumented build — the neutral answer kept, a
    /// marker printed wherever an exact rule would disagree — found **170
    /// such sites across 30 plugins** in the corpus. Requiring the handle
    /// wrapper to match would have turned all 170 into errors on shipped
    /// code, and none of them is about `const` at all.
    ///
    /// So the handle axis stays exactly as loose as it was before this
    /// ticket (`funcdef_types_compatible` peels it), and only the const axis
    /// tightens. One axis, measured, at one site.
    fn const_shapes_compatible(got: &TypeRepr, expected: &TypeRepr) -> bool {
        let (g_handle, g_pointee, g_outer) = Self::const_shape(got);
        let (e_handle, e_pointee, e_outer) = Self::const_shape(expected);
        if g_handle && e_handle {
            g_pointee == e_pointee && g_outer == e_outer
        } else {
            (g_pointee || g_outer) == (e_pointee || e_outer)
        }
    }

    fn funcdef_types_compatible(&self, got: &TypeRepr, expected: &TypeRepr) -> bool {
        // B152: `const` is part of the signature, so it is compared BEFORE
        // the peel below throws it away. The peel stays for everything else
        // — the handle axis is deliberately untouched, see
        // `const_shapes_compatible`.
        if !Self::const_shapes_compatible(got, expected) {
            return false;
        }
        let got = Self::peel_const_handle(got);
        let expected = Self::peel_const_handle(expected);
        match (got, expected) {
            (TypeRepr::Void, TypeRepr::Void) => true,
            // B110: EXACT, because a funcdef signature match is not a
            // conversion site. `is_convertible` used to stand here beside
            // `a == b`, which treated a function pointer's parameter and
            // return types as if they were VALUES being converted. They are
            // not: a delegate is called with the caller's types directly, so
            // there is no seam for the engine to insert a numeric conversion
            // into and a `double()` cannot stand in for an `int()`.
            //
            // MEASURED by B165, OP 1.29.14, 2026-09-01. The live engine
            // rejects the real `void H(int)` / `startnew(H, 1)` seam, plus
            // int/int64 and uint/uint64 in both directions and signed/unsigned
            // crossings at 32 and 64 bits. Both oracle controls behaved.
            // See `docs/probes/b165-primitive-funcdef.tsv`.
            //
            // The risk audit was repeated after measuring: a temporary
            // old-vs-exact marker reached 408 comparisons over 151 shipped
            // plugin dirs plus five in-repo fixtures. Every hit was one of
            // the four known overload-sibling pairs (int64 <-> uint64 and
            // either -> double); no int <-> int64 hit or reference-data
            // defect appeared. An exactly matching sibling still accepts at
            // those calls, pinned by the typedb-backed startnew test below.
            //
            // Value conversions elsewhere are untouched and correct:
            // `int x = someDouble;` really is a conversion, and
            // `is_convertible` is still what answers that.
            //
            // Spelling-safe rather than spelling-sensitive: `int`/`int32` and
            // `uint`/`uint32` fold onto one `PrimitiveType` in `from_name`,
            // so comparing the parsed primitives does not false-positive on
            // the typedb's `int32` against a source `int`. Pinned by
            // `the_int32_spelling_is_the_same_type_as_int`.
            (TypeRepr::Primitive(a), TypeRepr::Primitive(b)) => a == b,
            (
                TypeRepr::Named(a) | TypeRepr::Funcdef(a),
                TypeRepr::Named(b) | TypeRepr::Funcdef(b),
            ) => {
                // B081: the descendant is a name from SOURCE here, not an
                // engine identity, so it is resolved shadow-aware. The
                // ancestor stays as written — see `ExternalTypeName`.
                self.named_types_equivalent(a, b)
                    || self
                        .scope
                        .external_type_name(a)
                        .is_some_and(|d| self.scope.external_type_derives_from(d, b))
            }
            _ => got == expected,
        }
    }

    fn is_clearly_not_a_function(&self, ty: &TypeRepr) -> bool {
        matches!(
            Self::peel_const_handle(ty),
            TypeRepr::Primitive(_)
                | TypeRepr::Void
                | TypeRepr::Null
                | TypeRepr::Array(_)
                | TypeRepr::Generic { .. }
        )
    }

    /// `TypeRepr::Funcdef(q)` is handle-compatible with `Named(q)` /
    /// `Funcdef(q)` (the `@` form of the same funcdef).
    fn funcdef_handle_compatible(&self, arg: &TypeRepr, param: &TypeRepr) -> bool {
        fn type_name(t: &TypeRepr) -> Option<&str> {
            match t {
                TypeRepr::Funcdef(n) | TypeRepr::Named(n) => Some(n.as_str()),
                _ => None,
            }
        }
        match (type_name(arg), type_name(param)) {
            (Some(a), Some(p)) => {
                self.named_types_equivalent(a, p)
                    && (self.scope.lookup_funcdef(a).is_some()
                        || self.scope.lookup_funcdef(p).is_some())
            }
            _ => false,
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
        qualified_name: &str,
        args: &[CallArg],
        overload: &OverloadSig,
        elem: Option<&TypeRepr>,
    ) {
        // Compute each argument's type once (this also walks the args for
        // nested diagnostics — callers must not pre-walk).
        let mut arg_tys: Vec<TypeRepr> = Vec::with_capacity(args.len());
        let mut arg_literals = Vec::with_capacity(args.len());
        let mut arg_bool_literals = Vec::with_capacity(args.len());
        for a in args {
            arg_tys.push(self.expr_type(&a.value));
            arg_literals.push(arg_is_const_literal(&a.value));
            arg_bool_literals.push(matches!(a.value.kind, ExprKind::BoolLit(_)));
        }
        let mut next_positional = 0usize;
        for (i, arg) in args.iter().enumerate() {
            let named = arg.name.as_ref().map(|n| n.text(self.source));
            match call_site::bind_arg(named, &overload.param_names, &mut next_positional) {
                ArgBind::UnknownName => {
                    // Unknown names stay conservative; nested diagnostics
                    // were already retained by the walk above.
                    continue;
                }
                ArgBind::Index(param_index) => {
                    let arg_ty = &arg_tys[i];
                    let Some(param_text) = overload.param_types.get(param_index) else {
                        continue;
                    };
                    // GH #47 (unique-overload half): a BOOL literal cannot
                    // bind an `&out` slot — game probe (outmap, 2026-08-18):
                    // `UI::Begin("w2", true)` → `No matching signatures to
                    // 'UI::Begin(const string, const bool)'`, while numeric
                    // literals DO bind `&out` via temporaries
                    // (`Text::TryParseFloat("1.5", 0.0)` is accepted).
                    if overload
                        .param_out
                        .get(param_index)
                        .copied()
                        .unwrap_or(false)
                        && arg_bool_literals[i]
                    {
                        let text = render_no_matching_sig(
                            qualified_name,
                            &self.rendered_args(args, &arg_tys),
                        );
                        self.diagnostics.push(TypeDiagnostic {
                            span: arg.value.span,
                            kind: TypeDiagnosticKind::NoMatchingSignatures { text },
                        });
                        continue;
                    }
                    let param_ty = TypeRepr::parse_type_string(param_text.trim());
                    if matches!(param_ty, TypeRepr::Error(_)) {
                        continue;
                    }
                    if matches!(arg_ty, TypeRepr::Error(_)) {
                        continue;
                    }

                    let arg_core = Self::peel_const_handle(arg_ty);
                    let param_core = Self::peel_const_handle(&param_ty);

                    // B023: function-shaped args must actually bind a
                    // funcdef-typed param (ctor-path bind test) — a bare
                    // wrong-sig function would otherwise be whitelisted by
                    // the CoroutineFunc decay below. A provably
                    // non-function arg is `No matching signatures`
                    // (GH #47 style).
                    if let Some(fd) = self.slot_funcdef_templated(param_text, elem) {
                        if self.arg_fails_funcdef_slot(&fd, &arg.value, arg_ty) {
                            let text = render_no_matching_sig(
                                qualified_name,
                                &self.rendered_args(args, &arg_tys),
                            );
                            self.diagnostics.push(TypeDiagnostic {
                                span: arg.value.span,
                                kind: TypeDiagnosticKind::NoMatchingSignatures { text },
                            });
                            continue;
                        }
                    }
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
                    // GH #63: `UI::InputTextCallback(fn)` yields
                    // `Funcdef(qual)`, which is handle-compatible with the
                    // `@` form stored as `Named`/`Funcdef` in typedb params.
                    if self.funcdef_handle_compatible(arg_core, param_core) {
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
                            // GH #54: a single-argument constructor on the
                            // PARAMETER's value type is an implicit
                            // conversion in AngelScript — `mat4(const
                            // mat3&in)` is what makes `mat4::Inverse(mat3)`
                            // compile. Only value positions qualify: a
                            // handle on either side can't be constructed
                            // into or out of.
                            let ctor_converts =
                                !matches!(arg_ty.unwrap_const(), TypeRepr::Handle(_))
                                    && !matches!(param_ty.unwrap_const(), TypeRepr::Handle(_))
                                    && self.scope.external_ctor_converts(arg_n, param_n);
                            if !self.named_types_equivalent(arg_n, param_n)
                                // B081: shadow-aware on the descendant, as
                                // above. Note `is_external_type(arg_n)` is
                                // already consulted sixteen lines up for
                                // `is_unknown_workspace_arg` — the correct
                                // predicate was in view and the shadow-blind
                                // one was used here anyway, which is the
                                // selection failure this ticket is about.
                                && !self
                                    .scope
                                    .external_type_name(arg_n)
                                    .is_some_and(|d| {
                                        self.scope.external_type_derives_from(d, param_n)
                                    })
                                && !ctor_converts
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
        qualified_name: &str,
        args: &[CallArg],
        overloads: &[OverloadSig],
        elem: Option<&TypeRepr>,
    ) {
        if let Some(sig) = call_site::unique_overload_for_argc(overloads, args.len()) {
            self.walk_args_and_check_external_param_types(
                display_name,
                qualified_name,
                args,
                sig,
                elem,
            );
        } else {
            self.check_external_overload_set(qualified_name, args, overloads, elem);
        }
    }

    /// GH #47: several overloads accept the call's arity — check argument
    /// types against every candidate. When NO candidate can bind and at
    /// least one rejection rests on concrete evidence (primitive/primitive
    /// mismatch, or a literal bound to an `&out` param), emit
    /// `NoMatchingSignatures`, mirroring the game compiler's
    /// `UI::InputText(label, str, false)` rejection. Named/unknown-shape
    /// args never reject a candidate on their own (FP guard).
    fn check_external_overload_set(
        &mut self,
        qualified_name: &str,
        args: &[CallArg],
        overloads: &[OverloadSig],
        elem: Option<&TypeRepr>,
    ) {
        // Compute each argument's type once (this also walks the args for
        // nested diagnostics — callers must not pre-walk).
        let mut arg_tys: Vec<TypeRepr> = Vec::with_capacity(args.len());
        let mut arg_literals: Vec<bool> = Vec::with_capacity(args.len());
        let mut arg_bool_literals: Vec<bool> = Vec::with_capacity(args.len());
        for a in args {
            arg_tys.push(self.expr_type(&a.value));
            arg_literals.push(arg_is_const_literal(&a.value));
            arg_bool_literals.push(matches!(a.value.kind, ExprKind::BoolLit(_)));
        }
        let mut any_compatible = false;
        let mut evidence_span: Option<Span> = None;
        for sig in overloads
            .iter()
            .filter(|sig| args.len() >= sig.min_args && args.len() <= sig.param_types.len())
        {
            let mut compatible = true;
            for (i, arg_ty) in arg_tys.iter().enumerate() {
                if arg_ty.is_error() {
                    continue;
                }
                // Named args bind by parameter name (unknown name → neutral);
                // positional args bind left-to-right.
                let slot = match &args.get(i).and_then(|a| a.name.as_ref()) {
                    Some(name) => {
                        let needle = name.text(self.source);
                        match sig
                            .param_names
                            .iter()
                            .position(|p| p.as_deref() == Some(needle))
                        {
                            Some(slot) => slot,
                            None => continue,
                        }
                    }
                    None => {
                        if i >= sig.param_types.len() {
                            continue;
                        }
                        i
                    }
                };
                let Some(param_text) = sig.param_types.get(slot) else {
                    continue;
                };
                let param_out = sig.param_out.get(slot).copied().unwrap_or(false);
                // A BOOL literal can never bind an `&out` slot; numeric
                // literals bind via temporaries (game probe: outmap,
                // 2026-08-18 — `UI::Begin("w2", true)` rejected,
                // `Text::TryParseFloat("1.5", 0.0)` accepted). Non-literal
                // rvalues stay neutral (l-value detection is conservative).
                if param_out && arg_bool_literals[i] {
                    compatible = false;
                    evidence_span = Some(args[i].value.span);
                    continue;
                }
                let param_ty = TypeRepr::parse_type_string(param_text.trim());
                if param_ty.is_error() {
                    continue;
                }
                if Self::is_unsubstituted_generic_or_any(&Self::peel_const_handle(&param_ty)) {
                    continue;
                }
                // B023: a candidate whose parameter resolves to a funcdef
                // must be able to bind a function-shaped arg (ctor-path
                // bind test); a provably non-function arg rejects the
                // candidate outright. Named/unknown shapes stay neutral
                // (arg_fails_funcdef_slot's FP guard).
                if let Some(fd) = self.slot_funcdef_templated(param_text, elem) {
                    if self.arg_fails_funcdef_slot(&fd, &args[i].value, arg_ty) {
                        compatible = false;
                        evidence_span = Some(args[i].value.span);
                    }
                    continue;
                }
                // B023: a resolvable function / funcdef-typed value can
                // never bind a KNOWN non-funcdef parameter (the game's
                // positional binding rejects the candidate there — a bare
                // wrong-sig fn into UI::InputText otherwise survives on the
                // shifted 5-arg overload). Unknown param shapes stay
                // neutral.
                if self.arg_is_function_shaped(&args[i].value, arg_ty)
                    && !param_ty.is_error()
                    && !Self::is_unsubstituted_generic_or_any(&Self::peel_const_handle(&param_ty))
                {
                    compatible = false;
                    evidence_span = Some(args[i].value.span);
                    continue;
                }
                let arg_core = Self::peel_const_handle(arg_ty);
                let param_core = Self::peel_const_handle(&param_ty);
                if matches!(
                    (arg_core, param_core),
                    (TypeRepr::Primitive(_), TypeRepr::Primitive(_))
                ) && !is_convertible(arg_core, param_core)
                {
                    compatible = false;
                    evidence_span = Some(args[i].value.span);
                }
                // Core flag enums bind their underlying integer slots even
                // though source expressions resolve to named values. Keep
                // rejecting known math value types, which are distinct from
                // integer parameters and have no implicit conversion.
                if matches!(arg_core, TypeRepr::Named(arg_n) if
                    !self.scope.has_enum(arg_n) && is_math_value_type(arg_n))
                    && matches!(param_core, TypeRepr::Primitive(_))
                {
                    compatible = false;
                    evidence_span = Some(args[i].value.span);
                }
                // Named / handle / mixed shapes stay neutral — the strict
                // checks live on the unique-overload path.
            }
            if compatible {
                any_compatible = true;
            }
        }
        if !any_compatible {
            if let Some(span) = evidence_span {
                let text =
                    render_no_matching_sig(qualified_name, &self.rendered_args(args, &arg_tys));
                self.diagnostics.push(TypeDiagnostic {
                    span,
                    kind: TypeDiagnosticKind::NoMatchingSignatures { text },
                });
            }
        }
    }

    /// Type every argument expression exactly once.
    ///
    /// Split out because three workspace call paths need the SAME arg types
    /// for two different checks, and `expr_type` is not idempotent — it
    /// pushes the nested diagnostics of the argument subtree, so a second
    /// pass duplicates every one of them.
    ///
    /// It used to return a parallel "was this a literal?" flag as well. B108
    /// removed the last reader when `render_no_matching_sig` stopped taking
    /// parallel slices, but the producer survived: three callers were still
    /// building that vector and passing it to a parameter nobody read. Found
    /// by two lanes independently, from the `unused_variables` warning that
    /// was failing master's own clippy-delta gate — which is the argument for
    /// treating a lint failure on master as a defect rather than noise. The
    /// literal flags are still computed where they ARE read, at their own
    /// call sites; this function no longer computes them for nobody.
    fn type_args_once(&mut self, args: &[CallArg]) -> Vec<TypeRepr> {
        let mut arg_tys = Vec::with_capacity(args.len());
        for a in args {
            arg_tys.push(self.expr_type(&a.value));
        }
        arg_tys
    }

    /// B089: B023's funcdef-slot bind test, run over a call to a WORKSPACE
    /// callable.
    ///
    /// Nothing here re-implements the comparison. [`Self::slot_funcdef`]
    /// resolves a parameter whose text names a funcdef and
    /// [`Self::arg_fails_funcdef_slot`] carries B023's false-positive guards;
    /// both already ran on the external call path and at B076's four
    /// declaration positions. The gap this closes is purely POSITIONAL —
    /// `Maker(MakeDistinct)` and `Maker@ b = MakeDistinct;` were rejected
    /// while `Take(MakeDistinct)` was accepted, so the LSP gave two answers
    /// for one binding depending on where it was written.
    ///
    /// **An overload set is not a declaration, and that is the whole reason
    /// this is not simply the external check.** A candidate rejecting the
    /// argument is not the call being wrong: only when EVERY arity-compatible
    /// candidate is rejected does anything get reported. B060 is the standing
    /// demonstration of getting that backwards — widening the
    /// function-shaped rule there killed whole overload sets on one
    /// candidate's verdict, and the test that pins this lives in the same
    /// fixture as the errors.
    ///
    /// It is deliberately NARROWER than the external path in a second way: a
    /// candidate is only ever rejected here by a FUNCDEF slot. A
    /// function-shaped argument meeting a non-funcdef parameter leaves the
    /// candidate neutral, so a set with no funcdef parameter at that position
    /// can never fire. That is less than the engine does; widening it is
    /// B023's scope decision to reopen, and B060 is what widening it cost.
    ///
    /// Never walks `args` — callers pass the types from
    /// [`Self::type_args_once`].
    fn report_workspace_funcdef_slot_failure(
        &mut self,
        qualified_name: &str,
        args: &[CallArg],
        arg_tys: &[TypeRepr],
        overloads: &[OverloadSig],
    ) {
        let mut evidence: Option<Span> = None;
        let mut any_candidate = false;
        for sig in overloads
            .iter()
            .filter(|sig| args.len() >= sig.min_args && args.len() <= sig.param_types.len())
        {
            any_candidate = true;
            let mut rejected: Option<Span> = None;
            let mut next_positional = 0usize;
            for (i, arg) in args.iter().enumerate() {
                let named = arg.name.as_ref().map(|n| n.text(self.source));
                let ArgBind::Index(slot) =
                    call_site::bind_arg(named, &sig.param_names, &mut next_positional)
                else {
                    continue;
                };
                let Some(param_text) = sig.param_types.get(slot) else {
                    continue;
                };
                let Some(arg_ty) = arg_tys.get(i) else {
                    continue;
                };
                if arg_ty.is_error() {
                    continue;
                }
                let Some(fd) = self.slot_funcdef_in_scope(param_text, qualified_name) else {
                    continue;
                };
                if self.arg_fails_funcdef_slot(&fd, &arg.value, arg_ty) {
                    rejected = Some(arg.value.span);
                }
            }
            match rejected {
                Some(span) => evidence = evidence.or(Some(span)),
                // A candidate that no funcdef slot rejects keeps the whole
                // call alive. Bail immediately rather than accumulate.
                None => return,
            }
        }
        if !any_candidate {
            return;
        }
        if let Some(span) = evidence {
            let text = render_no_matching_sig(qualified_name, &self.rendered_args(args, arg_tys));
            self.diagnostics.push(TypeDiagnostic {
                span,
                kind: TypeDiagnosticKind::NoMatchingSignatures { text },
            });
        }
    }

    /// For primitive-typed args whose corresponding declared parameter type
    /// is also a primitive, emit an `ArgTypeMismatch` when they differ.
    /// Non-primitive arg types, unknown param types (non-primitive text),
    /// and error types are all silently skipped — this is deliberately
    /// conservative, mirroring `ReturnTypeMismatch`'s primitive-only strategy.
    ///
    /// Binding rules (AngelScript):
    /// - Unnamed (positional) args bind left-to-right to successive parameters.
    /// - Named args (`name: value`) bind to the matching parameter by name and
    ///   may skip optional/defaulted parameters.
    ///
    /// Takes already-computed arg types (B089): the caller walks once via
    /// [`Self::type_args_once`] and shares the result with the funcdef-slot
    /// check, because typing an argument twice duplicates the nested
    /// diagnostics of its subtree.
    fn check_arg_types_against_params(
        &mut self,
        display_name: &str,
        args: &[CallArg],
        arg_tys: &[TypeRepr],
        params: &[(String, String)],
    ) {
        let mut next_positional = 0usize;
        for (i, arg) in args.iter().enumerate() {
            let named = arg.name.as_ref().map(|n| n.text(self.source));
            match call_site::bind_arg_workspace(named, params, &mut next_positional) {
                ArgBind::UnknownName => {
                    // Unknown parameter name — no ArgTypeMismatch. The value
                    // was already typed, so its nested diagnostics stand.
                    continue;
                }
                ArgBind::Index(param_index) => {
                    let Some(arg_ty) = arg_tys.get(i).cloned() else {
                        continue;
                    };
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
                // 2. Implicit `this.method()` — find on current class. The
                //    callee expression's recorded type is the method's
                //    return type (queries like hover read it); silence
                //    sentinels here used to surface as `(local) <error>`.
                if let Some(ty) = self.lookup_class_member(&name) {
                    self.record_implicit_member_use(&name);
                    self.walk_args(args);
                    return ty;
                }
                if let Some(ty) = self
                    .current_class()
                    .and_then(|cls| self.scope.workspace_class_member(&cls.name, &name))
                {
                    self.record_implicit_member_use(&name);
                    self.walk_args(args);
                    return ty;
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
                        self.record_use(&qualified);
                        return self.resolve_workspace_function_call(
                            &name,
                            &qualified,
                            args,
                            callee.span,
                            t,
                        );
                    }
                    if let Some(ty) = self.try_funcdef_ctor(&qualified, args, callee.span) {
                        return ty;
                    }
                    if self.scope.has_type(&qualified) {
                        self.record_use(&qualified);
                        self.walk_args(args);
                        return TypeRepr::Named(qualified);
                    }
                    // B100: an overload set never satisfied the
                    // `lookup_function_return` at the top of this iteration,
                    // so it arrived here — checked by nothing. Ahead of the
                    // arm that swallows it, and behind everything else, so
                    // the only calls it intercepts are the ones that were
                    // reaching that arm.
                    if let Some(t) =
                        self.try_workspace_overload_set_call(&name, &qualified, args, callee.span)
                    {
                        return t;
                    }
                    if self.scope.has_global_ident(&qualified) {
                        self.record_value_use_variants(&qualified);
                        self.walk_args(args);
                        return TypeRepr::Error(String::new());
                    }
                }
                // 4. Top-level function.
                if let Some(t) = self.scope.lookup_function_return(&name) {
                    self.record_use(&name);
                    return self.resolve_workspace_function_call(
                        &name,
                        &name,
                        args,
                        callee.span,
                        t,
                    );
                }
                // 5. Funcdef constructor (`Handler(OnThing)`,
                //    `CoroutineFunc(fn)`). Must run before the generic
                //    `has_type` → `Named` path (GH #63).
                if let Some(ty) = self.try_funcdef_ctor(&name, args, callee.span) {
                    return ty;
                }
                // 6. Maybe it's a type-constructor form that slipped in
                //    as an Ident — surface the type when possible so
                //    chained `.member` access off a constructor can
                //    still resolve. Otherwise just silence.
                if self.scope.has_type(&name) {
                    // No `record_use` here, and that is MEASURED rather than
                    // assumed. This arm looks like it is missing the
                    // `record_use(&qualified)` its namespaced twin at step 3
                    // has, and a line was added here on that reasoning — but
                    // removing it again left
                    // `a_class_used_only_as_a_constructor_call_is_credited`
                    // green, so a workspace class reached by its constructor
                    // call is already credited by another route. The line was
                    // inert; an inert line that carries a comment claiming a
                    // bug is worse than no line.
                    let mut arg_tys = Vec::with_capacity(args.len());
                    for a in args {
                        arg_tys.push(self.expr_type(&a.value));
                    }
                    // GH #49: `int3(nat3_value)` matches no constructor.
                    self.check_math_value_ctor(
                        &TypeRepr::Named(name.clone()),
                        &arg_tys,
                        callee.span,
                    );
                    return TypeRepr::Named(name);
                }
                if let Some(resolved) = self.scope.resolve_unqualified(&name) {
                    self.walk_args(args);
                    return TypeRepr::Named(resolved);
                }
                // B071: the workspace's own declaration outranks an engine
                // enum that merely shares this SHORT name.
                //
                // Fleet-measured, not anticipated: `tm-editor-plus-plus`
                // declares `enum Axis` and writes `Axis(c)`, and the
                // short-name rescue below handed back the Core enum
                // `UI::Plot::Axis`. Nothing compared the two until enum
                // conversions were checked, at which point the plugin's enum
                // was reported incompatible with itself — the only false
                // positive the enum work produced across 151 real plugins,
                // and the mismatch report was right about the types it was
                // given. The wrong half was here.
                //
                // `workspace_shadowing_type_name` (B073) rather than the
                // bare-boolean `workspace_declares_type`, because two
                // predicates answering one question is how they drift
                // (B079: compose, never merely add). B073 found the same
                // rule at a different call site and needed the WINNER's
                // qualified name, since a shadowing declaration may not be
                // spelled the way the reference is. Taking the name here
                // hands the enum-conversion check a canonical key instead of
                // making it re-derive one.
                //
                // Honest limit on that, so the next reader does not test for
                // a behaviour difference there is not: TODAY the two spell
                // the same string at this position. The predicate's exact
                // arm returns `name` itself, and its tail arm — the one that
                // could return `Nz::Axis` for a bare `Axis` — is already
                // consumed by `resolve_unqualified` one fallback above,
                // which scans the same tail table. The qualified form is
                // correctness insurance if that ordering ever changes, not
                // an observable fix. Its exclusion of real funcdefs is right
                // here too: step 5 above already owns that case.
                //
                // THAT MASK IS NOW REDUNDANT, and this note is kept because
                // the warning it replaces was right. This guard used to be
                // the only thing standing between a bare call form and a
                // namespaced engine enum, and it covered only the names the
                // workspace happened to declare -- so the fleet sweep going
                // from one delta to zero was the mask, not the rule being
                // clean. B098 deleted the short-name rescue outright (see
                // below), so the 436 spellings the game rejects no longer
                // resolve here for any workspace. The guard still earns its
                // place: it decides WHICH declaration wins when the
                // workspace does declare the name, which is a different
                // question from whether an engine enum may be reached bare.
                if let Some(declared) = self.scope.workspace_shadowing_type_name(&name) {
                    // B084 parity, and this guard would otherwise UNDO it.
                    // The `has_global_ident` arm below credits every name
                    // that reaches it; returning above that arm without
                    // recording reintroduces the false "never used" for an
                    // enum whose only reference is this cast-call form —
                    // `tostring(WheelOrder(w))` is B084's own example.
                    // Pinned by `b071_shadowing_guard_credits_the_enum` in
                    // `analysis::unused`, which is the only harness that can
                    // see it: `compute_diagnostics` runs no unused analysis.
                    self.record_use(&declared);
                    self.walk_args(args);
                    return TypeRepr::Named(declared);
                }
                // B098: a bare short name used to reach a NAMESPACED engine
                // enum here, via `external_enum_by_short_name` — so `Axis(1)`
                // resolved to the Core enum `UI::Plot::Axis` and the call went
                // silent. The game answers `Identifier 'Axis' is not a data
                // type in global namespace`, measured for all 436 such bare
                // spellings in the committed B052 corpus.
                //
                // The lookup is GONE rather than narrowed, because measurement
                // showed it rescued nothing: all 62 bare spellings the game
                // does accept are answered by `resolve_unqualified` on the
                // line above and never reached it, while all 436 it rejects
                // reached it and only it. A predicate whose entire contribution
                // is false positives has no narrowing worth writing.
                //
                // B056 closed this same class in `resolve_unqualified`, which
                // deliberately excludes enums from its short-name rescue. This
                // was the second site that fix did not reach.
                // B084: the top-level twin of the namespaced arm above,
                // which has always done `record_value_use_variants`. This
                // one silenced the undefined-identifier diagnostic and
                // recorded NOTHING, so every callee that reached it was
                // reported "never used" however many times it was called.
                //
                // What reaches it is mostly OVERLOAD-shaped. Step 4 credits
                // a call through `lookup_function_return`, whose workspace
                // fallback `lookup_workspace_function_property` returns
                // `None` as soon as TWO function symbols share the name —
                // the deliberate "ambiguous, say nothing" arm. So a single
                // global function was credited and an overload set never
                // was. Measured on the tm-editor-plus-plus fixture: the
                // 10-way overloaded `LabeledValue`, called from dozens of
                // sites, produced 10 false warnings. The equally overloaded
                // `CoordToPos` did not, only because it also has an export
                // stub and B062 roots it — which is why the defect stayed
                // invisible.
                //
                // A global funcdef HANDLE invoked as a call (`g_cb()`)
                // lands here too, via `has_global_ident`'s Variable arm.
                //
                // `record_value_use_variants`, not `record_use`, for exact
                // parity with the namespaced arm; the extra `get_`/`set_`
                // and namespace-ladder spellings over-mark, which is this
                // module's stated preference, and they cannot mask a real
                // finding here: reaching this arm means the ladder already
                // failed to find `Ns::name` at every enclosing depth.
                // B100: the top-level twin of the ladder's entry point. Same
                // position for the same reason — immediately in front of the
                // arm that was swallowing these calls, behind every other
                // resolution, so nothing else changes hands.
                if let Some(t) =
                    self.try_workspace_overload_set_call(&name, &name, args, callee.span)
                {
                    return t;
                }
                if self.scope.has_global_ident(&name) {
                    self.record_value_use_variants(&name);
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
                // Resolved receiver type: `obj.member(...)` counts as a use
                // of `Type::member` across the inheritance chain.
                self.record_member_use_from_type(&type_name, &member_name);
                // Removed-API member names on engine types (GH #27/#28
                // family). `awaitable.WithRunContext` compiles clean but is
                // a silent runtime no-op: the run context is discarded
                // (game evidence 2026-08-23, DipsItemExperiment /
                // MLFeedRaceData). Fires before arity/type machinery so the
                // hint surfaces even where typedb member metadata is
                // missing.
                if type_name == "awaitable"
                    && member_name == "WithRunContext"
                    && self.scope.is_external_type(&type_name)
                {
                    self.diagnostics.push(TypeDiagnostic {
                        span: callee.span,
                        kind: TypeDiagnosticKind::RemovedApiUsage {
                            member: member_name.clone(),
                            hint: "Meta::StartWithRunContext".to_string(),
                            // Same reasoning as the member-access site: the
                            // migration moves the receiver into the call, so
                            // there is no correct span swap. The
                            // `with_run_context` family (B048) restructures
                            // the call instead; this is the site it reads,
                            // and `span` is the member EXPRESSION it anchors
                            // its search on.
                            replacement: None,
                        },
                    });
                    self.walk_args(args);
                    return TypeRepr::Error(String::new());
                }
                // B003: external method arity (unique + multi with no match).
                // B007: after arity OK, type-check Named/primitive args against
                // the unique matching overload's param types (incl. Nadeo).
                let overloads = self.scope.callables_method(&type_name, &member_name);
                if !overloads.is_empty() {
                    let ranges = call_site::arity_ranges(&overloads);
                    self.check_arity_against_ranges(&member_name, &ranges, args.len(), callee.span);
                    let qualified_method = format!("{type_name}::{member_name}");
                    // B102: the only site with a `T` to substitute. An
                    // `array<T>` receiver carries the element type that
                    // `T[]::less` needs; everywhere else passes `None`, so
                    // the synthesised comparator signature cannot fire
                    // where there is nothing to substitute.
                    let elem = obj_ty.array_element_type().cloned();
                    self.check_external_call_arg_types(
                        &member_name,
                        &qualified_method,
                        args,
                        &overloads,
                        elem.as_ref(),
                    );
                } else if let Some(ranges) = self
                    .scope
                    .lookup_external_method_arity_ranges(&type_name, &member_name)
                {
                    self.check_arity_against_ranges(&member_name, &ranges, args.len(), callee.span);
                    self.walk_args(args);
                } else {
                    // B089: a WORKSPACE class method. `callables_method` is
                    // typedb-only, so this arm has always been the "nothing
                    // known about the parameters" fallback — but the workspace
                    // symbol table does know them, and a funcdef-typed
                    // parameter on a plugin's own method was the one binding
                    // position with no check of any kind.
                    //
                    // Only the funcdef-slot rule is run here. Turning this arm
                    // into a general workspace-method arg-type check is a much
                    // larger change with its own false-positive surface, and
                    // this ticket's scope is the slot check's missing
                    // positions.
                    let qualified_method = format!("{type_name}::{member_name}");
                    let method_overloads =
                        self.method_overloads_including_inherited(&qualified_method);
                    let arg_tys = self.type_args_once(args);
                    self.report_workspace_funcdef_slot_failure(
                        &qualified_method,
                        args,
                        &arg_tys,
                        &method_overloads,
                    );
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
                // B114: constness comes from the POOL, for the same-file arm
                // as well as B116's cross-file one below, so there is ONE
                // source of truth rather than two that can drift.
                //
                // This retired `file_const_methods` and `file_class_methods`,
                // two per-file AST maps built by `index_file_classes`.
                // `workspace_own_method_is_const` reproduces BOTH exactly:
                //
                // * `Some(true)` iff any overload of the name is const —
                //   `file_const_methods` was likewise a set keyed by name, so
                //   one const declaration made it `contains`.
                // * The `get_V`/`set_V` accessor spelling, which the sidecar
                //   recorded under the STRIPPED property name.
                // * `None` for a FIELD, which is the method-vs-field
                //   discrimination `file_has_method` used to provide.
                //
                // Own-class scope is preserved deliberately: the sidecar
                // recorded only a class's own members, so this is the surface
                // that has to match it. An inherited non-const method reached
                // through a derived const receiver stays a false negative on
                // BOTH sides of the file boundary — B119, and not this
                // ticket's to widen, because the fleet exposure of walking
                // parents was measured for own-class scope only.
                let method_constness = self
                    .scope
                    .workspace_own_method_is_const(&type_name, &member_name);
                // Same-file workspace classes: prefer `file_classes` so we
                // get the resolved return type for const-propagation.
                if let Some((_, members)) = self.file_classes.get(&type_name) {
                    for (mname, t) in members {
                        if mname == &member_name {
                            let ret = t.clone();
                            // `None` still WRAPS but never diagnoses: a field
                            // read through a const receiver is genuinely
                            // const, so its type must inherit `Const(_)`,
                            // while firing on it would be a false positive.
                            // Only `Some(false)` — a known non-const METHOD —
                            // is the game-parity error (HatDump.as:60).
                            if receiver_const && method_constness != Some(true) {
                                if method_constness == Some(false) {
                                    self.diagnostics.push(TypeDiagnostic {
                                        span: callee.span,
                                        kind: TypeDiagnosticKind::NonConstMethodOnConstReceiver {
                                            method: member_name.clone(),
                                            object_type: type_name.clone(),
                                        },
                                    });
                                }
                                // B121: only an ALIASING return inherits the
                                // receiver's constness. A by-value return is a
                                // fresh object and writing to it is legal, so
                                // const-wrapping it invents a second, false
                                // diagnostic on top of the true one above.
                                if Self::method_return_may_alias_receiver(&ret) {
                                    return Self::apply_receiver_const(ret, true);
                                }
                                return ret;
                            }
                            return ret;
                        }
                    }
                }
                // B116: the same rule, one file boundary further out.
                // `file_classes` is built by `index_file_classes`, which
                // walks ONE document, so the arm above can only ever see a
                // class declared in THIS file — a const receiver whose class
                // lived in a sibling file fell straight through to
                // `lookup_method_return` and was silently unchecked. The
                // gate was `file_classes`, never the const set, which is why
                // pooling `is_const` (B101) did not fix it on its own.
                //
                // Provenance is what makes widening safe. The constness
                // comes from `workspace_own_method_is_const`, which reads
                // the WORKSPACE half of the merged view only, so an engine
                // receiver answers `None` and stays unchecked — the typedb
                // has no const-qualifier metadata at all, and treating
                // "not known const" as "non-const" would fire on it
                // uniformly. Measured on the 151-plugin fleet: 827 const
                // receiver call sites reach here, 807 of them engine or
                // builtin types (`string`, `Json::Value`, `vec3`, `array`,
                // …) that `None` correctly excludes.
                //
                // Same-file classes are excluded outright rather than
                // re-decided, so the arm above remains the single authority
                // for anything it can see and this can only ever ADD the
                // cross-file case. B114 note: both arms now read the SAME
                // `method_constness` computed once above — before, this one
                // consulted the pool while the arm above consulted an AST
                // sidecar, which is exactly the pair of answers that could
                // drift apart.
                if receiver_const
                    && !self.file_classes.contains_key(&type_name)
                    && method_constness == Some(false)
                {
                    self.diagnostics.push(TypeDiagnostic {
                        span: callee.span,
                        kind: TypeDiagnosticKind::NonConstMethodOnConstReceiver {
                            method: member_name.clone(),
                            object_type: type_name.clone(),
                        },
                    });
                    // B120: propagate the receiver's constness into the
                    // return type, exactly as the same-file arm does, so a
                    // downstream write (`c.GetArr()[0] = 5`) fires
                    // `ConstViolation` on both sides of the file boundary.
                    // Without this the two halves of one rule disagreed
                    // about how far const travels: the CALL was diagnosed
                    // cross-file (B116) but nothing derived from it was.
                    //
                    // The read is `workspace_class_member`, the WORKSPACE
                    // half of the merged view — the same structural
                    // provenance as the constness decision above, rather
                    // than a filter applied afterwards that a later edit
                    // could quietly drop.
                    //
                    // Propagation is deliberately confined to the arm that
                    // already emitted. That is the measured boundary, not a
                    // shortcut. AC19 wraps the result of a call that is
                    // ALREADY an error, so the cascade only ever lands on
                    // code the game would reject anyway. Widening it breaks
                    // parity in the other direction: on the 151-plugin fleet
                    // an EXTERNAL-receiver variant produced 10 false
                    // `ConstViolation`s across 7 plugins, every one an
                    // engine method returning a FRESH value rather than a
                    // view into the receiver (`const string &in s` then
                    // `s.Replace(..)` / `s.ToLower()` / `s.Split(..)`). The
                    // typedb cannot tell those apart, which is what "no
                    // const-qualifier metadata yet" costs in practice.
                    if let Some(t) = self.scope.workspace_class_member(&type_name, &member_name) {
                        // B121: same aliasing gate as the same-file arm.
                        if Self::method_return_may_alias_receiver(&t) {
                            return Self::apply_receiver_const(t, true);
                        }
                        return t;
                    }
                }

                // B119: the same rule, one INHERITANCE level further in.
                // Both arms above are own-class-only -- `file_classes` holds
                // a class's own members and `workspace_own_method_is_const`
                // reads its own pooled methods -- so an inherited non-const
                // method reached through a derived const receiver was silent
                // on BOTH sides of the file boundary, identically. B116 pinned
                // that parity deliberately rather than closing it, because
                // whether the engine rejects it was unmeasured.
                //
                // It does, at depths 1, 2 and 4 and for every receiver
                // spelling, and it accepts the inherited CONST case:
                // `No matching signatures to 'D::Mutate() const'`. Measured by
                // `scripts/probe_const_receiver.py` against Openplanet, with
                // `ctl-own-nonconst` proving the oracle can see a const
                // violation at all.
                //
                // Guarded on the own class NOT declaring the name, so an
                // override decides its own constness rather than this walk
                // reaching past it to a base declaration.
                //
                // The guard is NOT what prevents a double diagnostic in the
                // common case: since B120 the cross-file arm above returns
                // when `workspace_class_member` resolves, and the same-file
                // arm has always returned on a match, so neither reaches
                // here. B121 restructured that return into two branches --
                // const-wrapped when the return may alias, bare when it
                // cannot -- and BOTH still return, so the reachability is
                // unchanged. Verified by mutation: removing `!own_declares`
                // changes no test. It is kept for the residual path where the
                // cross-file arm fires and `workspace_class_member` answers
                // `None`, which falls through.
                //
                // This arm deliberately does NOT propagate constness into the
                // return type at all, neither B120's unconditional form nor
                // B121's shape-gated one. Propagation through an inherited
                // method has never been measured on the fleet, and B121's own
                // result -- that the shape proxy removes 7 of 10 false
                // positives and leaves 3 it cannot see -- is the reason to
                // stop rather than to extend it by analogy.
                // B114: one lookup, not two. This read `file_has_method`
                // (same-file, AST) OR the pooled question; retiring that
                // sidecar leaves the pooled call, which already answers for
                // both because the file being checked is itself in the pool.
                // `is_some()` is exactly "the own class declares this name",
                // since `None` is precisely the not-a-workspace-method case.
                let own_declares = self
                    .scope
                    .workspace_own_method_is_const(&type_name, &member_name)
                    .is_some();
                if receiver_const
                    && !own_declares
                    && self.inherited_method_is_const(&type_name, &member_name) == Some(false)
                {
                    self.diagnostics.push(TypeDiagnostic {
                        span: callee.span,
                        kind: TypeDiagnosticKind::NonConstMethodOnConstReceiver {
                            method: member_name.clone(),
                            object_type: type_name.clone(),
                        },
                    });
                }
                // B130: the same rule, on ENGINE receivers.
                //
                // B116 excluded this whole population by provenance, arguing
                // partly that no const-qualifier metadata existed. B124 showed
                // that was stale and B125 made it arrive, so the exclusion is
                // now a choice rather than a constraint — and both halves of
                // that choice were measured before it was made.
                //
                // FALSE POSITIVES, on the 151-plugin fleet: 807 unique engine
                // receiver call sites, and the typedb calls all 807 of them
                // CONST. Zero would-fire, and — the stronger half — zero
                // UNKNOWN, so the typedb has an answer at every single site.
                // The feared surface is measured at zero rather than argued
                // down. (B116's "807 would fire" was an artefact of a gate
                // reading "not known const" as "non-const"; with the real
                // qualifier none of them do.)
                //
                // CORRECTNESS, which the fleet cannot answer because shipped
                // plugins compile and so cannot contain a true positive:
                // asked of the engine directly, `scripts/probe_engine_const_receiver.py`,
                // three controls passing. The engine REJECTS a non-const
                // engine method on a const receiver —
                //   `MemoryBuffer::Seek`  -> "No matching signatures to
                //                            'MemoryBuffer::Seek(const int) const'"
                //   `Json::Value::Add`    -> "No matching signatures to
                //                            'Value::Add(Json::Value@&) const'"
                //   `const string &in` += -> "Illegal operation on 'const string&'"
                // — and still ACCEPTS a const one on all three, so this is the
                // engine's own rule rather than a tightening chosen here.
                //
                // `Some(true)` covers the overload case: `Json::Value::Get`
                // and `opIndex` each ship a const and a non-const overload,
                // the engine selects the const one on a const receiver, and
                // the fleet's 63 `Get` sites correctly stay silent.
                //
                // NO CONST PROPAGATION here, deliberately. B121 measured
                // exactly that at 10 false positives across 7 plugins —
                // engine methods returning a FRESH value, not a view — and
                // its shape proxy removes only 7 of them. Diagnosing the call
                // and propagating from it are separate questions with
                // separate evidence; this arm answers only the first.
                if receiver_const
                    && self
                        .scope
                        .external_method_is_const(&type_name, &member_name)
                        == Some(false)
                {
                    self.diagnostics.push(TypeDiagnostic {
                        span: callee.span,
                        kind: TypeDiagnosticKind::NonConstMethodOnConstReceiver {
                            method: member_name.clone(),
                            object_type: type_name.clone(),
                        },
                    });
                }
                if let Some(t) = self.scope.lookup_method_return(&type_name, &member_name) {
                    // EXTERNAL types are not const-checked here. The reason
                    // is NOT that the metadata is missing — that claim used
                    // to be written here and it was stale (B124).
                    //
                    // The dumps DO carry engine method constness, in the
                    // `decl` string: 301 of 1002 Core class methods end in
                    // ` const`, and `CoreMethod::declared_const` reads it.
                    // What is genuinely absent, at every level, is whether a
                    // returned HANDLE aliases the receiver. `string::Split`
                    // returns `string[]@` — a fresh array — and
                    // `Json::Value::Get` returns a real view into its
                    // receiver; both are `returntypeflags` 0 with a `@`, so
                    // nothing separates them.
                    //
                    // That is why this arm stays silent, and it is a
                    // narrower reason than the old comment gave. Consuming
                    // engine constness here would let the rule fire on the
                    // engine population, which B116 measured at 807 excluded
                    // receivers and B121 measured at 80 handle-returning
                    // sites, 3 of which are provably indistinguishable from
                    // views. It needs its own variant sweep before anyone
                    // turns it on — see B124.
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
                // Game parity (2026-08-23): unknown METHOD call on a
                // PARENTLESS same-file workspace class — mirror of the
                // gate in `member_access_type`. Classes with bases stay
                // silent (unresolved parent could carry the method).
                if let Some((parents, _)) = self.file_classes.get(&type_name) {
                    if parents.is_empty() {
                        self.diagnostics.push(TypeDiagnostic {
                            span: callee.span,
                            kind: TypeDiagnosticKind::UndefinedMember {
                                object_type: type_name.clone(),
                                member: member_name.clone(),
                            },
                        });
                    }
                }
                // B081: the mint, as in `member_access_type`.
                let Some(external_receiver) = self.scope.external_type_name(&type_name) else {
                    return TypeRepr::Error(String::new());
                };
                // Same Nadeo completeness rule as `member_access_type` (B006):
                // suppress only when the typedb member list is empty.
                if self.scope.is_nadeo_type(&type_name)
                    && !self.scope.nadeo_member_list_trusted(&type_name)
                {
                    return TypeRepr::Error(String::new());
                }
                // Openplanet-injected methods (GH #21, B059). Absent from
                // the Nadeo dump, so the overload/arity machinery above
                // found nothing to check and `args` were walked unchecked:
                // this table is their only arity authority, so check it
                // here. Unlike the member-access mirror of this gate, the
                // RETURN TYPE is load-bearing — `auto id = p.GetCurrentEntityID()`
                // must infer `uint`, not the silence sentinel.
                if let Some(inj) = self.injected_method(external_receiver, &member_name) {
                    self.check_arity_against_ranges(
                        &member_name,
                        &[inj.arity],
                        args.len(),
                        callee.span,
                    );
                    return Self::injected_method_return(inj);
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
                    self.record_use(&qual);
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
                        self.check_external_call_arg_types(display, &qual, args, &overloads, None);
                        return t;
                    }
                    if let Some(ranges) = self.scope.lookup_external_function_arity_ranges(&qual) {
                        self.check_arity_against_ranges(display, &ranges, args.len(), callee.span);
                    }
                    self.walk_args(args);
                    return t;
                }
                if let Some(ty) = self.try_funcdef_ctor(&qual, args, callee.span) {
                    return ty;
                }
                // B100: the `NS::Fn(...)` spelling of the same gap — gated
                // behind the same `lookup_function_return` above, and reaching
                // the same `has_global_ident` arm below. It goes AHEAD of the
                // `walk_args` on the next line rather than beside the arm it
                // guards, because the resolver walks the arguments itself and
                // walking them twice would double every nested diagnostic.
                if let Some(t) =
                    self.try_workspace_overload_set_call(display, &qual, args, callee.span)
                {
                    return t;
                }
                self.walk_args(args);
                if self.scope.has_type(&qual) {
                    self.record_use(&qual);
                    return TypeRepr::Named(qual);
                }
                if self.scope.has_global_ident(&qual) {
                    self.record_value_use_variants(&qual);
                    return TypeRepr::Error(String::new());
                }
                // GH #28 / #27: the call resolved to nothing workspace-side
                // and nothing external — consult the typedb before going
                // silent. Known API namespace + missing member, or absent
                // namespace + known removal, is a game-side `No matching
                // symbol` rejection. Unknown user namespaces stay quiet.
                if let Some(diag) = self.free_fn_no_matching_symbol(&qual, callee.span) {
                    self.diagnostics.push(diag);
                }
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

    /// GH #28 / #27 / #58: `Ns::Fn(...)` resolved to nothing. Decide
    /// whether we have enough confidence to diagnose the game-compiler
    /// `No matching symbol 'Ns::Fn'` rejection.
    ///
    /// The typedb owns every namespace it knows about — it is the only
    /// authority on the engine API surface — so its two triggers run first
    /// and, when `Ns` is a typedb namespace, they alone decide:
    ///
    /// - `Ns` has functions in the typedb (known API namespace) but `Ns::Fn`
    ///   is not among them → diagnose, optionally with near-name hints.
    /// - `Ns` is absent from the typedb entirely AND sits in the
    ///   removed-API relocation table (e.g. `Draw` on 1.29.5) → diagnose
    ///   with the replacement hint (`Display::GetWidth`).
    ///
    /// Otherwise the workspace half gets a turn
    /// ([`Self::workspace_ns_no_matching_symbol`], GH #58).
    fn free_fn_no_matching_symbol(&mut self, qual: &str, span: Span) -> Option<TypeDiagnostic> {
        let (ns, member) = qual.split_once("::")?;
        if let Some(idx) = self.scope.external() {
            if idx.namespace_has_functions(ns) {
                // Known API namespace: is the member really missing? The
                // lookup already failed above (caller reached the silent
                // path), but re-check to keep this helper self-contained.
                if idx.lookup_function(qual).is_some() {
                    return None;
                }
                // Near-name candidates soften the diagnostic (typo support).
                let members = idx.namespace_members(ns);
                let near: Vec<&String> = members
                    .iter()
                    .filter(|m| edit_distance(m, member) <= 2 && !m.is_empty())
                    .take(3)
                    .collect();
                let hint = if near.is_empty() {
                    None
                } else {
                    Some(SymbolHint::NearName(format!("{}::{}", ns, near[0])))
                };
                return Some(TypeDiagnostic {
                    span,
                    kind: TypeDiagnosticKind::NoMatchingSymbol {
                        qualified: qual.to_string(),
                        hint,
                    },
                });
            }
            if !idx.namespace_exists(ns) {
                if let Some(hint) = removed_api_hint(qual) {
                    return Some(TypeDiagnostic {
                        span,
                        kind: TypeDiagnosticKind::NoMatchingSymbol {
                            qualified: qual.to_string(),
                            hint: Some(SymbolHint::Relocation(hint.to_string())),
                        },
                    });
                }
            } else {
                // A typedb namespace with no functions (types/enums only):
                // trigger (a) already had its say. Don't let the workspace
                // rule second-guess the engine API surface.
                return None;
            }
        }
        self.workspace_ns_no_matching_symbol(qual, ns, member, span)
    }

    /// GH #58 triggers (c)+(d): `Ns::Fn(...)` where `Ns` is a namespace the
    /// WORKSPACE declares — the plugin's own module or a resolved
    /// dependency's export surface, which is exactly the set of files the
    /// game compiles together. When nothing named `Fn` sits under `Ns`, the
    /// game rejects the call (`No matching symbol 'TournamentStation::SendEvent'`,
    /// tournament-station 2026-08-23).
    ///
    /// AngelScript namespaces are open and additive across modules, so
    /// silence is the correct answer whenever the symbol set is not
    /// authoritative. The fences, in order:
    ///
    /// 1. `Ns` and `Fn` must both be non-empty, and `Fn` must be a single
    ///    segment — deeper paths (`A::B::Fn`) stay silent in this release.
    /// 2. Engine namespaces (`UI`, `Math`, …) belong to triggers (a)/(b):
    ///    a plugin may add helpers to `namespace UI` while still calling
    ///    the real API, which only the typedb can confirm.
    /// 3. The pooled table must be the plugin's complete compile surface
    ///    ([`GlobalScope::workspace_closure_complete`]) — an unread module
    ///    file may contribute the member. The five conditions behind that
    ///    flag are enumerated on
    ///    `workspace::load::PluginWorkspaceLoad::dependency_closure_complete`.
    /// 4. `Ns` must be declared as a namespace (a class is not one —
    ///    AngelScript has no statics, so `MyClass::Thing()` stays silent).
    /// 5. Nothing of ANY kind may match under it — function, class,
    ///    interface, enum, enum value, funcdef, namespace or global
    ///    variable, plus `get_`/`set_` virtual-property accessors.
    /// 6. Relative resolution must fail too: inside `namespace Outer`,
    ///    `Inner::Deep()` binds `Outer::Inner::Deep` even when a top-level
    ///    `Inner` exists.
    fn workspace_ns_no_matching_symbol(
        &self,
        qual: &str,
        ns: &str,
        member: &str,
        span: Span,
    ) -> Option<TypeDiagnostic> {
        if ns.is_empty() || member.is_empty() || member.contains("::") {
            return None;
        }
        if is_engine_namespace(ns) {
            return None;
        }
        if !self.scope.workspace_closure_complete() {
            return None;
        }
        if !self.scope.workspace_namespace_declared(ns) {
            return None;
        }
        if self.workspace_ns_resolves(ns, member) {
            return None;
        }
        // Relative resolution from each enclosing namespace prefix.
        for depth in (1..=self.namespace_stack.len()).rev() {
            let prefix = self.namespace_stack[..depth].join("::");
            let relative_ns = format!("{prefix}::{ns}");
            if self.scope.workspace_namespace_declared(&relative_ns)
                || self.workspace_ns_resolves(&relative_ns, member)
            {
                return None;
            }
        }
        // Near-name candidate, exactly like the typedb `UI::GetIO` path.
        // Additive only — it never gates the diagnostic (the motivating
        // repro has no near name at all).
        let hint = self
            .scope
            .workspace_namespace_member_names(ns)
            .into_iter()
            .filter(|m| edit_distance(m, member) <= 2)
            .min_by_key(|m| edit_distance(m, member))
            .map(|m| SymbolHint::NearName(format!("{ns}::{m}")));
        Some(TypeDiagnostic {
            span,
            kind: TypeDiagnosticKind::NoMatchingSymbol {
                qualified: qual.to_string(),
                hint,
            },
        })
    }

    /// True when the workspace declares anything named `member` under the
    /// namespace `ns` — any symbol kind (GH #58 decision 4), including a
    /// `get_`/`set_` virtual-property accessor pair.
    fn workspace_ns_resolves(&self, ns: &str, member: &str) -> bool {
        self.scope
            .workspace_declares_any(&format!("{ns}::{member}"))
            || self
                .scope
                .workspace_declares_any(&format!("{ns}::get_{member}"))
            || self
                .scope
                .workspace_declares_any(&format!("{ns}::set_{member}"))
    }

    fn expr_type(&mut self, expr: &Expr) -> TypeRepr {
        let ty = self.expr_type_inner(expr);
        if self.record_types {
            self.expr_types.insert(expr.span.start, ty.clone());
            self.span_expr_types
                .insert((expr.span.start, expr.span.end), ty.clone());
        }
        ty
    }

    /// Get-only virtual property write guard (todo-cover-cases-next.txt,
    /// spike-live-add-kinematic-ao). When the assignment LHS is a plain
    /// `obj.prop` member expression whose receiver type is a typedb value
    /// type with a get-only `prop` (e.g. `iso4.tx`), the game rejects the
    /// store with `The property has no set accessor`. Only fires on the
    /// external typedb's proven read-only set — workspace fields, Nadeo
    /// props and unknown types stay silent.
    fn check_readonly_prop_assign(&mut self, lhs: &Expr, position: AccessPosition) {
        let ExprKind::Member { object, member } = &lhs.kind else {
            return;
        };
        // Compute the receiver type directly, because the check-mode pass does
        // not populate the span map.
        //
        // B126. The previous comment here read "member-access typing is
        // read-only and idempotent, so walking the object again is safe" —
        // true of the TYPE it returns, false of its SIDE EFFECT. `expr_type`
        // pushes diagnostics, so re-walking an object that is a CALL emits
        // that call's diagnostics a second time: `c.Get().x = 1` produced two
        // `NonConstMethodOnConstReceiver` at the identical span. Idempotent in
        // its result is not idempotent in its effects, and only the first half
        // was checked.
        //
        // The sole caller has already typed `lhs`, which types this same
        // object, so everything this walk emits is a duplicate of a
        // diagnostic already recorded. Type here, keep nothing.
        let mark = self.diagnostics.len();
        let obj_ty = self.expr_type(object);
        self.diagnostics.truncate(mark);
        let Some(type_name) = Self::base_type_name(&obj_ty) else {
            return;
        };
        let prop = member.text(self.source).to_string();
        if self.scope.external_member_is_readonly(&type_name, &prop) {
            self.diagnostics.push(TypeDiagnostic {
                span: lhs.span,
                kind: TypeDiagnosticKind::ReadOnlyProperty {
                    property: format!("{type_name}.{prop}"),
                    compound: position.is_compound(),
                },
            });
        }
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
                    // Resolved to a member of this class (or an ancestor):
                    // counts as a use of that member (unused-decl).
                    self.record_implicit_member_use(&name);
                    return ty;
                }
                if let Some(ty) = self
                    .current_class()
                    .and_then(|cls| self.scope.workspace_class_member(&cls.name, &name))
                {
                    self.record_implicit_member_use(&name);
                    return ty;
                }
                // 3. Namespace-scoped global: try progressively shorter
                //    namespace prefixes.  Inside Ns "Outer::Inner", try
                //    "Outer::Inner::name" first, then "Outer::name".
                for depth in (1..=self.namespace_stack.len()).rev() {
                    let ns = self.namespace_stack[..depth].join("::");
                    let qualified = format!("{}::{}", ns, name);
                    if let Some(ty) = self.scope.lookup_global_value_type(&qualified) {
                        self.record_value_use_variants(&qualified);
                        return ty;
                    }
                    if self.scope.has_global_ident(&qualified) {
                        // Known name without a stored value type — silence
                        // rather than Named(varName) which false-positives
                        // ArgTypeMismatch against real param types.
                        self.record_value_use_variants(&qualified);
                        return TypeRepr::Error(String::new());
                    }
                }
                // B053: two or more enums in scope carry this value name, and
                // the game refuses to choose between them: `Found multiple
                // matching enum values`.
                //
                // BEFORE the global value lookup, not after: a global enum
                // the workspace declares is a CANDIDATE, not an answer, so
                // letting `lookup_global_value_type` resolve it first would
                // hide exactly the ambiguity the plugin's own enum created.
                // The lookups that do outrank the enums — a variable, a
                // function, a virtual property — are checked inside
                // `ambiguous_global_enum_value`, which stays quiet for them.
                if let Some(candidates) = self.scope.ambiguous_global_enum_value(&name) {
                    self.diagnostics.push(TypeDiagnostic {
                        span: expr.span,
                        kind: TypeDiagnosticKind::AmbiguousEnumValue {
                            name: name.clone(),
                            candidates,
                        },
                    });
                    self.record_value_use_variants(&name);
                    return TypeRepr::Error(String::new());
                }
                // 4. Global top-level lookup (vars, functions-as-values, enums).
                if let Some(ty) = self.scope.lookup_global_value_type(&name) {
                    self.record_value_use_variants(&name);
                    return ty;
                }
                if self.scope.has_global_ident(&name) {
                    self.record_value_use_variants(&name);
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
                // Direct operands that exempt an Add-mix: nested arithmetic
                // (only the topmost mix warns), literals, explicit
                // constructions, and ternaries. Ternary exemption is
                // game-verified (RemoteBuild probe 2026-08-18): `ix + (c ?
                // u : len)` compiles with NO warning even though the
                // ternary unifies to uint — the relational flavor DOES
                // pierce ternaries (`i < (c ? u : len)` warns).
                let operand_binary_or_lit = |e: &Expr| {
                    matches!(
                        e.kind,
                        ExprKind::Binary { .. }
                            | ExprKind::IntLit(_)
                            | ExprKind::HexLit(_)
                            | ExprKind::TypeConstruct { .. }
                            | ExprKind::Ternary { .. }
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
                        // Game probe (signmap, 2026-08-18): explicit
                        // conversions exempt the mix — `u < int(E::LAST)`
                        // and `u < int(3)` are silent; only var-vs-var
                        // mixes warn.
                        if !subtree_has_int_literal(lhs)
                            && !subtree_has_int_literal(rhs)
                            && !subtree_has_conversion(lhs)
                            && !subtree_has_conversion(rhs)
                        {
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
                if let Some(type_name) = Self::base_type_name(&obj_ty) {
                    // Field access through a typed receiver: counts as a
                    // use of `Type::member` across the chain.
                    let member_name = member.text(self.source).to_string();
                    self.record_member_use_from_type(&type_name, &member_name);
                }
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
                let target = self.resolve_type_expr(target_type);
                let _ = self.expr_type(inner);
                // GH #51: `cast<T>(x)` yields `T@`, so member access on the
                // result checks T's own member set plus its base chain
                // (`cf.Visible` → UndefinedMember when no base carries it).
                // Unresolvable targets stay silent rather than cascading.
                if target.is_error() || matches!(target, TypeRepr::Void) {
                    return TypeRepr::Error(String::new());
                }
                match target {
                    TypeRepr::Handle(_) => target,
                    TypeRepr::Const(pointee) => {
                        TypeRepr::Handle(Box::new(TypeRepr::Const(pointee)))
                    }
                    other => TypeRepr::Handle(Box::new(other)),
                }
            }
            ExprKind::TypeConstruct { target_type, args } => {
                let target = self.resolve_type_expr(target_type);
                if let Some(qual) = self.funcdef_name_of(&target) {
                    let arg_refs: Vec<&Expr> = args.iter().collect();
                    return self.check_funcdef_construct(&qual, &arg_refs, expr.span);
                }
                let mut arg_tys = Vec::with_capacity(args.len());
                for a in args {
                    arg_tys.push(self.expr_type(a));
                }
                self.check_math_value_ctor(&target, &arg_tys, expr.span);
                if target.is_error() {
                    TypeRepr::Error(String::new())
                } else {
                    target
                }
            }
            ExprKind::ArrayInit(items) => {
                for i in items {
                    let _ = self.expr_type(i);
                }
                TypeRepr::Error(String::new())
            }
            ExprKind::Assign { lhs, op, rhs } => {
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
                // B071, the fifth position. Plain `=` only: a compound
                // assign is `a = a OP b`, whose right side has already
                // decayed to `int` through `Binary`, and no probe covers
                // what the engine then does with an enum slot.
                if matches!(op, AssignOp::Assign) {
                    self.check_enum_conversion(&lhs_ty, rhs, &rhs_ty);
                }
                if matches!(lhs_ty, TypeRepr::Const(_)) {
                    self.diagnostics.push(TypeDiagnostic {
                        span: lhs.span,
                        kind: TypeDiagnosticKind::ConstViolation {
                            detail: "cannot assign to const value".to_string(),
                        },
                    });
                }
                // Value-assign through a const handle (`auto ty =
                // Reflection::GetType(…)` → `Handle(Const(…))`; then
                // `ty = …`): the game rejects the implicit write-through
                // with `Reference is read-only`. `@ty =` rebinds the slot
                // and is legal (checked in the HandleAssign arm).
                if matches!(&lhs_ty, TypeRepr::Handle(inner) if matches!(&**inner, TypeRepr::Const(_)))
                {
                    self.diagnostics.push(TypeDiagnostic {
                        span: lhs.span,
                        kind: TypeDiagnosticKind::ReadOnlyReference,
                    });
                }
                // Get-only virtual property write (`iso4.tx = …` /
                // `loc.tx += …`): the game rejects with
                // `The property has no set accessor`.
                self.check_readonly_prop_assign(lhs, AccessPosition::of_assign_op(*op));
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
                self.check_funcdef_binding(&lhs_ty, rhs, &rhs_ty);
                TypeRepr::Error(String::new())
            }
            ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                let _ = self.expr_type(condition);
                let then_ty = self.expr_type(then_expr);
                let else_ty = self.expr_type(else_expr);
                // GH #52 (repro 2): the game unifies both branches to one
                // type via implicit conversion and rejects the ternary when
                // it can't. The error span starts at the condition — the
                // game points there (probe: 13:19 on the fixture).
                match ternary_unify(&then_ty, &else_ty) {
                    TernaryUnify::Rejected => {
                        self.diagnostics.push(TypeDiagnostic {
                            span: expr.span,
                            kind: TypeDiagnosticKind::TernaryNoCommonType,
                        });
                        TypeRepr::Error(String::new())
                    }
                    TernaryUnify::Unified(t) => t,
                    TernaryUnify::Unknown => TypeRepr::Error(String::new()),
                }
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
                    self.record_use(&qual);
                    TypeRepr::Named(qual)
                } else {
                    // Qualified VALUE reference (`Ns::g_Var`, `Color::Red`,
                    // `Enum::Value`) — record the value name (plus accessor
                    // variants). Try the written form first, then with the
                    // enclosing namespace prefixes restored: inside
                    // `namespace Repro`, a written `StatusKind::B` refers
                    // to `Repro::StatusKind::B` in the pooled table.
                    for depth in (0..=self.namespace_stack.len()).rev() {
                        let candidate = if depth == 0 {
                            qual.clone()
                        } else {
                            format!("{}::{}", self.namespace_stack[..depth].join("::"), qual)
                        };
                        if self.scope.has_global_ident(&candidate) {
                            self.record_value_use_variants(&candidate);
                            break;
                        }
                    }
                    self.enum_member_type(&qual, expr.span)
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

/// GH #27 / #28: known API removals and relocations, verified against the
/// live game (OP 1.29.5). Keyed by full qualified name; value is the
/// replacement to hint. `Draw::*` wholesale moved to `Display::*` /
/// `UI::*` — the namespace is absent from the 1.29.5 typedb entirely.
fn removed_api_hint(qual: &str) -> Option<&'static str> {
    match qual {
        "Draw::GetWidth" => Some("Display::GetWidth"),
        "Draw::GetHeight" => Some("Display::GetHeight"),
        "Draw::MeasureString" => Some("UI::MeasureString"),
        // GH #18: `string::Join`/`string::Repeat` moved to `Text::` in
        // 1.29.14 and the whole `string` namespace left the dump — the
        // game hard-fails the old calls (verified live 2026-08-31). On a
        // pre-1.29.14 typedb the namespace exists, so trigger (b) never
        // consults this table there and the old calls stay legal.
        "string::Join" => Some("Text::Join"),
        "string::Repeat" => Some("Text::Repeat"),
        _ => None,
    }
}

/// Levenshtein distance capped at `max` — small helper for near-name
/// candidate hints. Returns `max + 1` when the strings are further apart.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0usize; m + 1];
    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m]
}

/// True if the expression subtree contains an integer literal anywhere
/// (`IntLit` / `HexLit`). Used by the `Signed/Unsigned mismatch` rule:
/// the game compiler stays silent whenever either compared subtree is
/// "literal-tainted" (RemoteBuild probe 2026-08-17: `n > 0`, `0 < n`,
/// `arr.Length > 0` all silent).
/// GH #47: game-compiler signature render for `No matching signatures to
/// 'Ns::Fn(const string, string, const bool)'` — qualified callee plus one
/// display type per argument; literals render const-qualified (the game
/// derives constness from the expression form).
fn render_no_matching_sig(qualified_name: &str, args: &[RenderedArg]) -> String {
    let rendered = args
        .iter()
        .map(RenderedArg::render)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{qualified_name}({rendered})")
}

/// One argument, already resolved into the only two facts the
/// `No matching signatures` renderer needs (B108).
///
/// It used to be handed `arg_tys` and `arg_literals` — two parallel slices —
/// and nothing else. That is why a CALLABLE printed as `void`: an argument's
/// TYPE is its return type, and by the time the renderer ran the callable had
/// already been flattened to it. The fix is not a third parallel slice; it is
/// to resolve each argument where its EXPRESSION is still in scope and hand
/// the renderer one value per argument.
struct RenderedArg {
    /// The argument's display text: its SIGNATURE when it resolves to
    /// exactly one function, otherwise its type.
    text: String,
    /// Literals render const-qualified — the game derives constness from the
    /// expression form (GH #47).
    is_literal: bool,
}

impl RenderedArg {
    fn render(&self) -> String {
        if self.is_literal {
            format!("const {}", self.text)
        } else {
            self.text.clone()
        }
    }
}

/// True when the argument expression is a literal, which the game renders
/// const-qualified. One definition: this test was copy-pasted at each site
/// that built an `arg_literals` slice.
fn arg_is_const_literal(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::BoolLit(_)
            | ExprKind::IntLit(_)
            | ExprKind::HexLit(_)
            | ExprKind::FloatLit(_)
            | ExprKind::StringLit
    )
}

/// True when the subtree contains an explicit type conversion
/// (`int(x)` TypeConstruct). Game probe (2026-08-18): a conversion anywhere
/// in a relational operand exempts the Signed/Unsigned warning.
fn subtree_has_conversion(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::TypeConstruct { .. } | ExprKind::Cast { .. } => true,
        ExprKind::Binary { lhs, rhs, .. } => {
            subtree_has_conversion(lhs) || subtree_has_conversion(rhs)
        }
        ExprKind::Unary { expr, .. } | ExprKind::Postfix { expr, .. } => {
            subtree_has_conversion(expr)
        }
        ExprKind::Call { callee, args } => {
            subtree_has_conversion(callee) || args.iter().any(|a| subtree_has_conversion(&a.value))
        }
        ExprKind::Member { object, .. } => subtree_has_conversion(object),
        ExprKind::Index { object, index } => {
            subtree_has_conversion(object) || subtree_has_conversion(index)
        }
        ExprKind::Is { expr, .. } => subtree_has_conversion(expr),
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            subtree_has_conversion(condition)
                || subtree_has_conversion(then_expr)
                || subtree_has_conversion(else_expr)
        }
        _ => false,
    }
}

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

/// GH #49: AngelScript math value types exposed by the game (Nadeo/TM
/// add-on). They are distinct engine value types with NO implicit
/// conversions between each other — the compiler rejects
/// `int3 size = nat3_value;` and `int3(nat3_value)` outright. Bare names
/// only; these types are never namespace-qualified in source.
fn is_math_value_type(name: &str) -> bool {
    matches!(
        name,
        "nat2" | "nat3" | "int2" | "int3" | "vec2" | "vec3" | "vec4" | "iso3" | "iso4" | "quat"
    )
}

/// GH #52 (repro 2): outcome of unifying a ternary's two branch types the
/// way the game's implicit conversion does.
enum TernaryUnify {
    /// Branches unify; carries the result type of the ternary.
    Unified(TypeRepr),
    /// The game rejects the ternary outright (hard ERR).
    Rejected,
    /// Pairs we can't classify confidently (enum/int, handle pairs,
    /// string vs other named types, any errored operand) — stay silent.
    Unknown,
}

/// GH #52 (repro 2): game-verified ternary branch unification (RemoteBuild
/// probes 2026-08-18). Verified verdicts:
///   unified  — same type; any two numeric primitives (int×uint promotes
///              to uint: `auto t = c ? i : u; t < i` warns like `i < (c ?
///              i : u)`); null + handle
///   rejected — distinct primitives that aren't both numeric (`c ? 1 :
///              true`, `c ? "" : 5`, `c ? "" : i`); string vs wstring;
///              string vs math value types (`c ? "" : vec3(1,2,3)`);
///              distinct math value types (`c ? nat3v : int3v`, same set
///              as the GH #49 assignment form); numeric vs null
///              (`c ? 1 : null`)
fn ternary_unify(a: &TypeRepr, b: &TypeRepr) -> TernaryUnify {
    let a = a.unwrap_const();
    let b = b.unwrap_const();
    if a.is_error() || b.is_error() {
        return TernaryUnify::Unknown;
    }
    if a == b {
        return TernaryUnify::Unified(a.clone());
    }
    match (a, b) {
        (TypeRepr::Null, TypeRepr::Handle(h)) | (TypeRepr::Handle(h), TypeRepr::Null) => {
            TernaryUnify::Unified(TypeRepr::Handle(h.clone()))
        }
        (TypeRepr::Primitive(pa), TypeRepr::Primitive(pb))
            if is_numeric_primitive(pa) && is_numeric_primitive(pb) =>
        {
            TernaryUnify::Unified(TypeRepr::Primitive(numeric_promote(*pa, *pb)))
        }
        (TypeRepr::Primitive(p), TypeRepr::Null) | (TypeRepr::Null, TypeRepr::Primitive(p))
            if is_numeric_primitive(p) =>
        {
            TernaryUnify::Rejected
        }
        (TypeRepr::Primitive(_), TypeRepr::Primitive(_)) => TernaryUnify::Rejected,
        (TypeRepr::Primitive(PrimitiveType::String), TypeRepr::Named(n))
        | (TypeRepr::Named(n), TypeRepr::Primitive(PrimitiveType::String))
            if n == "wstring" || is_math_value_type(n) =>
        {
            TernaryUnify::Rejected
        }
        (TypeRepr::Named(na), TypeRepr::Named(nb))
            if is_math_value_type(na) && is_math_value_type(nb) =>
        {
            TernaryUnify::Rejected
        }
        _ => TernaryUnify::Unknown,
    }
}

/// Wider-of-the-two for a numeric-mix ternary result. Small widths rank
/// below their wider peers, floats above every integer; the promoted type
/// drives downstream init checks (float/double vs int keeps the game's
/// truncation-warning parity).
fn numeric_promote(a: PrimitiveType, b: PrimitiveType) -> PrimitiveType {
    fn rank(p: PrimitiveType) -> u8 {
        match p {
            PrimitiveType::Int8 => 0,
            PrimitiveType::Uint8 => 1,
            PrimitiveType::Int16 => 2,
            PrimitiveType::Uint16 => 3,
            PrimitiveType::Int => 4,
            PrimitiveType::Uint => 5,
            PrimitiveType::Int64 => 6,
            PrimitiveType::Uint64 => 7,
            PrimitiveType::Float => 8,
            PrimitiveType::Double => 9,
            _ => 10,
        }
    }
    if rank(a) >= rank(b) {
        a
    } else {
        b
    }
}

/// GH #49: the int/nat vector families. Returns the vector dimension for
/// members (nat2/int2 → 2, nat3/int3 → 3); non-members yield `None`.
/// Same-dimension nat↔int pairs have no cross-type constructor (game
/// ground truth); cross-dimension and cross-family (vec/quat/iso) do have
/// legal conversions and are NOT flagged.
fn int_nat_family(name: &str) -> Option<u8> {
    match name {
        "nat2" | "int2" => Some(2),
        "nat3" | "int3" => Some(3),
        _ => None,
    }
}

fn render_funcdef_sig(fd: &FuncdefSig) -> String {
    format!(
        "{} ({})",
        if fd.return_type.is_empty() {
            "?"
        } else {
            fd.return_type.as_str()
        },
        fd.param_types.join(", ")
    )
}

fn render_func_or_type(sigs: &[OverloadSig], arg_ty: &TypeRepr) -> String {
    if let Some(sig) = sigs.first() {
        format!("{} ({})", sig.return_type, sig.param_types.join(", "))
    } else {
        arg_ty.display()
    }
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

    /// Like `check`, but the pooled workspace table is marked as the
    /// COMPLETE dependency closure — the authority the GH #58 workspace
    /// `No matching symbol` rule requires (B013 decision 3).
    fn check_closed_workspace(source: &str) -> Vec<TypeDiagnostic> {
        let tokens = tokenize_filtered(source);
        let mut parser = Parser::new(&tokens, source);
        let file = parser.parse_file();
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        let syms = SymbolTable::extract_symbols(fid, source, &file);
        ws.set_file_symbols(fid, syms);
        ws.mark_dependency_closure_complete();
        let scope = GlobalScope::new(&ws, None);
        let mut checker = Checker::new(source, &scope);
        checker.check_file(&file);
        checker.diagnostics
    }

    /// Qualified names (and hints) the `No matching symbol` rule flagged.
    fn no_matching_symbols(diags: &[TypeDiagnostic]) -> Vec<(String, Option<String>)> {
        diags
            .iter()
            .filter_map(|d| match &d.kind {
                TypeDiagnosticKind::NoMatchingSymbol { qualified, hint } => Some((
                    qualified.clone(),
                    hint.as_ref().map(|h| h.name().to_string()),
                )),
                _ => None,
            })
            .collect()
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

    /// Like `check_with_typedb`, but against a recorded snapshot from
    /// `tests/fixtures/typedb-versions/<version>/` (GH #18).
    fn check_with_versioned_typedb(source: &str, version: &str) -> Vec<TypeDiagnostic> {
        use crate::typedb::TypeIndex;
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb-versions")
            .join(version);
        let idx = TypeIndex::load(
            &dir.join("OpenplanetCore.json"),
            &dir.join("OpenplanetNext.json"),
        )
        .expect("versioned typedb snapshot must load");
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

    /// GH #18: `string::Join` / `string::Repeat` moved to `Text::` and the
    /// `string` namespace vanished from the 1.29.14 dump — the same shape
    /// as `Draw` on 1.29.5, so trigger (b) must fire with the relocation
    /// hint. The game hard-fails these calls on 1.29.14 (verified
    /// 2026-08-31).
    #[test]
    fn string_join_and_repeat_relocations_fire_on_1_29_14() {
        for (call, expect_hint) in [
            (r#"void F() { string::Join({"a"}, ","); }"#, "Text::Join"),
            (r#"void F() { string::Repeat("a", 3); }"#, "Text::Repeat"),
        ] {
            let diags = check_with_versioned_typedb(call, "1.29.14");
            let hit = diags.iter().find_map(|d| match &d.kind {
                TypeDiagnosticKind::NoMatchingSymbol { qualified, hint } => Some((
                    qualified.clone(),
                    hint.as_ref().map(|h| h.name().to_string()),
                )),
                _ => None,
            });
            let (qualified, hint) = hit
                .unwrap_or_else(|| panic!("expected NoMatchingSymbol for {call:?}, got {diags:?}"));
            assert!(qualified.starts_with("string::"), "{qualified}");
            assert_eq!(hint.as_deref(), Some(expect_hint), "for {call:?}");
        }
    }

    /// Negative control: on 1.29.5 the `string::` pair exists and the
    /// `Text::` pair does not — the exact version straddle. The old calls
    /// must stay silent there (B00x parity), and the NEW calls must fire
    /// via trigger (a) (`Text` is a known namespace on 1.29.5 too).
    #[test]
    fn string_join_silent_and_text_join_flagged_on_1_29_5() {
        for legal in [
            r#"void F() { string::Join({"a"}, ","); }"#,
            r#"void F() { string::Repeat("a", 3); }"#,
        ] {
            let diags = check_with_versioned_typedb(legal, "1.29.5");
            assert!(
                !diags
                    .iter()
                    .any(|d| matches!(&d.kind, TypeDiagnosticKind::NoMatchingSymbol { .. })),
                "legal on 1.29.5, got {diags:?}"
            );
        }
        let diags =
            check_with_versioned_typedb(r#"void F() { Text::Join({"a"}, ","); }"#, "1.29.5");
        assert!(
            diags.iter().any(|d| matches!(
                &d.kind,
                TypeDiagnosticKind::NoMatchingSymbol { qualified, .. } if qualified == "Text::Join"
            )),
            "Text::Join is missing on 1.29.5, got {diags:?}"
        );
    }

    /// And the migrated calls are legal on 1.29.14.
    #[test]
    fn text_join_and_repeat_silent_on_1_29_14() {
        for legal in [
            r#"void F() { Text::Join({"a"}, ","); }"#,
            r#"void F() { Text::Repeat("a", 3); }"#,
        ] {
            let diags = check_with_versioned_typedb(legal, "1.29.14");
            assert!(
                !diags
                    .iter()
                    .any(|d| matches!(&d.kind, TypeDiagnosticKind::NoMatchingSymbol { .. })),
                "legal on 1.29.14, got {diags:?}"
            );
        }
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
        // Policy FLIPPED 2026-08-23 (game parity, spike-live-add-kinematic-ao):
        // the game compiler rejects unknown members on script classes, so a
        // missing member on a PARENTLESS workspace class now emits
        // UndefinedMember. Classes WITH bases stay silent (unresolved
        // parents / mixins could carry the member — FP guard; cross-file
        // bases resolve through the GlobalScope walk below this gate).
        let diags = check("class C { int x; void f() { int y = this.bogus; } }");
        let undef_member: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { .. }))
            .collect();
        assert_eq!(
            undef_member.len(),
            1,
            "expected UndefinedMember on parentless class, got {:?}",
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

    /// **Corrected by B100**, which is why the name changed: an overloaded
    /// call's arity is checked against the SET's range, not suppressed.
    ///
    /// This was `arg_count_overloaded_suppressed` and asserted that
    /// `f()` against `f(int)` / `f(int, int)` reports nothing. That
    /// suppression dates from iter 19/22, before overload resolution existed,
    /// when the only available check was the single-overload `check_arg_count`
    /// and running it per candidate would have invented errors on legal calls.
    /// The resolver replaced it: `check_arity_against_ranges` fires only when
    /// the count is outside EVERY candidate's range, so it cannot do that.
    ///
    /// The repo already held the corrected rule and tested it —
    /// `inherited_overload_exceeding_all_arity_still_diagnoses` (GH #34 review
    /// Arm B): "the augmented overload set must not silently swallow a
    /// genuinely-wrong call the pre-#34 single-overload path caught". Two
    /// tests in this file asserted opposite things about one rule, and this
    /// one survived only because a workspace FREE-function set never reached
    /// the resolver at all — B100's gap. It was passing for a reason nobody
    /// had identified.
    ///
    /// Both halves are kept, because the suppression was guarding something
    /// real: a call INSIDE some candidate's range must still be silent, and
    /// the reported range must be the SET's (1..2), not one candidate's.
    #[test]
    fn arg_count_overloaded_uses_the_sets_arity_range() {
        let in_range =
            check("void f(int a) {} void f(int a, int b) {} void main() { f(1); f(1, 2); }");
        assert!(
            !in_range
                .iter()
                .any(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. })),
            "a call matching ANY candidate must stay silent — this is what the \
             old suppression was really protecting: {in_range:?}"
        );

        let diags = check("void f(int a) {} void f(int a, int b) {} void main() { f(); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "zero arguments is outside every candidate's range: {diags:?}"
        );
        assert_eq!(
            bad[0].kind,
            TypeDiagnosticKind::ArgCountMismatch {
                function_name: "f".into(),
                expected_min: 1,
                expected_max: 2,
                got: 0,
            },
            "and the range must span the SET, not one candidate: {diags:?}"
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

    /// GH #54: `mat4` registers `mat4(const mat3&in)`, so AngelScript
    /// converts the argument by constructing the parameter's value type.
    /// The relation is symmetric (`mat3(const mat4&in)` exists too).
    #[test]
    fn external_arg_single_arg_ctor_conversion_is_silent() {
        let diags = check_with_typedb(
            r#"
            void f(mat3 rot, mat4 world) {
                mat4 a = mat4::Inverse(rot);
                mat3 b = mat3::Inverse(world);
                quat q = Math::Slerp(quat(rot), quat(world), 0.5);
            }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "single-arg ctor conversions must not fire ArgTypeMismatch, got {:?}",
            diags
        );
    }

    /// GH #54 negative: there is no `mat4(const vec3&in)` constructor, so
    /// the mismatch must still fire. Guards the fix against blanket
    /// silencing of Named/Named argument pairs.
    #[test]
    fn external_arg_without_ctor_conversion_still_fires() {
        let diags = check_with_typedb(r#"void f(vec3 v) { mat4 a = mat4::Inverse(v); }"#);
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 ArgTypeMismatch for vec3 -> mat4 (no such ctor), got {:?}",
            diags
        );
        assert_eq!(
            bad[0].kind,
            TypeDiagnosticKind::ArgTypeMismatch {
                function_name: "Inverse".into(),
                param_index: 0,
                expected: "mat4".into(),
                got: "vec3".into(),
            }
        );
    }

    /// GH #54: an exact argument type is matched directly, not through the
    /// constructor path — the copy constructor is excluded from the typedb
    /// conversion graph precisely so the two cannot be confused (see
    /// `external_ctor_converts_follows_typedb_construct_behaviors`), and
    /// the relaxation runs only *after* arity has already picked a unique
    /// overload, so it can never re-point overload resolution.
    #[test]
    fn external_arg_exact_match_needs_no_ctor_conversion() {
        let diags = check_with_typedb(
            r#"
            void f(mat4 world, vec3 a, vec3 b) {
                mat4 inv = mat4::Inverse(world);
                float d = Math::Distance(a, b);
            }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "exact-match args must stay silent, got {:?}",
            diags
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

    // ── GH #52 (repro 2): ternary branch unification ─────────────────────

    #[test]
    fn ternary_string_vs_wstring_member_flags() {
        // Game-compiler ground truth (RemoteBuild probe 2026-08-18):
        // `c ? "" : map.MapInfo.FileName` → ERR "Can't find unambiguous
        // implicit conversion to make both expressions have the same type",
        // error column at the condition.
        let diags = check_with_typedb(
            r#"void F(CGameCtnChallenge@ map, bool c) {
                auto s = c ? "" : map.MapInfo.FileName;
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::TernaryNoCommonType))
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "expected 1 TernaryNoCommonType for string vs wstring, got {:?}",
            diags
        );
    }

    #[test]
    fn ternary_wstring_vs_string_literal_also_flags() {
        // Operand order must not matter — the wstring branch can be first.
        let diags = check_with_typedb(
            r#"void F(CGameCtnChallenge@ map, bool c) {
                auto s = c ? map.MapInfo.FileName : "";
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::TernaryNoCommonType))
            .collect();
        assert_eq!(bad.len(), 1, "got {:?}", diags);
    }

    #[test]
    fn ternary_bool_vs_int_flags() {
        // Game-verified (probe 2026-08-18): `int x = c ? 1 : true;` → ERR,
        // same wording. Numeric×bool never implicitly converts.
        let diags = check("void F(bool c) { int x = c ? 1 : true; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::TernaryNoCommonType))
            .collect();
        assert_eq!(bad.len(), 1, "got {:?}", diags);
    }

    #[test]
    fn ternary_string_vs_int_flags() {
        // Game-verified (probe 2026-08-18): `string s = c ? "" : i;` → ERR,
        // same wording. Only numeric×numeric pairs unify.
        let diags = check("void F(bool c, int i) { string s = c ? \"\" : i; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::TernaryNoCommonType))
            .collect();
        assert_eq!(bad.len(), 1, "got {:?}", diags);
    }

    #[test]
    fn ternary_nat3_vs_int3_flags() {
        // Game-verified (probe 2026-08-18): `c ? map.Size : int3(1,2,3)` →
        // ERR, same wording — same math-value-type set as the GH #49
        // assignment form.
        let diags = check_with_typedb(
            r#"void F(CGameCtnChallenge@ map, bool c) {
                auto v = c ? map.Size : int3(1, 2, 3);
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::TernaryNoCommonType))
            .collect();
        assert_eq!(bad.len(), 1, "got {:?}", diags);
    }

    #[test]
    fn ternary_legal_forms_stay_silent() {
        // Game-verified CLEAN (probe 2026-08-18): same-type (string, wstring,
        // bool), numeric mixes (literal and vars, int/uint), null + handle.
        let diags = check_with_typedb(
            r#"class C {}
            void F(CGameCtnChallenge@ map, bool c, C@ h, int i, uint u, float f) {
                auto a = c ? "" : string(map.MapInfo.FileName);
                auto b = c ? map.MapInfo.Name : map.MapInfo.FileName;
                auto d = c ? true : false;
                auto e = c ? 1 : 2.5;
                auto g = c ? i : u;
                auto k = c ? i : f;
                auto m = c ? null : h;
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::TernaryNoCommonType))
            .collect();
        assert!(
            bad.is_empty(),
            "game-legal ternaries must stay silent, got {:?}",
            diags
        );
    }

    #[test]
    fn ternary_unified_type_feeds_init_conversion() {
        // Game-verified (probe 2026-08-18): `int i = c ? 1.5 : 2.5;` → WARN
        // "Float value truncated in implicit conversion to integer" — the
        // unified (float) result flows into the init-conversion check with
        // the non-literal message variant.
        let diags = check("void F(bool c) { int i = c ? 1.5 : 2.5; }");
        assert_eq!(
            diags[0].kind,
            TypeDiagnosticKind::FloatTruncation { literal: false }
        );
    }

    #[test]
    fn ternary_same_type_int_init_stays_silent() {
        // Game-verified CLEAN: `int j = c ? 1 : 2;` unifies to int.
        let diags = check("void F(bool c) { int j = c ? 1 : 2; }");
        assert!(diags.is_empty(), "got {:?}", diags);
    }

    #[test]
    fn ternary_string_vs_math_value_flags() {
        // Game-verified ERR (RemoteBuild probe 2026-08-18): `c ? "" :
        // vec3(1,2,3)` — same wording. String never converts to a math
        // value type (and the pair is not string-vs-wstring).
        let diags = check_with_typedb("void F(bool c) { auto z = c ? \"\" : vec3(1, 2, 3); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::TernaryNoCommonType))
            .collect();
        assert_eq!(bad.len(), 1, "got {:?}", diags);
    }

    #[test]
    fn ternary_numeric_vs_null_flags() {
        // Game-verified ERR (RemoteBuild probe 2026-08-18): `c ? 1 : null`
        // — null only unifies with handles.
        let diags = check("void F(bool c) { auto n = c ? 1 : null; }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::TernaryNoCommonType))
            .collect();
        assert_eq!(bad.len(), 1, "got {:?}", diags);
    }

    #[test]
    fn ternary_unknown_pairs_stay_silent() {
        // Unverified pairs (enum vs int, handle vs handle, string vs other
        // named types — `text` may carry an implicit conv to string) are
        // conservatively silent.
        let diags = check(
            r#"enum E { A }
            class C {}
            void F(bool c, E e, C@ a, C@ b, C v) {
                auto x = c ? E::A : 5;
                auto y = c ? a : b;
                auto z = c ? "" : v;
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::TernaryNoCommonType))
            .collect();
        assert!(bad.is_empty(), "got {:?}", diags);
    }

    #[test]
    fn add_sign_mix_through_ternary_operand_stays_silent() {
        // Game-verified CLEAN (RemoteBuild probe 2026-08-18): `ix + (c ? u
        // : len)` and `(c ? u : len) + n` produce NO Signed/Unsigned
        // warning — the `+` flavor only fires on plain direct operands.
        // Regression guard: the ternary's real unified type (uint) must
        // not start feeding the Add-mix rule.
        let diags = check(
            r#"void F(bool c, int ix, int n, uint i, uint len) {
                auto a = ix + (i == 0 ? i : len);
                auto b = (c ? i : len) + n;
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::SignedUnsignedMismatch))
            .collect();
        assert!(bad.is_empty(), "got {:?}", diags);
    }

    #[test]
    fn relational_sign_mix_through_ternary_operand_warns() {
        // Game-verified WARN (RemoteBuild probe 2026-08-18): `i < (c ? u :
        // len)` and `(c ? u : len) > i` — unlike the `+` flavor, the
        // relational warning pierces ternary operands.
        let diags = check(
            r#"void F(bool c, int i, uint u, uint len) {
                bool b1 = i < (c ? u : len);
                bool b2 = (c ? u : len) > i;
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::SignedUnsignedMismatch))
            .collect();
        assert_eq!(bad.len(), 2, "got {:?}", diags);
    }

    #[test]
    fn relational_sign_mix_through_mixed_sign_ternary_warns() {
        // Game-verified WARN ×3 (RemoteBuild probe 2026-08-18): a mixed
        // int×uint ternary unifies to uint (int×uint promotes to the
        // unsigned side), so the relational pierce warns — both inline and
        // through an `auto` that captures the unified type.
        let diags = check(
            r#"void F(bool c, int i, uint u) {
                bool w1 = i < (c ? i : u);
                bool w2 = (c ? u : i) > i;
                auto t = c ? i : u;
                bool w3 = t < i;
            }"#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::SignedUnsignedMismatch))
            .collect();
        assert_eq!(bad.len(), 3, "got {:?}", diags);
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

    // ── B059: Openplanet-injected engine methods ───────────────────────────
    //
    // Openplanet registers methods on engine classes that the Nadeo dump
    // never lists. B006 trusts a non-empty Nadeo member list, so the checker
    // fired `UndefinedMember` on calls the game compiles. `MwAddRef` /
    // `MwRelease` (GH #21, above) were the first case; `GetCurrentEntityID`
    // and `GetManialinkPages` are the other two, across six explicit
    // registrations.
    //
    // The authority for the table these tests exercise is
    // `docs/probes/openplanet-injected-methods.md` — recovered from
    // Openplanet.dll, NOT from the old `convert_nadeo.ts`, which listed the
    // wrong return type for `GetManialinkPages` and missed `CGameNetwork`.
    //
    // Game ground truth — RemoteBuild probe, Trackmania + Openplanet 1.29.5,
    // 2026-08-30: see the fixture gate test
    // `check_command_issue_repro_b059_injected_methods_silences_only_the_legal_half`
    // in `tests/cli_check_tests.rs` for the verbatim verdicts, including the
    // REJECT lines that name each method's real return type.

    /// `UndefinedMember` diagnostics naming `member`.
    fn undefined_members_named<'d>(
        diags: &'d [TypeDiagnostic],
        member: &str,
    ) -> Vec<&'d TypeDiagnostic> {
        diags
            .iter()
            .filter(|d| {
                matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { member: m, .. }
                    if m == member)
            })
            .collect()
    }

    /// `ArgCountMismatch` diagnostics naming `function_name`.
    fn arg_count_mismatches<'d>(
        diags: &'d [TypeDiagnostic],
        name: &str,
    ) -> Vec<&'d TypeDiagnostic> {
        diags
            .iter()
            .filter(|d| {
                matches!(&d.kind, TypeDiagnosticKind::ArgCountMismatch { function_name, .. }
                    if function_name == name)
            })
            .collect()
    }

    /// B059: `GetCurrentEntityID` is injected on `CGamePlayer` itself, so a
    /// receiver of the declaring type must be silent.
    #[test]
    fn injected_entity_id_on_declaring_type_stays_silent() {
        let diags = check_with_typedb(
            r#"
            void Use(CGamePlayer@ gp) {
                auto id = gp.GetCurrentEntityID();
            }
            "#,
        );
        assert!(
            undefined_members_named(&diags, "GetCurrentEntityID").is_empty(),
            "GetCurrentEntityID on its declaring type must be silent, got {:?}",
            diags
        );
    }

    /// B081: a workspace class shadowing an engine one gets NONE of that engine
    /// class's injected methods, at the call path and the member-access path
    /// alike.
    ///
    /// HONEST STATUS: this passes before B081 as well as after, and it is a
    /// characterization test rather than a red/green. It is here because of
    /// WHY it passed before. Both `injected_method` call sites were correct
    /// only because an `is_external_type` guard fifteen-odd lines earlier —
    /// written for an unrelated purpose, "only emit when the type is a known
    /// external type" — happened to dominate them. Nothing tied the guard to
    /// the use, no test named the property, and a reader at the call site
    /// could not see it. B063 shipped exactly that defect on the completion
    /// side, where `completion.rs` has carried the mirror of this test ever
    /// since; the checker side had the behaviour and no witness.
    ///
    /// B081 makes it structural — `injected_method` takes an
    /// `ExternalTypeName`, which only the shadow-aware mint produces — so what
    /// this test now pins is a guarantee rather than a coincidence of control
    /// flow. The `compile_fail` doctests on `ExternalTypeName` are the
    /// red/green for the guarantee itself.
    #[test]
    fn b081_a_workspace_class_shadowing_an_engine_one_gets_no_injected_methods() {
        let diags = check_with_typedb(
            r#"
            class CSmPlayer { int ownField; }
            void Use(CSmPlayer@ p) {
                p.MwAddRef();
                p.GetCurrentEntityID();
                print("" + p.ownField);
            }
            "#,
        );
        let undefined: Vec<_> = diags
            .iter()
            .filter_map(|d| match &d.kind {
                TypeDiagnosticKind::UndefinedMember { member, .. } => Some(member.as_str()),
                _ => None,
            })
            .collect();
        for name in ["MwAddRef", "GetCurrentEntityID"] {
            assert!(
                undefined.contains(&name),
                "the workspace CSmPlayer has no `{name}`: the engine class's \
                 injected methods must not leak onto a shadowing declaration, \
                 got {diags:?}"
            );
        }
        // The control that makes the two assertions above mean something: the
        // shadowing class's OWN member still resolves, so this is a shadow
        // rule and not the receiver failing to type at all.
        assert!(
            !undefined.contains(&"ownField"),
            "the shadowing class's own field must still resolve: {diags:?}"
        );
    }

    /// B059 (the reported case): `CSmPlayer` is separately named in the
    /// binary's explicit registration list. This is the call shape in
    /// tm-dips-plus-plus `src/PlayerState.as:121`.
    #[test]
    fn injected_entity_id_on_subclass_stays_silent() {
        let diags = check_with_typedb(
            r#"
            void Use(CSmPlayer@ player) {
                auto entId = player.GetCurrentEntityID();
            }
            "#,
        );
        assert!(
            undefined_members_named(&diags, "GetCurrentEntityID").is_empty(),
            "GetCurrentEntityID on CSmPlayer must be silent, got {:?}",
            diags
        );
    }

    /// B059: the injected signature takes no arguments, so a call with
    /// arguments must still be diagnosed. Before the injected-method table
    /// this errored as `UndefinedMember`; the table knows the arity, so it
    /// errors as `ArgCountMismatch` — but it must not go silent.
    #[test]
    fn injected_entity_id_wrong_arity_still_errors() {
        let diags = check_with_typedb(
            r#"
            void Use(CSmPlayer@ player) {
                auto entId = player.GetCurrentEntityID(1);
            }
            "#,
        );
        let hits = arg_count_mismatches(&diags, "GetCurrentEntityID");
        assert_eq!(
            hits.len(),
            1,
            "1-arg GetCurrentEntityID must fire ArgCountMismatch, got {:?}",
            diags
        );
        assert_eq!(
            hits[0].kind,
            TypeDiagnosticKind::ArgCountMismatch {
                function_name: "GetCurrentEntityID".into(),
                expected_min: 0,
                expected_max: 0,
                got: 1,
            }
        );
    }

    /// B059 negative: the injection is scoped to the `CGamePlayer` branch.
    /// A `CMwNod@` (the common ancestor) and an unrelated engine type must
    /// still report the member as undefined — widening this to every engine
    /// type would re-introduce the FP class the ticket exists to avoid.
    #[test]
    fn injected_entity_id_wrong_receiver_still_errors() {
        let diags = check_with_typedb(
            r#"
            void Use(CMwNod@ nod, CGameCtnChallenge@ map) {
                auto a = nod.GetCurrentEntityID();
                auto b = map.GetCurrentEntityID();
            }
            "#,
        );
        assert_eq!(
            undefined_members_named(&diags, "GetCurrentEntityID").len(),
            2,
            "GetCurrentEntityID off the CGamePlayer branch must still error, got {:?}",
            diags
        );
    }

    /// B059 / B006 fence: `CSmPlayer`'s member list is non-empty and stays
    /// trusted. A genuine typo — including one a character away from the
    /// injected name — must still be reported.
    #[test]
    fn injected_table_does_not_widen_b006_trust() {
        let diags = check_with_typedb(
            r#"
            void Use(CSmPlayer@ player, CGameCtnNetwork@ net) {
                auto a = player.GetCurrentEntityId();
                auto b = player.NoSuchMemberAtAll();
                auto c = net.GetManialinkPage();
            }
            "#,
        );
        for member in [
            "GetCurrentEntityId",
            "NoSuchMemberAtAll",
            "GetManialinkPage",
        ] {
            assert_eq!(
                undefined_members_named(&diags, member).len(),
                1,
                "`{member}` must still fire UndefinedMember, got {:?}",
                diags
            );
        }
    }

    /// B059 sibling: `GetManialinkPages` is registered on FOUR types
    /// (Openplanet.dll 0x180137cd3/…d43/…db3/…e23). tm-shadowless-chrono is
    /// the live caller.
    ///
    /// Note `CGameManiaPlanetNetwork` carries an EMPTY member list in the
    /// dump, so B006 would already silence anything on it — it is here for
    /// completeness, not as a gate. `CGameNetwork` (48 members),
    /// `CGameCtnNetwork` (11) and `CTrackManiaNetwork` (2) are trusted lists
    /// and are the real assertions.
    #[test]
    fn injected_manialink_pages_stays_silent() {
        let diags = check_with_typedb(
            r#"
            void Use(CGameNetwork@ gn, CGameCtnNetwork@ net,
                     CGameManiaPlanetNetwork@ mpn, CTrackManiaNetwork@ tmn) {
                auto a = gn.GetManialinkPages();
                auto b = net.GetManialinkPages();
                auto c = mpn.GetManialinkPages();
                auto d = tmn.GetManialinkPages();
            }
            "#,
        );
        assert!(
            undefined_members_named(&diags, "GetManialinkPages").is_empty(),
            "GetManialinkPages on all four registered types must be silent, got {:?}",
            diags
        );
    }

    /// B059, coordinator's binary-derived correction: `CGameNetwork` is the
    /// PARENT of `CGameCtnNetwork` and is registered in its own right. The
    /// ticket's original "`CGameCtnNetwork` and descendants" rule — and the
    /// stale `convert_nadeo.ts` it came from — would still fire here, so
    /// this test is what separates the two designs.
    #[test]
    fn injected_manialink_pages_covers_the_parent_registration() {
        let diags = check_with_typedb(
            r#"
            void Use(CGameNetwork@ gn) {
                auto pages = gn.GetManialinkPages();
            }
            "#,
        );
        assert!(
            undefined_members_named(&diags, "GetManialinkPages").is_empty(),
            "CGameNetwork is a registered receiver, got {:?}",
            diags
        );
    }

    /// B059 sibling negative: same receiver fence as `GetCurrentEntityID`,
    /// and same arity rule.
    #[test]
    fn injected_manialink_pages_receiver_and_arity_fences() {
        let diags = check_with_typedb(
            r#"
            void Use(CSmPlayer@ player, CGameCtnNetwork@ net) {
                auto a = player.GetManialinkPages();
                auto b = net.GetManialinkPages(0);
            }
            "#,
        );
        assert_eq!(
            undefined_members_named(&diags, "GetManialinkPages").len(),
            1,
            "GetManialinkPages off CGameCtnNetwork must still error, got {:?}",
            diags
        );
        assert_eq!(
            arg_count_mismatches(&diags, "GetManialinkPages").len(),
            1,
            "1-arg GetManialinkPages must fire ArgCountMismatch, got {:?}",
            diags
        );
    }

    /// B059, the load-bearing half: the call must yield `uint`, not the
    /// `Error("")` silence sentinel GH #21 uses for the void `MwAddRef`.
    /// A sentinel would suppress the diagnostic AND mis-type `auto entId`,
    /// which is the harder bug to notice — so assert a downstream rule that
    /// only fires when the receiving value really is unsigned. `uint < int`
    /// is `SignedUnsignedMismatch`; a sentinel operand is silent.
    #[test]
    fn injected_entity_id_returns_a_real_unsigned_value() {
        let diags = check_with_typedb(
            r#"
            void Use(CSmPlayer@ player, int limit) {
                auto entId = player.GetCurrentEntityID();
                if (entId < limit) { print("under"); }
            }
            "#,
        );
        let hits: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::SignedUnsignedMismatch))
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "`auto entId` must infer uint (signed/unsigned compare fires), got {:?}",
            diags
        );
    }

    /// B059 sibling, same load-bearing point: the call yields
    /// `array<CGameManialinkPage@>@`, so indexing it gives a receiver whose
    /// members are checked. `Url` is a real member (silent); a bogus name is
    /// not. A silence sentinel would leave BOTH silent.
    #[test]
    fn injected_manialink_pages_returns_a_real_page_array() {
        let diags = check_with_typedb(
            r#"
            void Use(CGameCtnNetwork@ net) {
                auto pages = net.GetManialinkPages();
                auto ok = pages[0].Url;
                auto bad = pages[0].ZZZNoSuchPageMember;
            }
            "#,
        );
        assert!(
            undefined_members_named(&diags, "Url").is_empty(),
            "`Url` is a real CGameManialinkPage member, got {:?}",
            diags
        );
        let bogus = undefined_members_named(&diags, "ZZZNoSuchPageMember");
        assert_eq!(
            bogus.len(),
            1,
            "array element type must be CGameManialinkPage@ and checked, got {:?}",
            diags
        );
        assert!(
            matches!(&bogus[0].kind,
                TypeDiagnosticKind::UndefinedMember { object_type, .. }
                    if object_type.contains("CGameManialinkPage")),
            "element receiver type must be CGameManialinkPage, got {:?}",
            bogus[0].kind
        );
    }

    /// B059, DELIBERATE behaviour change beyond the ticket's scope: moving
    /// `MwAddRef`/`MwRelease` onto the shared table means the table's arity
    /// now applies to them. `nod.MwAddRef(1)` used to be `UndefinedMember`
    /// and, once the name was accepted, would have gone silent — a false
    /// NEGATIVE, since the DLL declares both as `void M()` with no
    /// parameters and the game rejects the call. Pinned here on BOTH
    /// universal methods and on a subclass receiver so it is inherited on
    /// purpose rather than by accident.
    ///
    /// Silence for the legal 0-arg form stays covered by
    /// `mw_addref_release_on_mwnod_stays_silent`.
    #[test]
    fn injected_universal_methods_wrong_arity_still_errors() {
        let diags = check_with_typedb(
            r#"
            void Use(CMwNod@ nod, CGameCtnBlockInfo@ bi) {
                nod.MwAddRef(1);
                nod.MwRelease(1);
                bi.MwAddRef(1, 2);
            }
            "#,
        );
        assert_eq!(
            arg_count_mismatches(&diags, "MwAddRef").len(),
            2,
            "1-arg and 2-arg MwAddRef must fire ArgCountMismatch, got {:?}",
            diags
        );
        assert_eq!(
            arg_count_mismatches(&diags, "MwRelease").len(),
            1,
            "1-arg MwRelease must fire ArgCountMismatch, got {:?}",
            diags
        );
        // The name is still accepted — this is an arity error, not a
        // reversal of GH #21.
        assert!(
            undefined_members_named(&diags, "MwAddRef").is_empty()
                && undefined_members_named(&diags, "MwRelease").is_empty(),
            "arity checking must not re-introduce UndefinedMember, got {:?}",
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

    // ── GH #58: workspace-declared namespaces (B013) ───────────────────────

    /// The motivating repro: `Ns` is declared in the workspace, `Ns::Fn`
    /// matches nothing under it, and the pool is the complete dependency
    /// closure — the game rejects it with `No matching symbol 'Ns::Fn'`.
    #[test]
    fn workspace_namespace_unknown_member_diagnoses() {
        let diags = check_closed_workspace(
            r#"namespace TournamentStation { void Known() {} }
               void Main() { TournamentStation::SendEvent("split", "{}"); }"#,
        );
        assert_eq!(
            no_matching_symbols(&diags),
            vec![("TournamentStation::SendEvent".to_string(), None)],
            "expected exactly one No-matching-symbol, got {:?}",
            diags
        );
    }

    /// Authority guard (B013 decision 3): an incomplete pool — no workspace
    /// root, or an unresolved required dependency — must stay silent. The
    /// module we could not read may contribute the member (AngelScript
    /// namespaces are open and additive).
    #[test]
    fn workspace_namespace_unknown_member_silent_without_closure() {
        let diags = check(
            r#"namespace TournamentStation { void Known() {} }
               void Main() { TournamentStation::SendEvent("split", "{}"); }"#,
        );
        assert!(
            no_matching_symbols(&diags).is_empty(),
            "a non-authoritative pool must stay silent, got {:?}",
            diags
        );
    }

    /// Every symbol kind under the namespace counts as resolution
    /// (B013 decision 4): function, class, enum, funcdef-typed global and
    /// plain global all make `Ns::Name(...)` resolvable.
    #[test]
    fn workspace_namespace_resolvable_members_stay_silent() {
        let diags = check_closed_workspace(
            r#"namespace N {
                   void Fn() {}
                   funcdef void Cb();
                   Cb@ Hook = null;
                   int Count = 0;
                   class Widget { int V; }
                   enum Mode { Idle }
               }
               void Main() {
                   N::Fn();
                   N::Widget();
                   N::Mode(0);
                   N::Hook();
                   int c = N::Count;
               }"#,
        );
        assert!(
            no_matching_symbols(&diags).is_empty(),
            "resolvable members must stay silent, got {:?}",
            diags
        );
    }

    /// A virtual property under the namespace resolves through its
    /// `get_`/`set_` accessors, same as the member gates elsewhere.
    #[test]
    fn workspace_namespace_virtual_property_accessor_stays_silent() {
        let diags = check_closed_workspace(
            r#"namespace N { int get_Speed() { return 1; } }
               void Main() { int s = N::Speed(); }"#,
        );
        assert!(
            no_matching_symbols(&diags).is_empty(),
            "a get_ accessor must resolve the property name, got {:?}",
            diags
        );
    }

    /// Engine namespaces belong to the typedb-driven triggers — a plugin
    /// that adds helpers to `namespace UI` must not turn every real `UI::`
    /// call into an error when no typedb is loaded.
    #[test]
    fn engine_namespace_declared_in_workspace_stays_silent() {
        let diags = check_closed_workspace(
            r#"namespace UI { void MyHelper() {} }
               void Main() { UI::MyHelper(); UI::Begin("x"); }"#,
        );
        assert!(
            no_matching_symbols(&diags).is_empty(),
            "engine namespaces must stay with the typedb triggers, got {:?}",
            diags
        );
    }

    /// `Tests` is an Openplanet Core namespace too. A plugin-local helper
    /// declaration must not arm workspace-only errors for game-owned call
    /// shapes when the typedb is unavailable.
    #[test]
    fn tests_engine_namespace_declared_in_workspace_stays_silent() {
        let diags = check_closed_workspace(
            r#"namespace Tests { void LocalHelper() {} }
               void Main() { Tests::LocalHelper(); Tests::GameOwnedCallShape(); }"#,
        );
        assert!(
            no_matching_symbols(&diags).is_empty(),
            "the Tests engine namespace must stay with typedb triggers, got {:?}",
            diags
        );
    }

    /// AngelScript resolves a qualified call relative to the enclosing
    /// namespace first: inside `Outer`, `Inner::Deep()` binds
    /// `Outer::Inner::Deep` even though a top-level `Inner` also exists.
    #[test]
    fn relative_namespace_resolution_stays_silent() {
        let diags = check_closed_workspace(
            r#"namespace Outer { namespace Inner { void Deep() {} } }
               namespace Inner { void Shallow() {} }
               namespace Outer { void Use() { Inner::Deep(); } }"#,
        );
        assert!(
            no_matching_symbols(&diags).is_empty(),
            "relative namespace resolution must stay silent, got {:?}",
            diags
        );
    }

    /// Only single-segment namespaces are authoritative in this release —
    /// a deeper path stays silent (B013 decision 3).
    #[test]
    fn nested_namespace_path_stays_silent() {
        let diags = check_closed_workspace(
            r#"namespace Outer { namespace Inner { void Deep() {} } }
               void Main() { Outer::Inner::Nope(); }"#,
        );
        assert!(
            no_matching_symbols(&diags).is_empty(),
            "nested namespace paths must stay silent, got {:?}",
            diags
        );
    }

    /// A namespace absent from both the workspace and the typedb keeps the
    /// GH #28 silence — there is nothing authoritative to check against.
    #[test]
    fn undeclared_namespace_stays_silent() {
        let diags = check_closed_workspace("void Main() { NotDeclaredAnywhere::Foo(); }");
        assert!(
            no_matching_symbols(&diags).is_empty(),
            "undeclared namespaces must stay silent, got {:?}",
            diags
        );
    }

    /// A class is not a namespace — AngelScript has no statics, so
    /// `MyClass::Thing()` stays silent rather than guessing.
    #[test]
    fn class_qualified_call_stays_silent() {
        let diags = check_closed_workspace(
            "class MyClass { void Thing() {} } void Main() { MyClass::Thing(); }",
        );
        assert!(
            no_matching_symbols(&diags).is_empty(),
            "class-qualified calls must stay silent, got {:?}",
            diags
        );
    }

    /// Near-name candidates under the namespace populate the hint, exactly
    /// like the typedb `UI::GetIO` rule. The hint is additive — it never
    /// gates the diagnostic (B013 decision 2).
    #[test]
    fn workspace_namespace_near_name_populates_hint() {
        let diags = check_closed_workspace(
            "namespace Hints { void Refresh() {} } void Main() { Hints::Refrsh(); }",
        );
        assert_eq!(
            no_matching_symbols(&diags),
            vec![(
                "Hints::Refrsh".to_string(),
                Some("Hints::Refresh".to_string())
            )],
            "expected a near-name hint, got {:?}",
            diags
        );
    }

    /// The typedb triggers still own their namespaces — `UI::GetIO` fires
    /// once (trigger a), never twice.
    #[test]
    fn typedb_namespace_member_miss_fires_once() {
        let diags = check_with_typedb("void Main() { UI::GetIO(); }");
        assert_eq!(
            no_matching_symbols(&diags).len(),
            1,
            "expected exactly one diagnostic for UI::GetIO, got {:?}",
            diags
        );
    }

    /// B016: the type DB keeps an anonymous enum under declaration context
    /// for internal queries, but source must still diagnose that synthetic
    /// key as an unknown type.
    #[test]
    fn anonymous_member_enum_key_reports_unknown_type() {
        let diags = check_with_typedb("void Main() { Control::CControlEntry::Type synthetic; }");
        assert!(
            diags.iter().any(|diag| matches!(
                &diag.kind,
                TypeDiagnosticKind::UnknownType(name)
                    if name == "Control::CControlEntry::Type"
            )),
            "synthetic anonymous enum key must be rejected in source: {:?}",
            diags
        );
    }

    // === GH #63: typedb funcdef constructors (B022) ===

    /// `UI::InputTextCallback(fn)` is a Core funcdef constructor. Openplanet
    /// accepts it; before indexing funcdefs the call path treats `UI` as a
    /// known API namespace and emits `No matching symbol`.
    #[test]
    fn typedb_funcdef_ctor_input_text_callback_is_known() {
        let diags = check_with_typedb(
            r#"
            void NameFilterCallback(UI::InputTextCallbackData@ data) {}
            void Main() {
                string str;
                bool changed;
                UI::InputText("x", str, changed, 0, UI::InputTextCallback(NameFilterCallback));
            }
            "#,
        );
        assert!(
            no_matching_symbols(&diags).is_empty(),
            "expected UI::InputTextCallback to resolve as a funcdef ctor, got {:?}",
            diags
        );
        let errors: Vec<_> = diags
            .iter()
            .filter(|d| d.severity() == TypeDiagnosticSeverity::Error)
            .collect();
        assert!(
            errors.is_empty(),
            "InputText + InputTextCallback ctor must type-check, got {:?}",
            diags
        );
    }

    /// Same ctor shape on a different typedb funcdef so the name is not
    /// hard-coded to InputTextCallback.
    #[test]
    fn typedb_funcdef_ctor_nod_tree_member_callback() {
        let diags = check_with_typedb(
            r#"
            void OnMember(CMwNod@ nod, Reflection::MwMemberInfo@ info) {}
            void Main() {
                CMwNod@ nod;
                UI::NodTree(nod, UI::NodTreeMemberCallback(OnMember));
            }
            "#,
        );
        assert!(
            no_matching_symbols(&diags).is_empty(),
            "expected UI::NodTreeMemberCallback to resolve, got {:?}",
            diags
        );
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "NodTree + NodTreeMemberCallback ctor must type-check, got {:?}",
            diags
        );
    }

    /// `Dev::ProcIntercept` — namespaced, `T &in` param (not a handle).
    #[test]
    fn typedb_funcdef_ctor_dev_proc_intercept() {
        let diags = check_with_typedb(
            r#"
            bool Hook(CMwStack&in stack) { return false; }
            void Main() {
                Dev::InterceptProc("CMwNod", "MwAddRef", Dev::ProcIntercept(Hook));
            }
            "#,
        );
        assert!(
            no_matching_symbols(&diags).is_empty(),
            "expected Dev::ProcIntercept to resolve, got {:?}",
            diags
        );
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "InterceptProc + ProcIntercept ctor must type-check, got {:?}",
            diags
        );
    }

    /// Un-namespaced Core funcdef (`CoroutineFunc`) plus the `const T &in`
    /// sibling (`CoroutineFuncUserdataString`).
    #[test]
    fn typedb_funcdef_ctor_unnamespaced_coroutine_family() {
        let diags = check_with_typedb(
            r#"
            void Worker() {}
            void WorkerStr(const string &in s) {}
            void Main() {
                startnew(CoroutineFunc(Worker));
                startnew(CoroutineFuncUserdataString(WorkerStr), "x");
            }
            "#,
        );
        assert!(
            no_matching_symbols(&diags).is_empty(),
            "expected CoroutineFunc* ctors to resolve, got {:?}",
            diags
        );
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "startnew + CoroutineFunc* ctors must type-check, got {:?}",
            diags
        );
    }

    /// Workspace-declared funcdefs keep working via the same ctor path
    /// (free function and method callback).
    #[test]
    fn workspace_funcdef_ctor_still_clean() {
        let diags = check(
            r#"
            funcdef void Handler(int);
            void OnThing(int a) {}
            void Main() { Handler@ h = Handler(OnThing); }
            "#,
        );
        assert!(
            diags.is_empty(),
            "workspace Handler(OnThing) must stay clean, got {:?}",
            diags
        );

        let method = check(
            r#"
            funcdef void Handler(int);
            class Server {
                Handler@ h;
                void Wire() { @h = Handler(OnThing); }
                void OnThing(int a) {}
            }
            void Main() { Server s; s.Wire(); }
            "#,
        );
        assert!(
            method.is_empty(),
            "workspace method callback must stay clean, got {:?}",
            method
        );
    }

    /// A class method with a matching signature binds the same way a free
    /// function does.
    #[test]
    fn typedb_funcdef_ctor_accepts_method_callback() {
        let diags = check_with_typedb(
            r#"
            class Filter {
                void OnFilter(UI::InputTextCallbackData@ data) {}
                void Wire() {
                    string str;
                    bool changed;
                    UI::InputText("x", str, changed, 0, UI::InputTextCallback(OnFilter));
                }
            }
            "#,
        );
        assert!(
            no_matching_symbols(&diags).is_empty(),
            "method callback must not look like a missing symbol, got {:?}",
            diags
        );
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "method callback must type-check, got {:?}",
            diags
        );
    }

    /// Wrong callback signature / ctor arity still error.
    #[test]
    fn typedb_funcdef_ctor_wrong_signature_and_arity_error() {
        let diags = check_with_typedb(
            r#"
            void Wrong(int x) {}
            void Ok(UI::InputTextCallbackData@ data) {}
            void Extra(int a, int b) {}
            void Main() {
                UI::InputTextCallback(Wrong);
                UI::InputTextCallback(Ok, Extra);
                UI::InputTextCallback();
            }
            "#,
        );
        let mismatches: Vec<_> = diags
            .iter()
            .filter(|d| matches!(d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        let arities: Vec<_> = diags
            .iter()
            .filter(|d| matches!(d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .collect();
        assert_eq!(
            mismatches.len(),
            1,
            "expected 1 ArgTypeMismatch for Wrong, got {:?}",
            diags
        );
        assert_eq!(
            arities.len(),
            2,
            "expected 2 ArgCountMismatch (2-arg and 0-arg), got {:?}",
            diags
        );
        assert!(
            no_matching_symbols(&diags).is_empty(),
            "wrong-sig still names a real funcdef, got {:?}",
            diags
        );
    }

    /// B056 checker fence: the BARE spelling is the one a plugin author writes,
    /// and it is an alias of the same enum as any scope-qualified form. Before
    /// the canonicalization fix all three of these mismatched; two were false
    /// positives on bindings the game accepts.
    ///
    /// B056 checker fence: the fixture's own aliases for one enum must compare
    /// equal inside funcdef signatures, while a genuinely different enum must
    /// remain incompatible. This asserts internal typedb/checker consistency,
    /// not that either alias spelling is accepted by the game.
    #[test]
    fn funcdef_signature_comparison_honors_typedb_enum_alias_identity() {
        let diags = check_with_typedb(
            r#"
            funcdef void EnumCallback(Meta::EDummyCollectionIdent value);
            void SameEnum(UI::EDummyCollectionIdent value) {}
            void BareEnum(EDummyCollectionIdent value) {}
            void DistinctEnum(CGameCtnBlock::ECardinalDirections value) {}
            void Main() {
                EnumCallback(SameEnum);
                EnumCallback(BareEnum);
                EnumCallback(DistinctEnum);
            }
            "#,
        );
        let mismatches: Vec<_> = diags
            .iter()
            .filter(|d| matches!(d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .collect();
        assert_eq!(
            mismatches.len(),
            1,
            "both same-enum spellings must bind and the distinct enum must not, got {:?}",
            diags
        );
        assert_eq!(
            mismatches[0].kind,
            TypeDiagnosticKind::ArgTypeMismatch {
                function_name: "EnumCallback".into(),
                param_index: 0,
                expected: "void (Meta::EDummyCollectionIdent)".into(),
                got: "void (CGameCtnBlock::ECardinalDirections)".into(),
            },
            "the sole mismatch must be the distinct-enum signature"
        );
    }

    /// Typedefs are stored as `SymbolKind::Funcdef` in the symbol table
    /// but are not funcdef constructors — `MyInt(1)` must stay a type
    /// conversion, not `ArgTypeMismatch`.
    #[test]
    fn workspace_typedef_construct_is_not_a_funcdef_ctor() {
        let diags = check(
            r#"
            typedef int MyInt;
            void Main() { MyInt x = MyInt(42); }
            "#,
        );
        assert!(
            !diags.iter().any(|d| matches!(
                d.kind,
                TypeDiagnosticKind::ArgTypeMismatch { .. }
                    | TypeDiagnosticKind::ArgCountMismatch { .. }
                    | TypeDiagnosticKind::UnknownType(_)
            )),
            "typedef construction must not be treated as a funcdef ctor, got {:?}",
            diags
        );
    }

    /// Unknown `UI::DefinitelyNotAFuncdef(...)` is still `No matching symbol`.
    #[test]
    fn unknown_ui_funcdef_still_no_matching_symbol() {
        let diags = check_with_typedb(
            r#"
            void Cb() {}
            void Main() { UI::DefinitelyNotAFuncdef(Cb); }
            "#,
        );
        assert_eq!(
            no_matching_symbols(&diags),
            vec![("UI::DefinitelyNotAFuncdef".to_string(), None)],
            "unknown UI member must stay No matching symbol, got {:?}",
            diags
        );
    }

    // === B025: `this.Method` / `obj.Method` references at funcdef ctor sites ===

    /// `CoroutineFunc(this.M)` is the required method-to-funcdef conversion
    /// (class methods do not implicitly decay the way free functions do), so
    /// the ctor arg is a method *reference* — not the method's return type.
    /// Game ground truth (RemoteBuild 2026-08-29, OP 1.29.5): CLEAN.
    #[test]
    fn typedb_funcdef_ctor_accepts_this_method_reference() {
        let diags = check_with_typedb(
            r#"
            class C {
                void M() {}
                void T() {
                    startnew(CoroutineFunc(this.M));
                }
            }
            "#,
        );
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "`CoroutineFunc(this.M)` must type-check as a method reference, got {:?}",
            diags
        );
    }

    /// Same reference through a local instance (`c.M`): the receiver's class
    /// comes from the local's declared type. Game ground truth (RemoteBuild
    /// 2026-08-29, OP 1.29.5): CLEAN.
    #[test]
    fn typedb_funcdef_ctor_accepts_local_instance_method_reference() {
        let diags = check_with_typedb(
            r#"
            class C {
                void M() {}
            }
            void Main() {
                C c;
                startnew(CoroutineFunc(c.M));
            }
            "#,
        );
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "`CoroutineFunc(c.M)` must type-check as a method reference, got {:?}",
            diags
        );
    }

    /// Handle init and ternary arms go through the same ctor path. Game
    /// ground truth (RemoteBuild 2026-08-29, OP 1.29.5): CLEAN.
    #[test]
    fn typedb_funcdef_ctor_this_method_in_handle_init_and_ternary() {
        let diags = check_with_typedb(
            r#"
            class C {
                void M() {}
                void Pick(bool use) {
                    auto @h = CoroutineFunc(this.M);
                    C o;
                    auto @h2 = use ? CoroutineFunc(o.M) : null;
                }
            }
            "#,
        );
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "handle init / ternary method references must type-check, got {:?}",
            diags
        );
    }

    /// Overloaded methods bind by compatibility: `void OV()` + `bool OV(int)`
    /// must accept `CoroutineFunc(this.OV)` because one overload matches —
    /// the game resolves the same way. Real-world shape: tm-editor-plus-plus
    /// Favorites.as `CoroutineFunc(this.OpenInvFolderMb)`. Game ground truth
    /// (RemoteBuild 2026-08-29, OP 1.29.5): CLEAN.
    #[test]
    fn typedb_funcdef_ctor_overloaded_method_binds_compatible_overload() {
        let diags = check_with_typedb(
            r#"
            class C {
                void OV() {}
                bool OV(int x) { return true; }
                void T() {
                    startnew(CoroutineFunc(this.OV));
                }
            }
            "#,
        );
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "an overload-compatible method reference must bind, got {:?}",
            diags
        );
    }

    /// `void(ref@)` method into `CoroutineFuncUserdata` (`void (ref)`):
    /// signature-compatible via the ctor conversion. Real-world shape:
    /// tm-editor-plus-plus Map_EditProps.as `CoroutineFuncUserdata(this.RevertDeco)`.
    /// Game ground truth (RemoteBuild 2026-08-29, OP 1.29.5): CLEAN.
    #[test]
    fn typedb_funcdef_ctor_userdata_accepts_ref_method_reference() {
        let diags = check_with_typedb(
            r#"
            class C {
                void RevertDeco(ref@ data) {}
                void T() {
                    startnew(CoroutineFuncUserdata(this.RevertDeco), this);
                }
            }
            "#,
        );
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "`CoroutineFuncUserdata(this.voidRefMethod)` must type-check, got {:?}",
            diags
        );
    }

    /// Receivers beyond `this`/locals: a workspace global (`g_cache.Update`)
    /// and an indexed array (`edges[i].Disconnect`, `g_edges[0].Disconnect` —
    /// field or global array). Real-world shapes: tm-editor-plus-plus
    /// NodeGraph/Node.as + Tools/MaterialsList.as. Game ground truth
    /// (RemoteBuild 2026-08-29, OP 1.29.5): CLEAN.
    #[test]
    fn typedb_funcdef_ctor_accepts_global_and_indexed_receivers() {
        let diags = check_with_typedb(
            r#"
            class Noodle { void Disconnect() {} }
            class Cache { void Update() {} }
            Noodle@[] g_edges;
            Cache@ g_cache;

            class Node {
                array<Noodle@> edges;
                void Wire() {
                    startnew(CoroutineFunc(g_cache.Update));
                    startnew(CoroutineFunc(g_edges[0].Disconnect));
                    startnew(CoroutineFunc(edges[0].Disconnect));
                }
            }
            "#,
        );
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "global/indexed method references must type-check, got {:?}",
            diags
        );
    }

    // Keep-flagged guards: game-verified rejections (RemoteBuild 2026-08-29,
    // OP 1.29.5 — the game errors with `Can't create delegate`).

    /// A `bool()` method does not convert to the `void()` `CoroutineFunc` —
    /// return types must match exactly.
    #[test]
    fn typedb_funcdef_ctor_rejects_bool_method_for_void_funcdef() {
        let diags = check_with_typedb(
            r#"
            class C {
                bool B() { return true; }
                void T() {
                    startnew(CoroutineFunc(this.B));
                }
            }
            "#,
        );
        assert!(
            diags
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. })),
            "`CoroutineFunc(this.boolMethod)` must stay flagged, got {:?}",
            diags
        );
    }

    /// A `void()` method does not convert to `CoroutineFuncUserdata`
    /// (`void (ref)`) — parameter lists must match.
    #[test]
    fn typedb_funcdef_ctor_rejects_void_method_for_userdata_funcdef() {
        let diags = check_with_typedb(
            r#"
            class C {
                void M() {}
                void T() {
                    startnew(CoroutineFuncUserdata(this.M), this);
                }
            }
            "#,
        );
        assert!(
            diags
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. })),
            "`CoroutineFuncUserdata(this.voidMethod)` must stay flagged, got {:?}",
            diags
        );
    }

    /// A non-method (primitive-typed property) member is still clearly not a
    /// function.
    #[test]
    fn typedb_funcdef_ctor_rejects_property_member_reference() {
        let diags = check_with_typedb(
            r#"
            class C {
                string label = "x";
                void T() {
                    startnew(CoroutineFunc(this.label));
                }
            }
            "#,
        );
        assert!(
            diags
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. })),
            "a property reference must stay flagged, got {:?}",
            diags
        );
    }

    /// Members shadow globals: a bare-ident receiver must resolve exactly
    /// like `expr_type` does — class field first, then global. Both
    /// polarities follow the FIELD's type, because that is the delegate the
    /// game builds (B025 review finding; field `int M()` → game errors
    /// `Can't create delegate`).
    #[test]
    fn method_ref_receiver_prefers_class_field_over_same_named_global() {
        let diags = check_with_typedb(
            r#"
            class A { void M() {} }
            class B { int M() { return 0; } }
            A@ h;
            class C {
                B@ h;
                void T() {
                    startnew(CoroutineFunc(h.M));
                }
            }
            "#,
        );
        assert!(
            diags
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. })),
            "field shadows global: int() field method into void() funcdef must stay flagged, got {:?}",
            diags
        );
    }

    #[test]
    fn method_ref_receiver_field_binds_when_global_type_lacks_method() {
        let diags = check_with_typedb(
            r#"
            class A { void Nope() {} }
            class B { void M() {} }
            A@ h;
            class C {
                B@ h;
                void T() {
                    startnew(CoroutineFunc(h.M));
                }
            }
            "#,
        );
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "field's void() method must bind (field shadows global), got {:?}",
            diags
        );
    }

    /// Chained member receivers (`box.inner.Go`) and Index-over-member
    /// (`box.ns[0].Go`) are the same method-reference idiom. Game ground
    /// truth: the delegate conversion is the same shape as the probed
    /// `field[i].M` / `local.field.M` forms (CLEAN).
    #[test]
    fn method_ref_receiver_resolves_chained_member_objects() {
        let diags = check_with_typedb(
            r#"
            class Inner { void Go() {} }
            class N { void Go() {} }
            class Box { Inner@ inner; array<N@>@ ns; }
            class C {
                Box@ box;
                void T() {
                    startnew(CoroutineFunc(box.inner.Go));
                    startnew(CoroutineFunc(box.ns[0].Go));
                }
            }
            "#,
        );
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "chained member method references must type-check, got {:?}",
            diags
        );
    }

    /// Call-result and cast receivers (`GetN().Disconnect`,
    /// `cast<N>(h).Disconnect`) are legal delegate sources. Game ground
    /// truth (RemoteBuild 2026-08-29, OP 1.29.5): CLEAN.
    #[test]
    fn method_ref_receiver_resolves_call_and_cast_receivers() {
        let diags = check_with_typedb(
            r#"
            class N { void Disconnect() {} }
            N@ GetN() { return N(); }
            void Main() {
                N@ h = N();
                startnew(CoroutineFunc(GetN().Disconnect));
                startnew(CoroutineFunc(cast<N>(h).Disconnect));
            }
            "#,
        );
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "call-result / cast receivers must type-check, got {:?}",
            diags
        );
    }

    /// Engine-typed receivers bind through the typedb's method overloads.
    /// Game ground truth (RemoteBuild 2026-08-29, OP 1.29.5): CLEAN
    /// (`Net::HttpRequest@ r; CoroutineFunc(r.Cancel)` — game accepts the
    /// delegate, LSP must not flag it).
    #[test]
    fn method_ref_receiver_binds_engine_typed_receiver_methods() {
        let diags = check_with_typedb(
            r#"
            void Main() {
                Net::HttpRequest@ r = Net::HttpGet("http://localhost/");
                startnew(CoroutineFunc(r.Cancel));
            }
            "#,
        );
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "engine-typed receiver methods must type-check, got {:?}",
            diags
        );
    }

    /// Generalization variants (B025 review): inherited methods, handle-typed
    /// local receivers, `this.M` in a ternary arm, and reversed overload
    /// declaration order must all bind.
    #[test]
    fn method_ref_receiver_generalization_variants_bind() {
        let diags = check_with_typedb(
            r#"
            class Base { void Inherited() {} }
            class Derived : Base {
                void OV(int x) {}
                void OV() {}
                void T(bool flag) {
                    startnew(CoroutineFunc(this.Inherited));
                    Derived@ d = this;
                    startnew(CoroutineFunc(d.Inherited));
                    startnew(CoroutineFunc(flag ? this.OV : null));
                    startnew(CoroutineFunc(this.OV));
                }
            }
            "#,
        );
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "generalized receiver/overload variants must type-check, got {:?}",
            diags
        );
    }

    /// Chained method-call receivers (`this.Make().Go`, `c.Make().Go`) and
    /// bare factory callees inside their namespace (`Make().Go` in
    /// `namespace Lib`) are legal delegate sources. Game ground truth
    /// (RemoteBuild 2026-08-29, OP 1.29.5): CLEAN.
    #[test]
    fn method_ref_receiver_resolves_chained_method_calls() {
        let diags = check_with_typedb(
            r#"
            namespace Lib {
                class P { void Go() {} }
                P@ Make() { return P(); }
                void Use() {
                    startnew(CoroutineFunc(Make().Go));
                }
            }
            class N { void Go() {} }
            class C {
                N MakeN() { return N(); }
                void T() {
                    startnew(CoroutineFunc(this.MakeN().Go));
                }
            }
            void Main() {
                C c;
                startnew(CoroutineFunc(c.MakeN().Go));
            }
            "#,
        );
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "chained method-call receivers must type-check, got {:?}",
            diags
        );
    }

    /// B027 review follow-up: a parameterless workspace funcdef must reach
    /// the ctor arity check — it used to collide with the typedef shape at
    /// `lookup_funcdef` (return_type set + params empty), which silently
    /// dropped the diagnostic. Game: delegate construction requires the
    /// exact signature (b025/b027 fixture ground truth), so wrong arity
    /// rejects and exact arity binds.
    #[test]
    fn funcdef_ctor_parameterless_funcdef_rejects_wrong_arity() {
        let diags = check_with_typedb(
            r#"
            funcdef void CB();
            void Tick() {}
            void Render() {
                CB@ bad = CB(Tick, 42);
                CB@ ok = CB(Tick);
            }
            "#,
        );
        let arity = diags
            .iter()
            .filter(|d| matches!(d.kind, TypeDiagnosticKind::ArgCountMismatch { .. }))
            .count();
        assert_eq!(
            arity, 1,
            "CB(Tick, 42) must reject on arity, CB(Tick) must bind, got {:?}",
            diags
        );
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error
                    && !matches!(d.kind, TypeDiagnosticKind::ArgCountMismatch { .. })),
            "the exact-arity ctor must stay silent, got {:?}",
            diags
        );
    }

    /// Delegate construction requires exact parameter passing modes: a
    /// `void(int &out)` method does NOT convert to `funcdef void(int)` —
    /// game ground truth (RemoteBuild 2026-08-29, OP 1.29.5): ERR
    /// `Can't create delegate`. Both polarities must flag.
    #[test]
    fn funcdef_ctor_rejects_out_param_modifier_mismatch() {
        let diags = check_with_typedb(
            r#"
            funcdef void FDI(int);
            funcdef void FDO(int &out);
            class C {
                void IntOut(int &out x) { x = 1; }
                void Plain(int x) {}
                void T() {
                    startnew(FDI(IntOut));
                    startnew(FDO(Plain));
                }
            }
            "#,
        );
        let mismatches = diags
            .iter()
            .filter(|d| matches!(d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .count();
        assert_eq!(
            mismatches, 2,
            "both &out-modifier mismatches must be flagged, got {:?}",
            diags
        );
    }

    /// B027 generalization (CLAUDE.md): the same exact-mode rule for free
    /// functions, both polarities; exact matches keep binding.
    #[test]
    fn funcdef_ctor_out_mode_mismatch_free_functions() {
        let diags = check(
            r#"
            funcdef void FDI(int);
            funcdef void FDO(int &out);
            void FnPlain(int x) {}
            void FnOut(int &out x) {}
            void Main() {
                FDI@ a = FDI(FnOut);
                FDO@ b = FDO(FnPlain);
                FDI@ c = FDI(FnPlain);
                FDO@ d = FDO(FnOut);
            }
            "#,
        );
        let mismatches = diags
            .iter()
            .filter(|d| matches!(d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .count();
        assert_eq!(
            mismatches, 2,
            "free-fn &out mismatches must flag both polarities and nothing else, got {:?}",
            diags
        );
    }

    /// B027: `&in` is enforced exactly like `&out` — game probe
    /// (RemoteBuild 2026-08-29, OP 1.29.5): `FIn(plain)` and
    /// `FPlain(fnWithIn)` both error, while exact `&in`↔`&in` binds.
    #[test]
    fn funcdef_ctor_enforces_in_mode_exactly() {
        let diags = check(
            r#"
            funcdef void FPlain(int);
            funcdef void FIn(int &in);
            void FnPlain(int x) {}
            void FnIn(int &in x) {}
            void Main() {
                FIn@ a = FIn(FnPlain);
                FPlain@ b = FPlain(FnIn);
                FIn@ c = FIn(FnIn);
                FPlain@ d = FPlain(FnPlain);
            }
            "#,
        );
        let mismatches = diags
            .iter()
            .filter(|d| matches!(d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .count();
        assert_eq!(
            mismatches, 2,
            "&in mismatches must flag both polarities, exact &in binds, got {:?}",
            diags
        );
    }

    /// B027: `&inout` is only legal on object types (the game rejects
    /// `int &inout` at declaration) and is enforced exactly — game probe
    /// 2026-08-29: `GInout(&inout)` vs `&out`/value methods error,
    /// `GInout` vs `&inout` method binds.
    #[test]
    fn funcdef_ctor_enforces_inout_mode_for_object_types() {
        let diags = check(
            r#"
            class H {}
            funcdef void GInout(H &inout);
            class D {
                void HPlain(H x) {}
                void HOut(H &out x) {}
                void HInout(H &inout x) {}
            }
            void Main() {
                D d;
                GInout@ a = GInout(d.HPlain);
                GInout@ b = GInout(d.HOut);
                GInout@ c = GInout(d.HInout);
            }
            "#,
        );
        let mismatches = diags
            .iter()
            .filter(|d| matches!(d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. }))
            .count();
        assert_eq!(
            mismatches, 2,
            "&inout mismatch vs value/&out methods must flag, exact &inout binds, got {:?}",
            diags
        );
    }

    /// B027: typedb method signatures carry the `&out` mode only as
    /// `OverloadSig.param_out` (their param text has no mode decoration) —
    /// the flag must participate in the same exact-mode comparison for
    /// both sides of the funcdef.
    #[test]
    fn func_matches_funcdef_uses_typedb_out_flag() {
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, None);
        let checker = Checker::new("", &scope);
        let engine_out_method = OverloadSig {
            param_names: vec![None],
            param_types: vec!["float".to_string()],
            param_out: vec![true],
            param_modes: vec![ParamMode::Out],
            min_args: 1,
            return_type: "void".to_string(),
        };
        let fd = |mode: ParamMode| FuncdefSig {
            qualified: "FD".to_string(),
            return_type: "void".to_string(),
            param_types: vec!["float".to_string()],
            param_modes: vec![mode],
        };
        assert!(
            !checker.func_matches_funcdef(&engine_out_method, &fd(ParamMode::Value)),
            "&out arg must not bind a value funcdef"
        );
        assert!(
            checker.func_matches_funcdef(&engine_out_method, &fd(ParamMode::Out)),
            "&out arg must bind an &out funcdef"
        );
        assert!(
            !checker.func_matches_funcdef(&engine_out_method, &fd(ParamMode::In)),
            "&out arg must not bind an &in funcdef"
        );
    }

    // ── B023: wrong-signature / non-function args at typedb funcdef slots ──

    /// Helper for the B023 tests: a `UI::InputTextCallbackData@` callback
    /// and a same-named-shape `void(int)` incompatible function.
    const B023_FNS: &str = r###"
        void Good(UI::InputTextCallbackData@ data) { data.SelectAll(); }
        void WrongSig(int x) {}
        void Tick() {}
    "###;

    /// B023: a resolvable function with an incompatible signature must
    /// reject every arity-compatible candidate at a typedb funcdef slot.
    /// UI::InputText has two 4-arg-compatible overloads, so this is the
    /// GH #47 overload-set path; emission is `NoMatchingSignatures`, like
    /// the game (`No matching signatures to 'UI::InputText(...)'`).
    #[test]
    fn external_overload_set_rejects_wrong_signature_function_arg() {
        let diags = check_with_typedb(&format!(
            "{}\n{}",
            B023_FNS,
            r###"
            void Render() {
                string s = "test";
                UI::InputText("##a", s, UI::InputTextFlags::None, WrongSig);
            }
            "###
        ));
        assert!(
            diags
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::NoMatchingSignatures { .. })),
            "wrong-sig fn into the InputText callback slot must be rejected, got {:?}",
            diags
        );
    }

    /// B023: a provably non-function arg (string literal) into a funcdef
    /// slot rejects every candidate — game ERR on the same line.
    #[test]
    fn external_overload_set_rejects_non_function_literal_arg() {
        let diags = check_with_typedb(&format!(
            "{}\n{}",
            B023_FNS,
            r###"
            void Render() {
                string s = "test";
                UI::InputText("##b", s, UI::InputTextFlags::None, "nope");
            }
            "###
        ));
        assert!(
            diags
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::NoMatchingSignatures { .. })),
            "string literal into the InputText callback slot must be rejected, got {:?}",
            diags
        );
    }

    /// B023: same machinery on the InputTextMultiline callback slot.
    #[test]
    fn external_overload_set_rejects_wrong_signature_in_multiline_slot() {
        let diags = check_with_typedb(&format!(
            "{}\n{}",
            B023_FNS,
            r###"
            void Render() {
                string s = "test";
                UI::InputTextMultiline("##c", s, vec2(), UI::InputTextFlags::None, WrongSig);
            }
            "###
        ));
        assert!(
            diags
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::NoMatchingSignatures { .. })),
            "wrong-sig fn into the Multiline callback slot must be rejected, got {:?}",
            diags
        );
    }

    /// B110: THE SHAPE 204 REAL CALL SITES USE, and the reason this ticket's
    /// fleet delta came back at zero.
    ///
    /// `startnew(H, u)` resolves against nine overloads, of which the
    /// `CoroutineFuncUserdata*` family differ only in the userdata type. Before
    /// B110 a handler matched several of them at once through numeric
    /// convertibility; now it matches exactly one. The call is silent either
    /// way, because the one it matches is the right one — that is what the
    /// instrumented sweep showed on 15 plugins and 408 flipped comparisons,
    /// and this is that shape held still.
    ///
    /// The required silence, not an incidental one: if this went red, the
    /// change would be a false error on `startnew(SetTimeLimit_Async, 120)`,
    /// which `tm-simple-room-admin` ships.
    #[test]
    fn b110_startnew_userdata_handlers_still_bind_their_exact_slot() {
        for (param, arg) in [
            ("int64 v", "1"),
            ("uint64 v", "uint64(1)"),
            ("double v", "1.0"),
            ("bool v", "true"),
            ("const string &in v", "\"x\""),
        ] {
            let src = format!("void H({param}) {{ }}\nvoid Render() {{ startnew(H, {arg}); }}");
            let diags = check_with_typedb(&src);
            let bad: Vec<_> = diags
                .iter()
                .filter(|d| {
                    matches!(
                        d.kind,
                        TypeDiagnosticKind::NoMatchingSignatures { .. }
                            | TypeDiagnosticKind::ArgTypeMismatch { .. }
                    )
                })
                .collect();
            assert!(
                bad.is_empty(),
                "a handler whose userdata type EXACTLY matches a \
                 CoroutineFuncUserdata* slot must still bind: {bad:?}\n{src}"
            );
        }
    }

    /// B165's directly measured real built-in seam, isolated rather than
    /// relying only on the exhaustive local-funcdef lattice.
    ///
    /// A handler declared with a BARE `int` at the `int64` userdata slot is
    /// rejected by the engine itself: B165's `startnew` row reports `No
    /// matching signatures`, and all eight generalized width/sign directions
    /// reject too (`docs/probes/b165-primitive-funcdef.tsv`). No plugin in the
    /// 151-plugin corpus writes the bare-int shape; the exposure sweep still
    /// reaches 408 neighbouring overload comparisons, so that zero is a real
    /// population result rather than a dead instrument.
    #[test]
    fn b165_a_bare_int_handler_does_not_bind_the_int64_userdata_slot() {
        let diags = check_with_typedb("void H(int v) { }\nvoid Render() { startnew(H, 1); }");
        assert!(
            diags
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::NoMatchingSignatures { .. })),
            "void(int) is not void(int64) and the engine compares signatures \
             rather than converting them: {diags:?}"
        );
    }

    /// B102's TEMPLATE-MEMBER slot reaches the same predicate, with the
    /// element type substituted per call site rather than looked up. An
    /// `int[]` comparator must take `int`, not merely something an `int`
    /// converts to.
    ///
    /// Both directions in one test, because the reject half alone is
    /// satisfied by a rule that rejects every comparator.
    #[test]
    fn b110_array_sort_comparator_element_type_is_exact() {
        let exact = check_with_typedb(
            "bool Less(const int &in a, const int &in b) { return a < b; }\n\
             void Render() { int[] xs = {3, 1}; xs.Sort(Less); }",
        );
        assert!(
            exact
                .iter()
                .all(|d| !matches!(d.kind, TypeDiagnosticKind::NoMatchingSignatures { .. })),
            "an int comparator on an int[] must bind: {exact:?}"
        );
        let widened = check_with_typedb(
            "bool Less(const int64 &in a, const int64 &in b) { return a < b; }\n\
             void Render() { int[] xs = {3, 1}; xs.Sort(Less); }",
        );
        assert!(
            widened
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::NoMatchingSignatures { .. })),
            "an int64 comparator is not an int comparator: {widened:?}"
        );
    }

    /// B023 generalization (CLAUDE.md), **corrected by B091**: at a funcdef
    /// slot a bare class-METHOD reference rejects whatever its signature is,
    /// and the CONSTRUCTOR is what binds.
    ///
    /// This test used to assert the opposite second polarity — that
    /// `this.OnFilter`, a method matching the slot exactly, binds silently —
    /// and B091 turned it red. That half was never measured. B023's game
    /// probe (2026-08-29, OP 1.29.5) verified seven REJECT and six legal
    /// lines and not one of them is a bare method reference: its legal half
    /// is free functions, ctor-wrapped functions and right-typed variables.
    /// The assertion generalised the FREE-FUNCTION decay rule onto methods,
    /// and methods are precisely where that rule does not hold — which
    /// B025's own prose says, with the same probe behind it. The population
    /// agrees: across 151 shipped plugins there are 235
    /// `CoroutineFunc*(receiver.Method)` constructions and zero bare method
    /// references at any funcdef slot.
    ///
    /// Split into two checks rather than counted over one, because a single
    /// count of 2 over three rows cannot tell "both bare rows rejected" from
    /// "one bare row and the wrapped row rejected".
    #[test]
    fn external_overload_set_method_reference_both_polarities() {
        let bare = check_with_typedb(&format!(
            "{}\n{}",
            B023_FNS,
            r###"
            class Filter {
                void WrongMethod(int x) {}
                void OnFilter(UI::InputTextCallbackData@ data) { data.SelectAll(); }
                void Wire() {
                    string s = "test";
                    UI::InputText("##1", s, UI::InputTextFlags::None, this.WrongMethod);
                    UI::InputText("##2", s, UI::InputTextFlags::None, this.OnFilter);
                }
            }
            void Render() {
                Filter f;
                f.Wire();
            }
            "###
        ));
        let rejections = bare
            .iter()
            .filter(|d| matches!(d.kind, TypeDiagnosticKind::NoMatchingSignatures { .. }))
            .count();
        assert_eq!(
            rejections, 2,
            "a bare method reference cannot bind a funcdef slot whatever its \
             signature: the constructor is the conversion. got {:?}",
            bare
        );

        let wrapped = check_with_typedb(&format!(
            "{}\n{}",
            B023_FNS,
            r###"
            class Filter {
                void OnFilter(UI::InputTextCallbackData@ data) { data.SelectAll(); }
                void Wire() {
                    string s = "test";
                    UI::InputText("##1", s, UI::InputTextFlags::None,
                                  UI::InputTextCallback(this.OnFilter));
                }
            }
            void Render() {
                Filter f;
                f.Wire();
            }
            "###
        ));
        assert!(
            !wrapped
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::NoMatchingSignatures { .. })),
            "the ctor-wrapped matching method IS the legal form and must bind, \
             got {:?}",
            wrapped
        );
    }

    /// B023 FP guard half (CLAUDE.md generalization): every matching shape
    /// stays silent — free fn, ctor-wrapped fn, correct funcdef-typed local.
    #[test]
    fn external_overload_set_keeps_matching_function_args_silent() {
        let diags = check_with_typedb(&format!(
            "{}\n{}",
            B023_FNS,
            r###"
            void Render() {
                string s = "test";
                UI::InputText("##1", s, UI::InputTextFlags::None, Good);
                UI::InputText("##2", s, UI::InputTextFlags::None, UI::InputTextCallback(Good));
                UI::InputTextCallback@ cb = Good;
                UI::InputText("##3", s, UI::InputTextFlags::None, cb);
            }
            "###
        ));
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "matching callbacks must stay silent, got {:?}",
            diags
        );
    }

    /// B023: a funcdef-typed variable of an incompatible type rejects the
    /// candidates; one of the correct type binds (CLAUDE.md both-polarity).
    #[test]
    fn external_overload_set_funcdef_typed_variable_both_polarities() {
        let diags = check_with_typedb(&format!(
            "{}\n{}",
            B023_FNS,
            r###"
            funcdef void WSig(int x);
            void Render() {
                string s = "test";
                WSig@ wrong = null;
                UI::InputTextCallback@ right = Good;
                UI::InputText("##1", s, UI::InputTextFlags::None, wrong);
                UI::InputText("##2", s, UI::InputTextFlags::None, right);
            }
            "###
        ));
        let rejections = diags
            .iter()
            .filter(|d| matches!(d.kind, TypeDiagnosticKind::NoMatchingSignatures { .. }))
            .count();
        assert_eq!(
            rejections, 1,
            "wrong-typed funcdef var must reject, right-typed must bind, got {:?}",
            diags
        );
    }

    /// B023: wrong-sig fn into `startnew` (CoroutineFunc family) — a unique
    /// arity match takes the unique-overload path, which needs the same
    /// bind test. Game ERR; `startnew(Tick)` stays silent.
    #[test]
    fn external_unique_overload_rejects_wrong_signature_startnew() {
        let diags = check_with_typedb(&format!(
            "{}\n{}",
            B023_FNS,
            r###"
            void Render() {
                startnew(WrongSig);
                startnew(Tick);
            }
            "###
        ));
        let rejections = diags
            .iter()
            .filter(|d| matches!(d.kind, TypeDiagnosticKind::NoMatchingSignatures { .. }))
            .count();
        assert_eq!(
            rejections, 1,
            "startnew(WrongSig) must reject, startnew(Tick) must bind, got {:?}",
            diags
        );
    }

    /// B023 review follow-up: name resolution honors locals first
    /// (expr_type's Ident precedence). A local funcdef handle named like an
    /// incompatible global function binds by its own type — the shadowed
    /// global's signature must never be consulted. (Game: a funcdef-typed
    /// variable at a funcdef slot binds — b023 fixture legal lines.)
    #[test]
    fn function_arg_ident_respects_local_funcdef_shadowing() {
        let diags = check_with_typedb(&format!(
            "{}\n{}",
            B023_FNS,
            r###"
            void Render() {
                string s = "test";
                UI::InputTextCallback@ WrongSig = Good;
                UI::InputText("##1", s, UI::InputTextFlags::None, WrongSig);
            }
            "###
        ));
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "local funcdef handle must shadow the same-named global fn and bind, got {:?}",
            diags
        );
    }

    /// B023 review follow-up (guard): a local of non-function type that
    /// shadows a global function name stays neutral — the name resolves to
    /// the local, and Named shapes never reject a candidate on their own
    /// (B023's Not-clause guard). Probe note (RemoteBuild 2026-08-29): the
    /// game REJECTS this exact shape (`No matching signatures to
    /// 'UI::InputText(..., UI::InputTextCallbackData@&)'`); neutrality
    /// trades that parity for FP safety across all Named shapes and is
    /// recorded on the ticket as a known false-negative.
    #[test]
    fn function_arg_ident_local_class_handle_shadow_stays_neutral() {
        let diags = check_with_typedb(&format!(
            "{}\n{}",
            B023_FNS,
            r###"
            void Render() {
                string s = "test";
                UI::InputTextCallbackData@ WrongSig = null;
                UI::InputText("##1", s, UI::InputTextFlags::None, WrongSig);
            }
            "###
        ));
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "local class-handle shadow must stay neutral, got {:?}",
            diags
        );
    }

    /// B023: wrong-sig fn into the UI::NodTree member-callback slot
    /// (NodTreeMemberCallback family, unique arity match).
    #[test]
    fn external_unique_overload_rejects_wrong_signature_nodtree_callback() {
        let diags = check_with_typedb(&format!(
            "{}\n{}",
            B023_FNS,
            r###"
            void OnMember(CMwNod@ nod, Reflection::MwMemberInfo@ info) {}
            void Render() {
                CMwNod@ nod;
                UI::NodTree(nod, WrongSig);
                UI::NodTree(nod, OnMember);
            }
            "###
        ));
        let rejections = diags
            .iter()
            .filter(|d| matches!(d.kind, TypeDiagnosticKind::NoMatchingSignatures { .. }))
            .count();
        assert_eq!(
            rejections, 1,
            "NodTree(WrongSig) must reject, NodTree(OnMember) must bind, got {:?}",
            diags
        );
    }

    /// B023 FP guard (must stay): Named/unknown-shape args — workspace
    /// classes the typedb can't see — never reject a candidate on their
    /// own. The game rejects this call, but the LSP deliberately stays
    /// silent (same conservatism as the GH #47 guard).
    #[test]
    fn external_overload_set_named_unknown_shape_arg_stays_neutral() {
        let diags = check_with_typedb(&format!(
            "{}\n{}",
            B023_FNS,
            r###"
            class Handler { int calls; }
            void Render() {
                string s = "test";
                Handler h;
                UI::InputText("##x", s, UI::InputTextFlags::None, h);
            }
            "###
        ));
        assert!(
            !diags
                .iter()
                .any(|d| d.severity() == TypeDiagnosticSeverity::Error),
            "workspace-class args must stay neutral (FP guard), got {:?}",
            diags
        );
    }

    /// Const cast targets keep the game's rejection: a delegate cannot be
    /// built from a `const` receiver's method. Game ground truth
    /// (RemoteBuild 2026-08-29, OP 1.29.5): ERR `Can't create delegate`.
    #[test]
    fn funcdef_ctor_keeps_flagging_const_cast_receivers() {
        let diags = check_with_typedb(
            r#"
            class N { void Go() {} }
            void Main() {
                N@ h = N();
                startnew(CoroutineFunc(cast<const N@>(h).Go));
            }
            "#,
        );
        assert!(
            diags
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. })),
            "`cast<const N@>(h).Go` must stay flagged, got {:?}",
            diags
        );
    }

    /// Maintenance guard for the engine fence. `is_engine_namespace` is a
    /// STATIC list — deliberately, so the GH #26 note stays stable across
    /// typedb versions — but the GH #58 rule leans on it as the no-typedb
    /// stand-in for "this namespace belongs to the engine". A Core namespace
    /// missing from the list lets a plugin-local `namespace Tests { … }` arm
    /// the rule against game-owned calls — the exact FP that added `Tests`.
    /// Every leading segment the typedb knows must therefore be listed, so a
    /// typedb refresh that introduces a namespace fails HERE rather than as a
    /// false `No matching symbol` in someone's plugin.
    #[test]
    fn engine_namespace_list_covers_every_typedb_namespace() {
        use crate::typedb::TypeIndex;
        let cp = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetCore.json");
        let np = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetNext.json");
        let idx = TypeIndex::load(&cp, &np).expect("typedb fixtures must load");
        let missing: Vec<String> = idx
            .namespaces()
            .into_iter()
            .filter_map(|ns| {
                let leading = ns.split("::").next().unwrap_or(&ns).to_string();
                (!is_engine_namespace(&leading)).then_some(leading)
            })
            .collect();
        assert!(
            missing.is_empty(),
            "typedb namespaces absent from is_engine_namespace: {:?} — add them, \
             or a plugin declaring one of these arms the GH #58 rule against \
             game-owned calls",
            missing
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

    /// Like `check_with_recording`, but with the fixture type databases
    /// loaded — needed for engine receivers.
    fn check_with_recording_typedb(src: &str) -> Checker<'static> {
        use crate::typedb::TypeIndex;
        let cp = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetCore.json");
        let np = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetNext.json");
        let idx: &'static TypeIndex = Box::leak(Box::new(
            TypeIndex::load(&cp, &np).expect("typedb fixtures"),
        ));
        let src: &'static str = Box::leak(src.to_string().into_boxed_str());
        let mut table = SymbolTable::new();
        let fid = table.allocate_file_id();
        let analysis = crate::analysis::DocumentAnalysis::analyze_plain(src);
        let syms = SymbolTable::extract_symbols(fid, analysis.masked_source(), &analysis.file);
        table.set_file_symbols(fid, syms);
        let table: &'static SymbolTable = Box::leak(Box::new(table));
        let scope: &'static GlobalScope<'static> =
            Box::leak(Box::new(GlobalScope::new(table, Some(idx))));
        let mut c = Checker::new(src, scope).with_type_recording();
        c.check_file(&analysis.file);
        c
    }

    /// B059: the type the RECEIVING VARIABLE ends up with, asserted
    /// directly rather than via absence-of-diagnostic. `Error("")` — the
    /// silence sentinel GH #21 returns for the void `MwAddRef` — would
    /// suppress the false positive while typing `entId` wrong, so the
    /// absence assertion alone cannot tell the fix from that regression.
    #[test]
    fn injected_entity_id_infers_uint_on_the_local() {
        let src =
            "void Use(CSmPlayer@ p) { auto entId = p.GetCurrentEntityID(); uint c = entId; }\n";
        let c = check_with_recording_typedb(src);
        // The `entId` READ in `uint c = entId;` — the local's inferred type.
        let read_at = src.rfind("entId").unwrap() as u32;
        let ty = c.type_at_span(read_at).expect("entId read recorded");
        assert_eq!(
            ty,
            &TypeRepr::Primitive(PrimitiveType::Uint),
            "auto entId must infer uint, got {:?}",
            ty
        );
    }

    /// B059 sibling: `GetManialinkPages` returns
    /// `array<CGameManialinkPage@>@` — a HANDLE to an array. The ticket's
    /// `CGameManialinkPage@[]&` spelling came from the stale convert script;
    /// the DLL registration is the authority
    /// (`docs/probes/openplanet-injected-methods.md`).
    #[test]
    fn injected_manialink_pages_infers_page_handle_array() {
        let src = "void Use(CGameCtnNetwork@ n) { auto pages = n.GetManialinkPages(); auto c = pages; }\n";
        let c = check_with_recording_typedb(src);
        let read_at = src.rfind("pages").unwrap() as u32;
        let ty = c.type_at_span(read_at).expect("pages read recorded");
        assert_eq!(
            ty,
            &TypeRepr::Handle(Box::new(TypeRepr::Array(Box::new(TypeRepr::Handle(
                Box::new(TypeRepr::Named("CGameManialinkPage".into()))
            ))))),
            "auto pages must infer array<CGameManialinkPage@>@, got {:?}",
            ty
        );
    }

    /// B059 fence: the void rows on the injected table (GH #21
    /// `MwAddRef`/`MwRelease`) keep the silence sentinel. Promoting them to
    /// `TypeRepr::Void` would change unrelated conversion diagnostics and is
    /// not part of this fix.
    #[test]
    fn injected_void_row_keeps_the_silence_sentinel() {
        let src = "void Use(CMwNod@ n) { auto r = n.MwAddRef(); auto c = r; }\n";
        let c = check_with_recording_typedb(src);
        let read_at = src.rfind(" r;").unwrap() as u32 + 1;
        let ty = c.type_at_span(read_at).expect("r read recorded");
        assert!(
            ty.is_error(),
            "void injected row must stay the silence sentinel, got {:?}",
            ty
        );
    }

    /// B059, the downstream consequence of returning a REAL type: member
    /// completion reads `type_at_offset` at the byte before the `.`
    /// (`src/server/completion.rs`), so indexing the injected call has to
    /// resolve there too. With a silence sentinel this query returns an
    /// error type and `net.GetManialinkPages()[0].` offers nothing.
    #[test]
    fn injected_manialink_pages_element_resolves_for_member_queries() {
        let src =
            "void Use(CGameCtnNetwork@ n) { auto p = n.GetManialinkPages(); auto q = p[0]; }\n";
        let c = check_with_recording_typedb(src);
        // The byte completion would query: the `]` closing the index.
        let bracket = src.rfind("p[0]").unwrap() + 3;
        let ty = c
            .type_at_offset(bracket as u32)
            .expect("indexed element recorded");
        assert_eq!(
            ty,
            &TypeRepr::Handle(Box::new(TypeRepr::Named("CGameManialinkPage".into()))),
            "indexing the injected array must yield CGameManialinkPage@, got {:?}",
            ty
        );
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

    /// Same as `check` but with the fixture typedb loaded — needed for
    /// cases whose receiver types are engine types (`awaitable` etc.).
    fn check_with_typedb(source: &str) -> Vec<TypeDiagnostic> {
        use crate::typedb::TypeIndex;
        let cp = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetCore.json");
        let np = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetNext.json");
        let idx = TypeIndex::load(&cp, &np).expect("typedb fixtures must load");
        let tokens = crate::lexer::tokenize_filtered(source);
        let mut parser = crate::parser::Parser::new(&tokens, source);
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

    /// Same-scope redecl (todo-cover-cases 2026-08-25): game
    /// `'name' is already declared`. Distinct from GH #37d shadowing
    /// (inner block). Open — the checker is silent today.
    fn already_declared(diags: &[TypeDiagnostic]) -> Vec<String> {
        diags
            .iter()
            .filter(|d| d.message().contains("already declared"))
            .map(|d| d.message())
            .collect()
    }

    #[test]
    fn duplicate_local_same_type_errors() {
        let diags = check("void f() { int a = 1; int a = 2; }");
        let hits = already_declared(&diags);
        assert_eq!(hits.len(), 1, "got {diags:?}");
        assert!(hits[0].contains("'a'"), "game names the binding: {hits:?}");
    }

    #[test]
    fn duplicate_local_different_type_errors() {
        let diags = check("void f() { int b = 1; string b = \"x\"; }");
        assert_eq!(already_declared(&diags).len(), 1, "got {diags:?}");
    }

    #[test]
    fn duplicate_local_typed_then_auto_errors() {
        let diags = check("void f() { int c = 1; auto c = 2; }");
        assert_eq!(already_declared(&diags).len(), 1, "got {diags:?}");
    }

    #[test]
    fn duplicate_local_auto_then_typed_errors() {
        let diags = check("void f() { auto d = 1; int d = 2; }");
        assert_eq!(already_declared(&diags).len(), 1, "got {diags:?}");
    }

    #[test]
    fn duplicate_local_const_then_mut_errors() {
        let diags = check("void f() { const int e = 1; int e = 2; }");
        assert_eq!(already_declared(&diags).len(), 1, "got {diags:?}");
    }

    #[test]
    fn duplicate_local_handle_then_value_errors() {
        let diags = check("class T {} void f() { T@ f = T(); T f; }");
        assert_eq!(already_declared(&diags).len(), 1, "got {diags:?}");
    }

    #[test]
    fn duplicate_local_param_then_local_errors() {
        let diags = check("void f(int g) { int g = 2; }");
        assert_eq!(already_declared(&diags).len(), 1, "got {diags:?}");
    }

    #[test]
    fn duplicate_local_ref_in_param_then_local_errors() {
        let diags = check("void f(int &in h) { int h = 2; }");
        assert_eq!(already_declared(&diags).len(), 1, "got {diags:?}");
    }

    #[test]
    fn duplicate_local_ref_out_param_then_local_errors() {
        let diags = check("void f(int &out i) { int i = 2; }");
        assert_eq!(already_declared(&diags).len(), 1, "got {diags:?}");
    }

    #[test]
    fn duplicate_local_handle_in_param_then_local_errors() {
        let diags = check("class T {} void f(T@ &in j) { T@ j; }");
        assert_eq!(already_declared(&diags).len(), 1, "got {diags:?}");
    }

    #[test]
    fn duplicate_local_multi_declarator_errors() {
        let diags = check("void f() { int k = 1, k = 2; }");
        assert_eq!(already_declared(&diags).len(), 1, "got {diags:?}");
    }

    #[test]
    fn duplicate_local_handle_out_param_then_local_errors() {
        let diags = check("class T {} void f(T@ &out p) { T@ p; }");
        assert_eq!(already_declared(&diags).len(), 1, "got {diags:?}");
    }

    #[test]
    fn duplicate_local_const_handle_then_handle_errors() {
        let diags = check("class T {} void f() { const T@ q = T(); T@ q; }");
        assert_eq!(already_declared(&diags).len(), 1, "got {diags:?}");
    }

    /// Motivating cascade: after `auto @cp0 = Zone()` fails as a redecl,
    /// `cp0` keeps `CpSpec@` (no `index`).
    #[test]
    fn duplicate_local_keeps_first_type_for_member_access() {
        let src = r#"
class CpSpec { int dummy; }
class Zone { int index; }
void f() {
    CpSpec@ cp0 = CpSpec();
    auto @cp0 = Zone();
    int n = cp0.index;
}
"#;
        let diags = check(src);
        assert_eq!(
            already_declared(&diags).len(),
            1,
            "redecl must error, got {diags:?}"
        );
        let member_miss = diags.iter().any(|d| {
            matches!(
                &d.kind,
                TypeDiagnosticKind::UndefinedMember {
                    object_type,
                    member
                } if object_type.contains("CpSpec") && member == "index"
            )
        });
        assert!(
            member_miss,
            "cp0.index must resolve against the first type CpSpec; got {diags:?}"
        );
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

    // === Const-receiver non-const-method calls (game parity, 2026-08-23) ===
    //
    // Game evidence (spike-live-add-kinematic-ao, HatDump.as:60):
    //   class SceneHandle { bool PayloadValid() { return true; } }
    //   Json::Value HandleJson(const SceneHandle &in h) { h.PayloadValid(); }
    //   → ERR : No matching signatures to 'SceneHandle::PayloadValid() const'
    //
    // Variant matrix per AGENTS.md: fire on const-ref receiver + non-const
    // method; stay silent for every other receiver/method combination.

    /// RED-1: the reported case — non-const method on `const T &in`.
    #[test]
    fn nonconst_method_on_const_ref_param_fires() {
        let diags = check(
            "class C { bool M() { return true; } } \
             void f(const C &in h) { h.M(); }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::NonConstMethodOnConstReceiver { .. }
                )
            })
            .collect();
        assert_eq!(bad.len(), 1, "expected 1 diagnostic, got {:?}", diags);
    }

    /// Const method on const receiver: legal, must stay silent.
    #[test]
    fn const_method_on_const_ref_param_ok() {
        let diags = check(
            "class C { bool M() const { return true; } } \
             void f(const C &in h) { h.M(); }",
        );
        assert!(diags.is_empty(), "expected no diagnostics, got {:?}", diags);
    }

    /// Non-const method on a plain (non-const) reference: legal.
    #[test]
    fn nonconst_method_on_plain_ref_param_ok() {
        let diags = check(
            "class C { bool M() { return true; } } \
             void f(C &in h) { h.M(); }",
        );
        assert!(diags.is_empty(), "expected no diagnostics, got {:?}", diags);
    }

    /// Non-const method on a value parameter `T h`: legal (caller copy).
    #[test]
    fn nonconst_method_on_value_param_ok() {
        let diags = check(
            "class C { bool M() { return true; } } \
             void f(C h) { h.M(); }",
        );
        assert!(diags.is_empty(), "expected no diagnostics, got {:?}", diags);
    }

    /// `T@` handle receiver (non-const pointee): rebindable handle, the
    /// object itself is not const — legal.
    #[test]
    fn nonconst_method_on_handle_param_ok() {
        let diags = check(
            "class C { bool M() { return true; } } \
             void f(C@ h) { h.M(); }",
        );
        assert!(diags.is_empty(), "expected no diagnostics, got {:?}", diags);
    }

    /// `const T@ &in` — handle to a CONST pointee: the object contents
    /// are const, so a non-const method call must fire (same rule as
    /// `const T &in`).
    #[test]
    fn nonconst_method_on_const_handle_param_fires() {
        let diags = check(
            "class C { bool M() { return true; } } \
             void f(const C@ &in h) { h.M(); }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::NonConstMethodOnConstReceiver { .. }
                )
            })
            .collect();
        assert_eq!(bad.len(), 1, "expected 1 diagnostic, got {:?}", diags);
    }

    /// Local `const T` local variable receiver: contents are const.
    #[test]
    fn nonconst_method_on_const_local_fires() {
        let diags = check(
            "class C { bool M() { return true; } } \
             void f() { const C c = C(); c.M(); }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::NonConstMethodOnConstReceiver { .. }
                )
            })
            .collect();
        assert_eq!(bad.len(), 1, "expected 1 diagnostic, got {:?}", diags);
    }

    /// Field access on a const receiver is NOT a method call — the
    /// existing ConstViolation machinery handles writes; reads are legal.
    #[test]
    fn field_read_on_const_receiver_ok() {
        let diags = check(
            "class C { int x; } \
             void f(const C &in h) { int y = h.x; }",
        );
        assert!(diags.is_empty(), "expected no diagnostics, got {:?}", diags);
    }

    /// Inherited non-const method through a const receiver of the PARENT
    /// type: parent class is in the same file — the rule must fire there
    /// too (same-file parent, resolved qualname).
    #[test]
    fn inherited_nonconst_method_const_receiver_fires() {
        let diags = check(
            "class B { bool M() { return true; } } \
             class D : B {} \
             void f(const B &in h) { h.M(); }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::NonConstMethodOnConstReceiver { .. }
                )
            })
            .collect();
        assert_eq!(bad.len(), 1, "expected 1 diagnostic, got {:?}", diags);
    }

    /// A receiver whose class is in NO file and NOT in the pool: silent.
    ///
    /// This test used to be named `cross_file_method_const_receiver_silent`
    /// and was read as documenting deliberate cross-file silence. It never
    /// tested that. `check` builds a workspace holding only this source, so
    /// `C` is absent from the pooled table as well — the case measured here
    /// is an UNKNOWN type, not a cross-file one. B116 fixed the real
    /// cross-file case (see `b116_cross_file_const_receiver_tests`); this
    /// one keeps its own, still-correct contract: with nothing known about
    /// `C` anywhere, the rule must stay quiet.
    #[test]
    fn unknown_class_const_receiver_silent() {
        let diags = check("void f(const C &in h) { h.M(); }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::NonConstMethodOnConstReceiver { .. }
                )
            })
            .collect();
        assert!(
            bad.is_empty(),
            "expected no NonConstMethodOnConstReceiver, got {:?}",
            diags
        );
    }

    // === GAP 1: unknown member on a parentless workspace class ===
    //
    // Game evidence (2026-08-23, spike-live-add-kinematic-ao): the game
    // rejects `ctx.AssertSame(...)` on a user class with no such member.
    // The LSP stays silent. Fire only for classes with NO base classes:
    // a parent we can't resolve could carry the member (FP-hostile),
    // and mixin consumers list the mixin in their bases (GH #46).

    /// RED-1: unknown member on a parentless user class fires.
    #[test]
    fn unknown_member_on_parentless_workspace_class_fires() {
        let diags = check(
            "class Ctx { int x; } \
             void f() { Ctx@ c = Ctx(); int y = c.bogus; }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::UndefinedMember { object_type, member } if object_type == "Ctx" && member == "bogus"
                )
            })
            .collect();
        assert_eq!(bad.len(), 1, "expected 1 UndefinedMember, got {:?}", diags);
    }

    /// Known member on the same class: silent (negative case).
    #[test]
    fn known_member_on_workspace_class_silent() {
        let diags = check(
            "class Ctx { int x; } \
             void f() { Ctx@ c = Ctx(); int y = c.x; }",
        );
        assert!(diags.is_empty(), "expected no diagnostics, got {:?}", diags);
    }

    // === Virtual-property accessors on parentless classes (FP fix,
    // 2026-08-23 review pass) ===
    //
    // AngelScript's get_/set_ convention: `obj.prop` is legal when the
    // class declares `T get_prop()` and/or `set_prop(T v)`. The
    // UndefinedMember gate must honor it — real-world FPs: E++
    // ClickFlags.get_RMB → .RMB, GameQuat.get_f0 → .f0, AABB.get_pos →
    // .pos; dips++ DonoCheerCount.get_TotalAmt → .TotalAmt (16 errors
    // across the two fixture plugins).

    /// RED: getter accessor satisfies a member read.
    #[test]
    fn getter_accessor_satisfies_member_read() {
        let diags = check(
            "class C { int flags; bool get_RMB() { return true; } } \
             void f(const C &in c) { bool b = c.RMB; }",
        );
        assert!(
            diags.is_empty(),
            "get_RMB must satisfy .RMB, got {:?}",
            diags
        );
    }

    /// RED: setter accessor satisfies a member write/read reference.
    #[test]
    fn setter_accessor_satisfies_member_reference() {
        let diags = check(
            "class C { float q; void set_f0(float v) { q = v; } } \
             void f(C@ c) { c.f0 = 1.0; }",
        );
        assert!(diags.is_empty(), "set_f0 must satisfy .f0, got {:?}", diags);
    }

    /// Getter + setter pair (the GameQuat.f0 shape): read and write both
    /// satisfied.
    #[test]
    fn getter_setter_pair_satisfies_member() {
        let diags = check(
            "class Q { float q; float get_f0() const { return q; } void set_f0(float v) { q = v; } } \
             void f(Q@ q) { float a = q.f0; q.f0 = a; }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(
                |d| matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { member, .. } if member == "f0"),
            )
            .collect();
        assert!(
            bad.is_empty(),
            "get/set pair must satisfy .f0, got {:?}",
            diags
        );
    }

    /// Accessors on the receiver class still fire for a DIFFERENT,
    /// genuinely-missing member — the fix must not blanket-silence.
    #[test]
    fn accessor_present_but_other_member_still_fires() {
        let diags = check(
            "class C { int flags; bool get_RMB() { return true; } } \
             void f(const C &in c) { bool b = c.Nope; }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::UndefinedMember { object_type, member } if object_type == "C" && member == "Nope"
                )
            })
            .collect();
        assert_eq!(bad.len(), 1, "Nope must still fire, got {:?}", diags);
    }

    /// Method-call form `c.RMB()` invokes the non-const getter directly —
    /// const-receiver rule still applies (game parity: direct accessor
    /// call = normal method call). The property READ form stays silent
    /// via the property registration; this variant pins the call form.
    #[test]
    fn accessor_only_method_call_on_const_receiver_fires() {
        let diags = check(
            "class C { int flags; bool get_RMB() { return true; } } \
             void f(const C &in c) { c.RMB(); }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::NonConstMethodOnConstReceiver { method, object_type } if method == "RMB" && object_type == "C"
                )
            })
            .collect();
        assert_eq!(
            bad.len(),
            1,
            "const receiver + non-const accessor call must fire, got {:?}",
            diags
        );
    }

    /// this.bogus inside a method of a parentless class: same rule,
    /// receiver resolves to the enclosing class.
    #[test]
    fn this_unknown_member_on_parentless_class_fires() {
        let diags = check("class C { int x; void m() { int y = this.bogus; } }");
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::UndefinedMember { object_type, member } if object_type == "C" && member == "bogus"
                )
            })
            .collect();
        assert_eq!(bad.len(), 1, "expected 1 UndefinedMember, got {:?}", diags);
    }

    /// A class WITH an (unresolvable) base class stays silent — the base
    /// may carry the member cross-file.
    #[test]
    fn unknown_member_on_derived_class_stays_silent() {
        let diags = check(
            "class D : Base { int x; } \
             void f() { D@ d = D(); int y = d.bogus; }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "derived-class miss must stay silent, got {:?}",
            diags
        );
    }

    /// A mixin's members must not be mis-flagged on the consuming class.
    /// (The `UnknownType("Base")` on the mixin base name itself is
    /// pre-existing single-file behavior — mixins aren't indexed as
    /// classes — and is out of scope here.)
    #[test]
    fn mixin_consumer_members_not_flagged() {
        let diags = check(
            "mixin Base { int mval; } \
             class D : Base { int x; } \
             void f() { D@ d = D(); int y = d.mval; }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::UndefinedMember { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "mixin member access must stay silent, got {:?}",
            diags
        );
    }

    // === GAP 2: awaitable.WithRunContext is a removed API ===
    //
    // Game evidence (2026-08-23, DipsItemExperiment/MLFeedRaceData):
    // `.WithRunContext()` on an awaitable compiles clean but logs
    // "[ScriptRuntime] [ERROR] DEPRECATED: awaitable.WithRunContext() is
    // no longer valid. Use Meta::StartWithRunContext instead." — the run
    // context is silently discarded. Same family as GH #27/#28
    // (removed-API), detectable syntactically on the member path.

    /// RED-2: `.WithRunContext(` on an `awaitable` receiver errors with
    /// the replacement hint.
    #[test]
    fn awaitable_with_run_context_removed_api_fires() {
        let diags = check_with_typedb(
            r#"
            void Worker() {}
            void Main() { startnew(Worker).WithRunContext(Meta::RunContext::AfterMainLoop); }
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::RemovedApiUsage { member, hint, .. }
                        if member == "WithRunContext" && hint.contains("Meta::StartWithRunContext")
                )
            })
            .collect();
        assert_eq!(bad.len(), 1, "expected 1 RemovedApiUsage, got {:?}", diags);
    }

    /// B036: the replacement an editor needs is DATA, not prose. `hint` is
    /// the human sentence; a quick fix must not re-parse English out of it,
    /// so the variant carries a structured `replacement` beside it.
    #[test]
    fn removed_api_usage_carries_a_structured_replacement_field() {
        let diags = check_with_typedb(
            r#"
            void Worker() {}
            void Main() { startnew(Worker).WithRunContext(Meta::RunContext::AfterMainLoop); }
            "#,
        );
        let found = diags.iter().find_map(|d| match &d.kind {
            TypeDiagnosticKind::RemovedApiUsage { replacement, .. } => Some(replacement.clone()),
            _ => None,
        });
        // Deliberately `None` for this member: `Meta::StartWithRunContext`
        // is a FREE function that takes the coroutine as an argument, so
        // `startnew(W).WithRunContext(ctx)` cannot be migrated by swapping
        // the member span — the receiver has to move into the call. A
        // structured replacement here would produce
        // `Meta::StartWithRunContext(ctx)` and silently drop the coroutine.
        assert_eq!(
            found,
            Some(None),
            "expected the awaitable finding to carry `replacement: None`, got {:?}",
            diags
        );
    }

    /// Other awaitable members must NOT fire (the type still exists and
    /// has real methods — e.g. `Then`/`cancelling` usage stays legal).
    #[test]
    fn awaitable_other_members_stay_silent() {
        let diags = check_with_typedb(
            r#"
            void Worker() {}
            void Main() { startnew(Worker).Then(Other); }
            void Other() {}
            "#,
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::RemovedApiUsage { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "expected no RemovedApiUsage for non-removed members, got {:?}",
            diags
        );
    }

    /// A user class with its own `WithRunContext` method is NOT the
    /// removed API — member name alone must not fire.
    #[test]
    fn user_class_with_run_context_method_silent() {
        let diags = check(
            "class C { void WithRunContext(int x) { } } \
             void f() { C@ c = C(); c.WithRunContext(3); }",
        );
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| matches!(&d.kind, TypeDiagnosticKind::RemovedApiUsage { .. }))
            .collect();
        assert!(
            bad.is_empty(),
            "user-class WithRunContext must stay silent, got {:?}",
            diags
        );
    }
    // ── B073 / GH #38: a workspace type shadows an engine funcdef ───────────

    /// Every POSITION a type name can be written in, as one source per row.
    /// `{T}` is substituted with the type name; the body always reaches a
    /// member the type does not have, so a correctly resolved receiver
    /// produces exactly one `UndefinedMember`.
    ///
    /// The receiver is what is under test, not the member: if the declared
    /// type resolves to the engine funcdef instead of the plugin's own type,
    /// the member check goes neutral and the diagnostic vanishes. A LOST true
    /// positive is the readable signal here; the ctor row below carries the
    /// false-positive half.
    const B073_POSITIONS: [(&str, &str); 6] = [
        ("local", "void f() { {T}@ c; print(\"\" + c.zzzNoSuch); }\n"),
        ("parameter", "void g({T}@ p) { print(\"\" + p.zzzNoSuch); }\n"),
        (
            "return type",
            "{T}@ h() { return null; }\nvoid f() { print(\"\" + h().zzzNoSuch); }\n",
        ),
        (
            "member field",
            "class ZZZHolder { {T}@ fld; }\nvoid f() { ZZZHolder@ h; print(\"\" + h.fld.zzzNoSuch); }\n",
        ),
        (
            "array element",
            "void f() { array<{T}@> a; print(\"\" + a[0].zzzNoSuch); }\n",
        ),
        (
            "array shorthand element",
            "void f() { {T}@[] a; print(\"\" + a[0].zzzNoSuch); }\n",
        ),
    ];

    /// B073 / GH #38: a plugin type named after a Core funcdef must behave in
    /// every position exactly like a plugin type named after nothing.
    ///
    /// Each colliding row is paired with a NON-COLLIDING control of the same
    /// shape in the same position, and the two are required to agree. That
    /// pairing is what makes the sweep meaningful: it asserts a relation
    /// between two runs rather than a hardcoded diagnostic list, so a row
    /// cannot pass by both sides going silent for an unrelated reason — the
    /// control would go silent too and the assert would still hold only if
    /// the control's own floor (below) is met.
    #[test]
    fn a_plugin_type_named_after_an_engine_funcdef_behaves_the_same_in_every_position() {
        let mut wrong: Vec<String> = Vec::new();
        let mut control_diags = 0usize;

        for (shape, decl) in [
            ("class", "class {T} { int v; }\n"),
            ("interface", "interface {T} { int Get(); }\n"),
        ] {
            for (position, body) in B073_POSITIONS {
                let build = |name: &str| {
                    format!("{}{}", decl.replace("{T}", name), body.replace("{T}", name))
                };
                let collided = check_with_typedb(&build("CoroutineFunc"));
                let control = check_with_typedb(&build("ZZZWidget"));
                control_diags += usize::from(!control.is_empty());

                let render = |d: &[TypeDiagnostic]| {
                    d.iter()
                        .map(|x| format!("{:?}", x.kind).replace("ZZZWidget", "<T>"))
                        .collect::<Vec<_>>()
                };
                let (a, b) = (render(&collided), render(&control));
                let a: Vec<String> = a
                    .iter()
                    .map(|s| s.replace("CoroutineFunc", "<T>"))
                    .collect();
                if a != b {
                    wrong.push(format!(
                        "{shape} in {position} position:\n  named after a funcdef: {a:?}\
                         \n  named after nothing:   {b:?}"
                    ));
                }
            }
        }

        // Anti-vacuity: enough control rows must actually be diagnosing
        // something, or "the two agree" is satisfied by two empty lists.
        // The floor is one full pass over the positions, which the `class`
        // rows supply — an `interface` has no members to miss, so those rows
        // are legitimately silent on both sides and are not counted on.
        assert!(
            control_diags >= B073_POSITIONS.len(),
            "only {control_diags} of the non-colliding control rows diagnosed \
             anything — the comparison above would be vacuous"
        );
        assert!(
            wrong.is_empty(),
            "{} rows differ: {:#?}",
            wrong.len(),
            wrong
        );
    }

    /// The false-positive half, which is the damaging direction: constructing
    /// the plugin's own type was read as constructing the ENGINE funcdef, so
    /// `CoroutineFunc()` was rejected for taking 0 of 1 arguments — a
    /// declaration the game accepts.
    #[test]
    fn constructing_a_plugin_type_named_after_an_engine_funcdef_is_not_an_arity_error() {
        for (name, decl) in [
            ("CoroutineFunc", "class CoroutineFunc { int v; }\n"),
            (
                "CoroutineFuncUserdataString",
                "class CoroutineFuncUserdataString { int v; }\n",
            ),
        ] {
            let src = format!("{decl}void f() {{ {name}@ c = {name}(); }}\n");
            let diags = check_with_typedb(&src);
            assert!(
                diags.is_empty(),
                "`{name}()` constructs the plugin's own class, not the engine \
                 funcdef: {diags:?}"
            );
        }

        // The engine funcdef ctor still has to be checked when no plugin type
        // shadows it, or the row above passes by the check being gone.
        let diags = check_with_typedb("void f() { auto c = CoroutineFunc(); }\n");
        assert!(
            !diags.is_empty(),
            "with nothing shadowing it, the engine `CoroutineFunc` funcdef ctor \
             is still arity-checked"
        );
    }

    /// The namespace axis at the checker level: the declaration is
    /// `namespace Nz { class CoroutineFunc … }` and the reference inside it is
    /// written BARE, so an exact-name guard misses it however many shapes it
    /// enumerates. The diagnostic must name the qualified type, exactly as the
    /// non-colliding control does.
    #[test]
    fn a_namespaced_plugin_type_named_after_an_engine_funcdef_keeps_its_qualifier() {
        for (name, expected) in [
            ("CoroutineFunc", "Nz::CoroutineFunc"),
            ("ZZZWidget", "Nz::ZZZWidget"),
        ] {
            let src = format!(
                "namespace Nz {{ class {name} {{ int v; }} \
                 void f() {{ {name}@ c; print(\"\" + c.zzzNoSuch); }} }}\n"
            );
            let diags = check_with_typedb(&src);
            let members: Vec<String> = diags
                .iter()
                .filter_map(|d| match &d.kind {
                    TypeDiagnosticKind::UndefinedMember { object_type, .. } => {
                        Some(object_type.clone())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                members,
                vec![expected.to_string()],
                "`{name}` inside `namespace Nz`: {diags:?}"
            );
        }
    }

    /// The over-blocking edge, which is the direction a shadowing fix breaks
    /// things in: with no plugin type of that name, the engine funcdef must
    /// still construct, still bind a matching function, and still REJECT a
    /// mismatched one. B060 paid four review rounds on exactly this edge.
    #[test]
    fn an_engine_funcdef_is_untouched_when_no_plugin_type_shadows_it() {
        let clean = check_with_typedb("void W() {}\nvoid f() { startnew(CoroutineFunc(W)); }\n");
        assert!(
            clean.is_empty(),
            "a matching function still binds the engine funcdef: {clean:?}"
        );

        let mismatched =
            check_with_typedb("void W(int a) {}\nvoid f() { startnew(CoroutineFunc(W)); }\n");
        assert!(
            mismatched
                .iter()
                .any(|d| matches!(d.kind, TypeDiagnosticKind::ArgTypeMismatch { .. })),
            "a mismatched signature is still rejected: {mismatched:?}"
        );
    }

    /// The same edge one level along: a plugin type named after an engine
    /// funcdef must behave at a CALL SLOT exactly like one named after
    /// nothing — neither newly accepted nor newly rejected. Both the engine
    /// slot (`startnew`) and a workspace slot whose parameter is written with
    /// the colliding name are checked, because the two reach the funcdef
    /// through different predicates.
    ///
    /// IF YOU LANDED HERE FROM A FAILURE, this is what it means. Two more
    /// sites ask `lookup_funcdef` without the GH #38 shadowing guard that
    /// [`Self::try_funcdef_ctor`] carries — [`Self::funcdef_name_of`] and
    /// [`Self::slot_funcdef`] — and they were UNGUARDED DELIBERATELY. Measured
    /// when B073 landed: with a plugin type named after a Core funcdef, both
    /// produced output identical to the non-colliding control, so a guard there
    /// could not fire. A guard that cannot fire is worse than none, because at
    /// review time it is indistinguishable from one that can, and it tells the
    /// next reader an axis is covered when nothing is checking it.
    ///
    /// THAT ARGUMENT HAS SINCE EXPIRED FOR ONE OF THEM, exactly as this test
    /// was built to detect. B089 ran the funcdef-slot check over workspace
    /// call sites — the site the reachability argument said did not exist —
    /// and this test went red on the `void g({T}@ p)` row, reporting
    /// `NoMatchingSignatures` where the control was silent. The guard now
    /// lives in [`Self::slot_funcdef_in_scope`], which is the only caller
    /// that can reach it while shadowing; `slot_funcdef` and
    /// `funcdef_name_of` stay unguarded on the original argument, which
    /// still holds for their callers.
    ///
    /// This test is the substitute for those guards. It goes red the moment
    /// either site becomes reachable-and-shadowing, which forces that call to
    /// be made deliberately instead of discovered. So a failure here is not a
    /// broken assertion to relax — it is the news that the reachability
    /// argument above has expired, and the fix is to guard the site that
    /// changed.
    #[test]
    fn a_plugin_type_named_after_an_engine_funcdef_behaves_the_same_at_a_call_slot() {
        for body in [
            "void f() { {T}@ c; startnew(c); }\n",
            "void g({T}@ p) {}\nvoid f() { {T}@ c; g(c); }\n",
        ] {
            let build =
                |name: &str| format!("class {name} {{ int v; }}\n{}", body.replace("{T}", name));
            let collided = check_with_typedb(&build("CoroutineFunc"));
            let control = check_with_typedb(&build("ZZZWidget"));
            let render = |d: &[TypeDiagnostic], name: &str| {
                d.iter()
                    .map(|x| format!("{:?}", x.kind).replace(name, "<T>"))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                render(&collided, "CoroutineFunc"),
                render(&control, "ZZZWidget"),
                "call-slot behaviour diverges for `{body}`"
            );
        }
    }
    // ── B098: a bare short name must not rescue a namespaced engine enum ────

    /// Check `source` against a scope built ONCE by the caller. `check_with_typedb`
    /// reloads the type index per call, which is fine for a single fixture and
    /// costs minutes over a 436-row sweep.
    fn check_against(scope: &GlobalScope, source: &str) -> Vec<TypeDiagnostic> {
        let tokens = crate::lexer::tokenize_filtered(source);
        let mut parser = crate::parser::Parser::new(&tokens, source);
        let file = parser.parse_file();
        let mut checker = Checker::new(source, scope);
        checker.check_file(&file);
        checker.diagnostics
    }

    /// `(bare ACCEPTED, bare REJECTED)` spellings from the committed B052
    /// corpus — the game's own verdict on each name written as a TYPE.
    fn b052_bare_verdicts() -> (Vec<String>, Vec<String>) {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("docs/probes/b052-qualified-enum-spellings.tsv");
        let text = std::fs::read_to_string(&path).expect("the probe corpus must be committed");
        let (mut acc, mut rej) = (Vec::new(), Vec::new());
        for line in text.lines() {
            if line.starts_with('#') || line.is_empty() || line.starts_with("spelling\t") {
                continue;
            }
            let mut c = line.split('\t');
            let spelling = c.next().expect("a spelling column");
            let verdict = c.next().unwrap_or("");
            if spelling.contains("::") {
                continue;
            }
            match verdict {
                "ACCEPTED" => acc.push(spelling.to_string()),
                "REJECTED" => rej.push(spelling.to_string()),
                _ => {}
            }
        }
        (acc, rej)
    }

    /// B098. A bare short name reached a NAMESPACED engine enum through
    /// `external_enum_by_short_name`, so `Axis(1)` resolved to the Core enum
    /// `UI::Plot::Axis`. The game answers
    /// `Identifier 'Axis' is not a data type in global namespace`.
    ///
    /// Driven over the whole committed corpus rather than one spelling,
    /// because the blast radius is the point: **436** bare names the game
    /// rejects, and 62 it accepts, all measured in the same run so the fix
    /// cannot buy the rejections by breaking the acceptances.
    ///
    /// B056 closed this same class in `resolve_unqualified`, which now
    /// deliberately excludes enums from its short-name rescue. This is the
    /// second call site that fix did not reach — the third instance in this
    /// batch of a rule living at one of N sites (B076, B073).
    #[test]
    fn every_bare_spelling_the_game_rejects_is_rejected_in_call_position() {
        let index = typedb_fixture_index_checker();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));
        let (accepted, rejected) = b052_bare_verdicts();

        assert_eq!(
            rejected.len(),
            436,
            "the corpus's bare REJECTED set changed size"
        );
        assert_eq!(
            accepted.len(),
            62,
            "the corpus's bare ACCEPTED set changed size"
        );

        let mut wrongly_silent = Vec::new();
        for name in &rejected {
            if check_against(&scope, &format!("void zzzf() {{ {name}(1); }}\n")).is_empty() {
                wrongly_silent.push(name.clone());
            }
        }
        assert!(
            wrongly_silent.is_empty(),
            "{} bare spellings the game REJECTS are silent in call position: {:?}",
            wrongly_silent.len(),
            &wrongly_silent[..wrongly_silent.len().min(20)]
        );

        let mut wrongly_loud = Vec::new();
        for name in &accepted {
            let d = check_against(&scope, &format!("void zzzf() {{ {name}(1); }}\n"));
            if !d.is_empty() {
                wrongly_loud.push(format!("{name}: {:?}", d[0].kind));
            }
        }
        assert!(
            wrongly_loud.is_empty(),
            "{} bare spellings the game ACCEPTS became errors: {:?}",
            wrongly_loud.len(),
            &wrongly_loud[..wrongly_loud.len().min(20)]
        );
    }

    /// The internal-consistency half, and the strongest argument for the change
    /// that needs no game at all: this crate ALREADY rejects all 436 in VALUE
    /// position. Measured, not assumed — 0 of 436 were silent there.
    ///
    /// So call position was the outlier against the crate's OWN behaviour, and
    /// the fix narrows toward what value position has always done rather than
    /// inventing a rule. Asserting the two agree is what keeps them from
    /// drifting apart again.
    ///
    /// Scoped to the names the game REJECTS, and the exclusion is not a
    /// convenience. Call and value position legitimately differ for a name the
    /// game ACCEPTS: `CGameCtnApp` is a class, so `CGameCtnApp(1)` is a
    /// constructor while `"" + CGameCtnApp` is a type name used as a value and
    /// should error. Demanding parity across the accepted set would assert
    /// something untrue and would have to be relaxed by the first person to
    /// read it. Those 62 are covered by the sweep above, which requires them
    /// silent in call position.
    #[test]
    fn call_position_and_value_position_agree_on_every_rejected_bare_spelling() {
        let index = typedb_fixture_index_checker();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));
        let (_accepted, rejected) = b052_bare_verdicts();

        let mut disagree = Vec::new();
        let mut diagnosed_somewhere = 0usize;
        for name in rejected.iter() {
            let call = check_against(&scope, &format!("void zzzf() {{ {name}(1); }}\n"));
            let value = check_against(
                &scope,
                &format!("void zzzf() {{ print(\"\" + {name}); }}\n"),
            );
            diagnosed_somewhere += usize::from(!call.is_empty() || !value.is_empty());
            if call.is_empty() != value.is_empty() {
                disagree.push(format!(
                    "{name}: call {} / value {}",
                    if call.is_empty() { "silent" } else { "errors" },
                    if value.is_empty() { "silent" } else { "errors" },
                ));
            }
        }
        assert!(
            diagnosed_somewhere >= 400,
            "anti-vacuity: only {diagnosed_somewhere} names diagnosed in either \
             position, so 'the two agree' would be near-vacuous"
        );
        assert!(
            disagree.is_empty(),
            "{} bare spellings resolve differently in call and value position: {:?}",
            disagree.len(),
            &disagree[..disagree.len().min(20)]
        );
    }

    /// The shapes, per CLAUDE.md — the 436 are not homogeneous, and a fix that
    /// happened to catch only one kind of engine enum would satisfy a sweep
    /// while leaving the others. Each row is paired with the SILENCE that must
    /// survive it, so the test cannot pass by rejecting everything.
    #[test]
    fn a_bare_engine_enum_needs_its_qualifier_in_every_shape() {
        let index = typedb_fixture_index_checker();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&index));

        // (shape, the bare spelling the game refuses, the qualified spelling it takes)
        for (shape, bare, qualified) in [
            (
                "a Core enum nested in a namespace",
                "Axis",
                "UI::Plot::Axis",
            ),
            ("a Core enum in a one-level namespace", "Cond", "UI::Cond"),
            (
                "a Core enum under a nested namespace",
                "Align",
                "nvg::Align",
            ),
            (
                "a Nadeo enum under a class scope",
                "EMenuOrder",
                "CGameMenu::EMenuOrder",
            ),
        ] {
            for form in ["{N}(1)", "print(\"\" + {N})"] {
                let src = format!("void zzzf() {{ {}; }}\n", form.replace("{N}", bare));
                assert!(
                    !check_against(&scope, &src).is_empty(),
                    "{shape}: bare `{bare}` must not resolve — the game answers \
                     `Identifier '{bare}' is not a data type in global namespace`"
                );
            }
            let src = format!("void zzzf() {{ {qualified}(1); }}\n");
            assert!(
                check_against(&scope, &src).is_empty(),
                "{shape}: the QUALIFIED spelling `{qualified}` must still work — \
                 the game takes it, and a fix that broke it would satisfy every \
                 rejection row above: {:?}",
                check_against(&scope, &src)
            );
        }

        // A genuinely GLOBAL engine enum is the control in the other direction:
        // its source name IS its bare name, the game accepts it bare, and it
        // must keep resolving in both positions.
        for form in ["VirtualKey(1)", "print(\"\" + VirtualKey)"] {
            let src = format!("void zzzf() {{ {form}; }}\n");
            assert!(
                check_against(&scope, &src).is_empty(),
                "a globally visible engine enum stays usable bare: {:?}",
                check_against(&scope, &src)
            );
        }
    }

    /// A workspace declaration of the same short name still wins, in both
    /// positions. The workspace is the one authority a bare name legitimately
    /// resolves against, and narrowing the ENGINE rescue must not touch it.
    #[test]
    fn a_workspace_declaration_of_the_short_name_still_resolves_bare() {
        let index = typedb_fixture_index_checker();
        for decl in [
            "enum Axis { X, Y }\n",
            "class Axis { int v; }\n",
            "funcdef void Axis();\n",
        ] {
            let src = format!("{decl}void zzzf() {{ Axis@ a; }}\n");
            let tokens = crate::lexer::tokenize_filtered(&src);
            let mut parser = crate::parser::Parser::new(&tokens, &src);
            let file = parser.parse_file();
            let mut ws = SymbolTable::new();
            let fid = ws.allocate_file_id();
            let syms = SymbolTable::extract_symbols(fid, &src, &file);
            ws.set_file_symbols(fid, syms);
            let scope = GlobalScope::new(&ws, Some(&index));
            let mut checker = Checker::new(&src, &scope);
            checker.check_file(&file);
            assert!(
                checker.diagnostics.is_empty(),
                "a workspace `{}` named `Axis` must resolve bare: {:?}",
                decl.split_whitespace().next().unwrap(),
                checker.diagnostics
            );
        }
    }

    /// The shipped type-DB fixtures, loaded once per test that needs them.
    fn typedb_fixture_index_checker() -> crate::typedb::TypeIndex {
        use crate::typedb::TypeIndex;
        let base =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
        TypeIndex::load(
            &base.join("OpenplanetCore.json"),
            &base.join("OpenplanetNext.json"),
        )
        .expect("typedb fixtures must load")
    }
}

// ── B116: NonConstMethodOnConstReceiver across a file boundary ──────────────
//
// The rule's sole emit site used to sit inside the
// `if let Some((_, members)) = self.file_classes.get(&type_name)` arm, and
// `file_classes` is built by `index_file_classes`, which walks ONE document.
// So a const receiver whose class lived in another file was silently
// unchecked — the gate was `file_classes`, never the const set.
//
// Every case below is stated TWICE: once same-file (the control, which
// already passed) and once split across files (the ticket). Keeping the pair
// adjacent is the point — it is what stops the two from being conflated
// again, in either direction.
#[cfg(test)]
mod b116_cross_file_const_receiver_tests {
    use super::*;
    use crate::lexer::tokenize_filtered;
    use crate::parser::Parser;
    use crate::symbols::SymbolTable;

    /// Check `main` against a workspace that also holds every `sibling`
    /// under its own file id. Diagnostics are those of `main` only, so a
    /// sibling's own body can never contribute to the count.
    fn check_ws(
        main: &str,
        siblings: &[&str],
        typedb: Option<&crate::typedb::TypeIndex>,
    ) -> Vec<TypeDiagnostic> {
        let mut ws = SymbolTable::new();
        for sibling in siblings {
            let tokens = tokenize_filtered(sibling);
            let mut parser = Parser::new(&tokens, sibling);
            let file = parser.parse_file();
            let fid = ws.allocate_file_id();
            let syms = SymbolTable::extract_symbols(fid, sibling, &file);
            ws.set_file_symbols(fid, syms);
        }
        let tokens = tokenize_filtered(main);
        let mut parser = Parser::new(&tokens, main);
        let file = parser.parse_file();
        let fid = ws.allocate_file_id();
        let syms = SymbolTable::extract_symbols(fid, main, &file);
        ws.set_file_symbols(fid, syms);

        let scope = GlobalScope::new(&ws, typedb);
        let mut checker = Checker::new(main, &scope);
        checker.check_file(&file);
        checker.diagnostics
    }

    /// How many `NonConstMethodOnConstReceiver` the rule produced.
    fn fired(diags: &[TypeDiagnostic]) -> usize {
        diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::NonConstMethodOnConstReceiver { .. }
                )
            })
            .count()
    }

    /// The class the cross-file cases reach for. `Mutate` is non-const,
    /// `Peek` is const, `x` is a plain field.
    const SIBLING_C: &str =
        "class C { int x; void Mutate() {} bool Peek() const { return true; } }";

    /// Same-file spelling of `SIBLING_C`, so each case can be stated as a
    /// pair without duplicating the class body.
    fn same_file(body: &str) -> String {
        format!("{SIBLING_C} {body}")
    }

    /// Every receiver spelling, with what the rule must answer. `&out` is
    /// an OUTPUT parameter — the callee assigns it, so it is never const;
    /// `&inout` aliases a mutable object. A by-value parameter is the
    /// caller's copy. Only the spellings that make the OBJECT const fire.
    ///
    /// (`const C &out` is not valid AngelScript — an output reference that
    /// cannot be written to is a contradiction — so it has no row.)
    const RECEIVERS: &[(&str, bool)] = &[
        ("const C &in c", true),
        ("const C@ &in c", true),
        ("const C@ c", true),
        ("const C c", true),
        ("C &in c", false),
        ("C &out c", false),
        ("C &inout c", false),
        ("C c", false),
        ("C@ c", false),
    ];

    /// The ticket's repro, verbatim: `class C` in `a.as`, the const
    /// receiver in `b.as`. RED before B116 — the emit arm was gated on
    /// `file_classes`, which never held `C`.
    #[test]
    fn cross_file_nonconst_method_on_const_ref_fires() {
        let diags = check_ws("void f(const C@ c) { c.Mutate(); }", &[SIBLING_C], None);
        assert_eq!(
            fired(&diags),
            1,
            "cross-file const receiver must diagnose, got {:?}",
            diags
        );
    }

    /// The same-file control for the repro. This one always passed; it is
    /// here so a regression cannot silently move the pair apart.
    #[test]
    fn same_file_nonconst_method_on_const_ref_fires() {
        let diags = check_ws(&same_file("void f(const C@ c) { c.Mutate(); }"), &[], None);
        assert_eq!(fired(&diags), 1, "same-file control, got {:?}", diags);
    }

    /// The full receiver matrix, stated as a same-file/cross-file PAIR for
    /// every spelling. Both halves must give the same verdict: that
    /// equality is the property B116 restores.
    #[test]
    fn receiver_matrix_agrees_across_the_file_boundary() {
        for (recv, should_fire) in RECEIVERS {
            let body = format!("void f({recv}) {{ c.Mutate(); }}");
            let want = usize::from(*should_fire);

            let same = check_ws(&same_file(&body), &[], None);
            assert_eq!(
                fired(&same),
                want,
                "same-file `{recv}` expected {want}, got {:?}",
                same
            );

            let split = check_ws(&body, &[SIBLING_C], None);
            assert_eq!(
                fired(&split),
                want,
                "cross-file `{recv}` expected {want}, got {:?}",
                split
            );
        }
    }

    /// A method that carries the trailing `const` qualifier is legal on a
    /// const receiver — and the qualifier has to survive the file
    /// boundary, or widening the gate just trades a false negative for a
    /// false positive on every well-written const method.
    #[test]
    fn const_method_stays_silent_across_the_file_boundary() {
        for recv in RECEIVERS.iter().map(|(r, _)| r) {
            let body = format!("void f({recv}) {{ c.Peek(); }}");
            let split = check_ws(&body, &[SIBLING_C], None);
            assert_eq!(
                fired(&split),
                0,
                "cross-file const method `{recv}` must be silent, got {:?}",
                split
            );
            let same = check_ws(&same_file(&body), &[], None);
            assert_eq!(
                fired(&same),
                0,
                "same-file const method `{recv}` must be silent, got {:?}",
                same
            );
        }
    }

    /// Reading a FIELD through a const receiver is not a method call. The
    /// widened gate must not mistake a field for a non-const method just
    /// because the pooled symbol lives in another file.
    #[test]
    fn cross_file_field_read_on_const_receiver_is_silent() {
        let diags = check_ws("void f(const C@ c) { int y = c.x; }", &[SIBLING_C], None);
        assert_eq!(
            fired(&diags),
            0,
            "field read must be silent, got {:?}",
            diags
        );
    }

    /// An INHERITED method reached through a derived const receiver.
    ///
    /// The EQUALITY is what this test has always been for, and it is
    /// unchanged: the verdict must not depend on the file boundary, because
    /// B116 established the two arms are separate code paths. What changed is
    /// the VALUE it pins, and B119 is why.
    ///
    /// B116 left this silent deliberately — `index_file_classes` records only
    /// a class's OWN members, so `d.Mutate()` on `class D : B` never matched
    /// the same-file arm either, and B116 restored parity rather than
    /// inventing a rule the same-file side never had. Whether the engine
    /// actually rejects it was UNMEASURED, and the ticket said so.
    ///
    /// B119 measured it (`scripts/probe_const_receiver.py`, corpus in
    /// `docs/probes/b119-const-receiver.tsv`). The engine REJECTS it —
    /// `No matching signatures to 'D::Mutate() const'` — at depths 1, 2 and 4
    /// and for every receiver spelling, while accepting the inherited CONST
    /// case. So possibility 1 of that ticket, "a real missed error", is the
    /// answer here and the rule now walks parents.
    ///
    /// The fleet site that motivated the ticket is NOT this case. It was
    /// `const Editor::SetSkinSpec@[]@`, and the probe found a SECOND defect
    /// there: the parser bound a leading `const` to the first `@`, making the
    /// array element a const receiver when the engine treats it as a mutable
    /// pointee behind a const slot. Both readings the ticket offered were
    /// true, of different things — see
    /// `b119_inherited_const_receiver_tests` and
    /// `Parser::const_binding_at`.
    #[test]
    fn inherited_nonconst_method_agrees_across_the_file_boundary() {
        let base = "class B { void Mutate() {} }";
        let derived = "class D : B {}";
        let split = check_ws("void f(const D@ d) { d.Mutate(); }", &[base, derived], None);
        let same = check_ws(
            &format!("{base} {derived} void f(const D@ d) {{ d.Mutate(); }}"),
            &[],
            None,
        );
        assert_eq!(
            fired(&split),
            fired(&same),
            "inherited-method verdict must not depend on the file boundary: \
             cross-file {:?} vs same-file {:?}",
            split,
            same
        );
        assert_eq!(
            fired(&same),
            1,
            "B119: the engine rejects an inherited non-const method on a const \
             receiver, so the rule walks parents. Measured, not chosen — if \
             this is 0 again the parent walk regressed: {:?}",
            same
        );
    }

    /// An inherited CONST method is silent on both sides too — same
    /// mechanism, but here the silence is also the CORRECT answer, so it
    /// pins the pair from the other direction.
    #[test]
    fn cross_file_inherited_const_method_is_silent() {
        let base = "class B { bool Peek() const { return true; } }";
        let derived = "class D : B {}";
        let diags = check_ws("void f(const D@ d) { d.Peek(); }", &[base, derived], None);
        assert_eq!(fired(&diags), 0, "inherited const method, got {:?}", diags);
    }

    /// An override in the DERIVED class wins over the base's qualifier:
    /// a const base method overridden without `const` is non-const here.
    #[test]
    fn cross_file_derived_override_decides_constness() {
        let base = "class B { bool Peek() const { return true; } }";
        let derived = "class D : B { bool Peek() { return false; } }";
        let diags = check_ws("void f(const D@ d) { d.Peek(); }", &[base, derived], None);
        assert_eq!(
            fired(&diags),
            1,
            "derived non-const override must diagnose, got {:?}",
            diags
        );
    }

    /// A virtual-property accessor: `get_V() const` satisfies a const
    /// receiver, `set_V()` does not. Same-file handled this through the
    /// `file_const_methods` AST sidecar, which recorded a const accessor
    /// under the STRIPPED property name; B114 retired that sidecar, and
    /// both surfaces now read `workspace_own_method_is_const`, whose
    /// accessor fallback is what keeps this case working. So this no
    /// longer checks that two paths AGREE — there is only one — but it is
    /// exactly the case that would regress if that fallback were dropped.
    #[test]
    fn cross_file_virtual_property_accessors_follow_their_qualifier() {
        let sib =
            "class P { int _v; int get_V() const { return _v; } void set_V(int v) { _v = v; } }";
        let read = check_ws("void f(const P@ p) { int y = p.get_V(); }", &[sib], None);
        assert_eq!(
            fired(&read),
            0,
            "const getter must be silent, got {:?}",
            read
        );
        let write = check_ws("void f(const P@ p) { p.set_V(1); }", &[sib], None);
        assert_eq!(
            fired(&write),
            1,
            "non-const setter must diagnose, got {:?}",
            write
        );
    }

    /// B114: constness is keyed by the name a call uses, just like the
    /// retired same-file sidecar. A virtual-property accessor therefore
    /// contributes under both `get_V`/`set_V` and its stripped `V` name.
    ///
    /// State the cases as same-file/cross-file pairs: B116 deliberately uses
    /// this same pooled question, so a collision must not change its verdict
    /// merely because the declaration moved to a sibling file.
    #[test]
    fn b114_accessor_names_and_direct_collisions_preserve_name_keyed_constness() {
        let accessor = "class P { int _v; int get_V() const { return _v; } }";
        let collision = "class C { void M() {} int get_M() const { return 1; } }";
        let setter_collision = "class S { void M() {} void set_M() const {} }";
        let no_const_match = "class N { void M() {} int get_M() { return 1; } }";

        for (class, receiver, call, want, label) in [
            (accessor, "P", "p.V()", 0, "stripped const getter"),
            (
                collision,
                "C",
                "c.M()",
                0,
                "direct/accessor const collision",
            ),
            (
                setter_collision,
                "S",
                "s.M()",
                0,
                "direct/setter const collision",
            ),
            (
                no_const_match,
                "N",
                "n.M()",
                1,
                "direct/accessor all non-const",
            ),
        ] {
            let var = receiver.to_ascii_lowercase();
            let body = format!("void f(const {receiver}@ {var}) {{ {call}; }}");
            let same = check_ws(&format!("{class} {body}"), &[], None);
            let split = check_ws(&body, &[class], None);
            assert_eq!(
                fired(&same),
                want,
                "same-file {label} expected {want}, got {:?}",
                same
            );
            assert_eq!(
                fired(&split),
                want,
                "cross-file {label} expected {want}, got {:?}",
                split
            );
        }
    }

    /// THE FALSE-POSITIVE GUARD, narrowed by B130 to the population it is
    /// still true of: NADEO receivers.
    ///
    /// B116 wrote this as "engine receivers never fire (no const metadata)".
    /// Both halves of that have since been measured and only one survived.
    /// The metadata claim was stale (B124): Core carries constness in its
    /// `decl` suffix, B125 made it arrive, and B130 consumes it — so a
    /// non-const CORE method on a const receiver now correctly fires, which
    /// the engine itself confirms.
    ///
    /// The NADEO half still holds, for a sharper reason than "no metadata".
    /// The engine enforces the rule there too — probed live,
    /// `const CMwNod@ n; n.MwAddRef()` gives "No matching signatures to
    /// 'CMwNod::MwAddRef() const'" — but the Nadeo LOADER hardcodes
    /// `is_const: false` on every method because that dump carries no
    /// declaration text. So `false` means UNKNOWN there, and firing on it
    /// would flag every correctly-written const Nadeo call. The rule is
    /// right and our data is absent, which is a false-positive surface, not
    /// a licence.
    ///
    /// (The original third row, `const MwFastBuffer<uint>@`, was removed
    /// rather than re-pointed: probed live it fails with "Object handle is
    /// not supported for this type" — a value type cannot be held by handle,
    /// so the shape never compiled and the row was never evidence about
    /// constness at all. Core coverage lives in
    /// `b130_engine_const_receiver_tests` on shapes the game accepts.)
    #[test]
    fn nadeo_engine_receiver_never_fires() {
        use crate::typedb::TypeIndex;
        let base =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
        let idx = TypeIndex::load(
            &base.join("OpenplanetCore.json"),
            &base.join("OpenplanetNext.json"),
        )
        .expect("typedb fixtures must load");
        for src in [
            "void f(const CTrackMania@ app) { app.BackToMainMenu(); }",
            "void f(const CMwNod@ n) { n.MwAddRef(); }",
        ] {
            let diags = check_ws(src, &[], Some(&idx));
            assert_eq!(
                fired(&diags),
                0,
                "Nadeo receiver must not fire — `is_const` is hardcoded false \
                 there, so it means UNKNOWN: {src} -> {:?}",
                diags
            );
        }
    }

    /// A namespaced workspace class, split across files: the pooled name
    /// is `N::C`, and the rule has to normalize to it or it silently
    /// stops firing again for every plugin that uses a namespace.
    #[test]
    fn cross_file_namespaced_class_fires() {
        let sib = "namespace N { class C { void Mutate() {} } }";
        let diags = check_ws("void f(const N::C@ c) { c.Mutate(); }", &[sib], None);
        assert_eq!(
            fired(&diags),
            1,
            "namespaced cross-file class must diagnose, got {:?}",
            diags
        );
    }
}

// ── B120: const propagation through a CROSS-FILE method return type ─────────
//
// B116 made the const-receiver CALL check cross the file boundary. It did not
// make the const travel any further: the same-file arm calls
// `apply_receiver_const` on the method's return type, the cross-file arm did
// not, so `c.GetArr()[0] = 5` fired `ConstViolation` when `C` was in this file
// and stayed silent when it was next door.
//
// Every case is stated as a same-file / cross-file PAIR and asserts the two
// diagnostic sets are EQUAL. That is the assertion that matters: it pins the
// halves together without hardcoding what either produces, so a later change
// to the AC19 propagation model moves both or fails loudly. Hardcoded
// expectations would let the halves drift apart again while both tests still
// passed.
#[cfg(test)]
mod b120_cross_file_const_propagation_tests {
    use super::*;
    use crate::lexer::tokenize_filtered;
    use crate::parser::Parser;
    use crate::symbols::SymbolTable;

    fn check_ws(main: &str, siblings: &[&str]) -> Vec<TypeDiagnostic> {
        let mut ws = SymbolTable::new();
        for sib in siblings {
            let tokens = tokenize_filtered(sib);
            let mut parser = Parser::new(&tokens, sib);
            let file = parser.parse_file();
            let fid = ws.allocate_file_id();
            let syms = SymbolTable::extract_symbols(fid, sib, &file);
            ws.set_file_symbols(fid, syms);
        }
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

    /// The diagnostic KINDS produced, sorted — the comparable summary. The
    /// pair assertion is on kinds rather than spans because the same-file
    /// and cross-file sources differ in length, so spans cannot match even
    /// when the behaviour does.
    fn kinds(diags: &[TypeDiagnostic]) -> Vec<String> {
        let mut v: Vec<String> = diags
            .iter()
            .map(|d| match &d.kind {
                TypeDiagnosticKind::NonConstMethodOnConstReceiver { .. } => {
                    "NonConstMethodOnConstReceiver".to_string()
                }
                TypeDiagnosticKind::ConstViolation { .. } => "ConstViolation".to_string(),
                other => format!("{:?}", std::mem::discriminant(other)),
            })
            .collect();
        v.sort();
        v
    }

    /// `(label, class_body, use_body)`. The class is spelled once and placed
    /// either in the main file or in a sibling; nothing else differs between
    /// the two halves of a pair.
    ///
    /// Return-type axes per the ticket: a handle, a bare value, an array by
    /// value, an array handle, and a class handle whose FIELD is then
    /// written — each in both a non-const and a const flavour, since the
    /// non-const flavour is the one that propagates and the const flavour is
    /// the one that must not.
    const CASES: &[(&str, &str, &str)] = &[
        (
            "nonconst returning array handle, element written",
            "class C { array<int>@ GetArr() { return null; } }",
            "void f(const C@ c) { c.GetArr()[0] = 5; }",
        ),
        (
            "const returning array handle, element written",
            "class C { array<int>@ GetArr() const { return null; } }",
            "void f(const C@ c) { c.GetArr()[0] = 5; }",
        ),
        (
            "nonconst returning array by value, element written",
            "class C { array<int> GetVals() { array<int> a; return a; } }",
            "void f(const C@ c) { c.GetVals()[0] = 5; }",
        ),
        (
            "const returning array by value, element written",
            "class C { array<int> GetVals() const { array<int> a; return a; } }",
            "void f(const C@ c) { c.GetVals()[0] = 5; }",
        ),
        (
            "nonconst returning a value into auto, then reassigned",
            "class C { string Name() { return \"\"; } }",
            "void f(const C@ c) { auto s = c.Name(); s = \"x\"; }",
        ),
        (
            "const returning a value into auto, then reassigned",
            "class C { string Name() const { return \"\"; } }",
            "void f(const C@ c) { auto s = c.Name(); s = \"x\"; }",
        ),
        (
            "nonconst returning class handle, field written",
            "class P { int x; } class C { P@ Get() { return null; } }",
            "void f(const C@ c) { c.Get().x = 1; }",
        ),
        (
            "const returning class handle, field written",
            "class P { int x; } class C { P@ Get() const { return null; } }",
            "void f(const C@ c) { c.Get().x = 1; }",
        ),
        (
            "nonconst returning void, result unused",
            "class C { void Poke() {} }",
            "void f(const C@ c) { c.Poke(); }",
        ),
        (
            "const returning void, result unused",
            "class C { void Poke() const {} }",
            "void f(const C@ c) { c.Poke(); }",
        ),
        // The ticket's named axis: an overload set carrying BOTH a const and
        // a non-const member. `workspace_own_method_is_const` answers
        // `Some(true)` because a const overload exists and the engine would
        // select it — so the call is legal and NOTHING propagates. This is
        // where "any const overload makes the call legal" and "the return
        // type of the one that matched" can diverge, so the halves being
        // equal is the property under test.
        (
            "const + nonconst overload, array handle element written",
            "class C { array<int>@ Get() { return null; } array<int>@ Get() const { return null; } }",
            "void f(const C@ c) { c.Get()[0] = 5; }",
        ),
        (
            "nonconst + const overload declared in the other order",
            "class C { array<int>@ Get() const { return null; } array<int>@ Get() { return null; } }",
            "void f(const C@ c) { c.Get()[0] = 5; }",
        ),
        (
            "overload set differing in ARITY, all non-const",
            "class C { array<int>@ Get() { return null; } array<int>@ Get(int i) { return null; } }",
            "void f(const C@ c) { c.Get()[0] = 5; }",
        ),
    ];

    /// The property B120 exists to establish: for every case, the same-file
    /// and cross-file halves produce the SAME diagnostics.
    #[test]
    fn const_propagation_agrees_across_the_file_boundary() {
        for (label, class_body, use_body) in CASES {
            let same = check_ws(&format!("{class_body} {use_body}"), &[]);
            let split = check_ws(use_body, &[class_body]);
            assert_eq!(
                kinds(&same),
                kinds(&split),
                "`{label}` must not depend on the file boundary:\n  same-file {:?}\n  cross-file {:?}",
                same,
                split
            );
        }
    }

    /// NON-VACUITY. The pair assertion above is satisfied by two empty sets,
    /// so on its own it would still pass with the propagation deleted. This
    /// pins the case that must actually produce BOTH diagnostics on BOTH
    /// sides — the ticket's repro — so the equality above is known to be
    /// comparing something.
    #[test]
    fn the_repro_case_really_produces_both_diagnostics_on_both_sides() {
        let class_body = "class C { array<int>@ GetArr() { return null; } }";
        let use_body = "void f(const C@ c) { c.GetArr()[0] = 5; }";
        let want = vec![
            "ConstViolation".to_string(),
            "NonConstMethodOnConstReceiver".to_string(),
        ];
        let same = check_ws(&format!("{class_body} {use_body}"), &[]);
        assert_eq!(kinds(&same), want, "same-file half, got {:?}", same);
        let split = check_ws(use_body, &[class_body]);
        assert_eq!(kinds(&split), want, "cross-file half, got {:?}", split);
    }

    /// A LEGAL call must stay completely silent on both sides. Paired with
    /// the test above this brackets the rule from both directions: the error
    /// case produces two diagnostics, the legal case produces none.
    #[test]
    fn a_const_method_call_stays_silent_on_both_sides() {
        let class_body = "class C { array<int>@ GetArr() const { return null; } }";
        let use_body = "void f(const C@ c) { c.GetArr()[0] = 5; }";
        for (half, diags) in [
            (
                "same-file",
                check_ws(&format!("{class_body} {use_body}"), &[]),
            ),
            ("cross-file", check_ws(use_body, &[class_body])),
        ] {
            assert!(
                diags.is_empty(),
                "{half}: a const method on a const receiver is legal and its \
                 result must not inherit const, got {:?}",
                diags
            );
        }
    }

    /// THE FALSE-POSITIVE GUARD, and the reason the propagation is confined
    /// to the arm that already emitted. An ENGINE method reached through a
    /// const receiver returns a FRESH value, not a view into the receiver —
    /// `const string &in s` then `s.Replace(..)` yields a new string that is
    /// perfectly assignable. The typedb has no metadata to tell a view from
    /// a fresh value, so nothing here may propagate.
    ///
    /// Measured: allowing it produced 10 false `ConstViolation`s across 7
    /// fleet plugins on shipped, compiling code — tm-cotd-hud/Color.as:356,
    /// tm-match-recorder/Main.as:162-163, tm-birds/Filter.as:65,
    /// tm-archivist/HttpServer.as:313 and others, every one this shape.
    #[test]
    fn engine_receiver_return_values_never_inherit_const() {
        use crate::typedb::TypeIndex;
        let base =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
        let idx = TypeIndex::load(
            &base.join("OpenplanetCore.json"),
            &base.join("OpenplanetNext.json"),
        )
        .expect("typedb fixtures must load");
        // The exact shapes the fleet sweep flagged.
        for src in [
            "string f(const string &in raw) { auto r = raw.Replace(\"a\", \"b\"); r = r.Replace(\"c\", \"d\"); return r; }",
            "string f(const string &in s) { auto x = s.ToLower(); x = \"y\"; return x; }",
            "void f(const string &in term) { auto parts = term.Split(\" \"); parts[0] = \"x\"; }",
        ] {
            let tokens = tokenize_filtered(src);
            let mut parser = Parser::new(&tokens, src);
            let file = parser.parse_file();
            let mut ws = SymbolTable::new();
            let fid = ws.allocate_file_id();
            let syms = SymbolTable::extract_symbols(fid, src, &file);
            ws.set_file_symbols(fid, syms);
            let scope = GlobalScope::new(&ws, Some(&idx));
            let mut checker = Checker::new(src, &scope);
            checker.check_file(&file);
            let bad: Vec<_> = checker
                .diagnostics
                .iter()
                .filter(|d| matches!(&d.kind, TypeDiagnosticKind::ConstViolation { .. }))
                .collect();
            assert!(
                bad.is_empty(),
                "engine method returns a fresh value, not a const view: {src}\n  got {:?}",
                bad
            );
        }
    }

    /// The propagation must read the WORKSPACE half only. A class that is
    /// nowhere — not in this file, not in the pool — must not acquire a
    /// return type or a const from anywhere.
    #[test]
    fn unknown_class_propagates_nothing() {
        let diags = check_ws("void f(const Nope@ n) { n.Whatever()[0] = 5; }", &[]);
        let bad: Vec<_> = diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::ConstViolation { .. }
                        | TypeDiagnosticKind::NonConstMethodOnConstReceiver { .. }
                )
            })
            .collect();
        assert!(bad.is_empty(), "unknown class, got {:?}", diags);
    }
}

// ── B121: constness travels only through a return that can ALIAS ────────────
//
// AC19 treated every method return as a view into the receiver, so a const
// receiver made the result const whatever its shape. That is right for a
// returned handle into `this` and wrong for a returned VALUE, which is a fresh
// object: `const string &in s` then `s.Replace(..)` hands back a new string
// that is perfectly assignable.
//
// The gate is a SHAPE proxy and it is partial by construction. Measured on the
// 151-plugin fleet by re-running B120's variant C (propagation widened to
// engine receivers) with and without it: it removes 7 of the 10 false
// positives. The 3 survivors are all `string::Split`, which returns
// `string[]@` — a FRESH array behind a handle, indistinguishable by shape from
// `Json::Value::Get`, which returns a genuine view. No metadata in the dumps
// separates them. `residual_fresh_handles_are_documented_not_fixed` pins that
// so the limit is a test rather than a claim in a comment.
#[cfg(test)]
mod b121_return_aliasing_tests {
    use super::*;
    use crate::lexer::tokenize_filtered;
    use crate::parser::Parser;
    use crate::symbols::SymbolTable;

    fn check_ws(
        main: &str,
        siblings: &[&str],
        typedb: Option<&crate::typedb::TypeIndex>,
    ) -> Vec<TypeDiagnostic> {
        let mut ws = SymbolTable::new();
        for sib in siblings {
            let tokens = tokenize_filtered(sib);
            let mut parser = Parser::new(&tokens, sib);
            let file = parser.parse_file();
            let fid = ws.allocate_file_id();
            let syms = SymbolTable::extract_symbols(fid, sib, &file);
            ws.set_file_symbols(fid, syms);
        }
        let tokens = tokenize_filtered(main);
        let mut parser = Parser::new(&tokens, main);
        let file = parser.parse_file();
        let fid = ws.allocate_file_id();
        let syms = SymbolTable::extract_symbols(fid, main, &file);
        ws.set_file_symbols(fid, syms);
        let scope = GlobalScope::new(&ws, typedb);
        let mut checker = Checker::new(main, &scope);
        checker.check_file(&file);
        checker.diagnostics
    }

    /// Diagnostics that mean "you wrote through a const view". The
    /// propagation surfaces as either kind depending on the write's shape —
    /// `ConstViolation` for an indexed/field write, `ReadOnlyReference` for
    /// a direct reassignment — and this rule is about WHETHER const
    /// travelled, not which downstream check noticed. Counting only one
    /// kind would read a real propagation as an absence.
    fn violations(diags: &[TypeDiagnostic]) -> usize {
        diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::ConstViolation { .. }
                        | TypeDiagnosticKind::ReadOnlyReference
                )
            })
            .count()
    }

    fn nonconst(diags: &[TypeDiagnostic]) -> usize {
        diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::NonConstMethodOnConstReceiver { .. }
                )
            })
            .count()
    }

    fn typedb() -> crate::typedb::TypeIndex {
        let base =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
        crate::typedb::TypeIndex::load(
            &base.join("OpenplanetCore.json"),
            &base.join("OpenplanetNext.json"),
        )
        .expect("typedb fixtures must load")
    }

    /// `(label, class_body, use_body, expect_violation)`. Every row is a
    /// non-const method on a const receiver, so `NonConstMethodOnConstReceiver`
    /// always fires; what varies is whether the RESULT also inherits const.
    ///
    /// The pair that is the whole point: a returned handle may be a view into
    /// `this`, a returned value cannot be. Either row alone proves nothing.
    const CASES: &[(&str, &str, &str, bool)] = &[
        (
            "returns array HANDLE — may alias, propagates",
            "class C { array<int>@ Get() { return null; } }",
            "void f(const C@ c) { c.Get()[0] = 5; }",
            true,
        ),
        (
            "returns array BY VALUE — fresh, must not propagate",
            "class C { array<int> Get() { array<int> a; return a; } }",
            "void f(const C@ c) { c.Get()[0] = 5; }",
            false,
        ),
        (
            "returns class HANDLE, field written — may alias",
            "class P { int x; } class C { P@ Get() { return null; } }",
            "void f(const C@ c) { c.Get().x = 1; }",
            true,
        ),
        (
            "returns class BY VALUE, field written — fresh",
            "class P { int x; } class C { P Get() { P p; return p; } }",
            "void f(const C@ c) { c.Get().x = 1; }",
            false,
        ),
        (
            "returns string BY VALUE into auto, reassigned — the FP shape",
            "class C { string Name() { return \"\"; } }",
            "void f(const C@ c) { auto s = c.Name(); s = \"x\"; }",
            false,
        ),
        (
            "returns string HANDLE into auto, reassigned",
            "class C { string@ Name() { return null; } }",
            "void f(const C@ c) { auto s = c.Name(); s = \"x\"; }",
            true,
        ),
        (
            "returns a primitive by value",
            "class C { int N() { return 0; } }",
            "void f(const C@ c) { auto n = c.N(); n = 1; }",
            false,
        ),
    ];

    /// The rule, on the same-file half.
    #[test]
    fn only_aliasing_returns_inherit_receiver_const_same_file() {
        for (label, class_body, use_body, want) in CASES {
            let d = check_ws(&format!("{class_body} {use_body}"), &[], None);
            // `== 1`, tightened from `>= 1` by B126. A write THROUGH a call
            // (`c.Get().x = 1`) used to walk the callee TWICE — once to type
            // the assignment target, once for the value — emitting the call
            // diagnostic twice at the same span. lane-enum relaxed this to
            // `>= 1` during B120 with the reason written in, rather than
            // asserting `== 1` and thereby asserting a bug is absent when it
            // was not. The double walk is fixed, so the honest assertion is
            // available again. It matters at unit level even though the CLI
            // dedups by span+kind: the dedup is why the fleet never showed
            // this, not a reason it was harmless.
            assert_eq!(
                nonconst(&d),
                1,
                "`{label}`: the call must be diagnosed exactly once, got {:?}",
                d
            );
            assert_eq!(
                violations(&d) > 0,
                *want,
                "`{label}`: expected propagation={want}, got {:?}",
                d
            );
        }
    }

    /// The same rule must hold on the CROSS-FILE half. B120 pinned the two
    /// halves together; B121 changes both, so the pairing is re-asserted here
    /// rather than assumed to have survived.
    #[test]
    fn aliasing_rule_agrees_across_the_file_boundary() {
        for (label, class_body, use_body, _) in CASES {
            let same = check_ws(&format!("{class_body} {use_body}"), &[], None);
            let split = check_ws(use_body, &[class_body], None);
            assert_eq!(
                (nonconst(&same), violations(&same)),
                (nonconst(&split), violations(&split)),
                "`{label}` must not depend on the file boundary:\n  same {:?}\n  split {:?}",
                same,
                split
            );
        }
    }

    /// B126: a write THROUGH a call diagnoses the callee exactly ONCE.
    ///
    /// `check_readonly_prop_assign` re-walks the assignment target's object to
    /// type it, and `expr_type` PUSHES DIAGNOSTICS. Its comment claimed
    /// member-access typing was "read-only and idempotent, so walking the
    /// object again is safe" — true of the type it returns, false of its
    /// effect on the diagnostic list. So `c.Get().x = 1` reported the const
    /// receiver twice at the identical span.
    ///
    /// This is pinned at unit level BECAUSE the CLI dedups by span+kind. That
    /// dedup is why the fleet never showed it — it is not a reason the double
    /// emit was harmless, and any consumer reading diagnostics before the
    /// dedup (an LSP client, a count, a test) saw two.
    #[test]
    fn a_write_through_a_call_does_not_double_report_the_callee() {
        let class = "class C { C@ Get() { return this; } int x; }";
        let write = check_ws(
            &format!("{class} void f(const C@ c) {{ c.Get().x = 1; }}"),
            &[],
            None,
        );
        assert_eq!(
            nonconst(&write),
            1,
            "the callee must be diagnosed once, not once per walk: {:?}",
            write
        );

        // THE PAIR. Reading through the same call is also exactly one, which
        // is what distinguishes the fix from "assignment suppresses
        // diagnostics" — a suppression would satisfy the assertion above.
        let read = check_ws(
            &format!("{class} void f(const C@ c) {{ auto v = c.Get().x; }}"),
            &[],
            None,
        );
        assert_eq!(
            nonconst(&read),
            1,
            "a READ through the same call must also report once: {:?}",
            read
        );

        // CONTROL, and the one that matters for the shape of the fix: the
        // de-duplication discards a REDUNDANT walk, not diagnostics. The
        // assignment's own violation is still reported, so the truncation is
        // not swallowing the finding the re-walk exists to produce.
        assert_eq!(
            violations(&write),
            1,
            "the assignment's own const violation must survive the \
             de-duplication: {:?}",
            write
        );
    }

    /// NON-VACUITY. The pair test above is satisfied by two identical empty
    /// sets, and the table test would pass with the gate hardcoded to `false`
    /// if no row expected propagation. This pins one row of each polarity to
    /// exact counts, so both tests are known to be discriminating.
    #[test]
    fn the_two_polarities_produce_exactly_the_expected_counts() {
        let handle = check_ws(
            "class C { array<int>@ Get() { return null; } } void f(const C@ c) { c.Get()[0] = 5; }",
            &[],
            None,
        );
        assert_eq!(
            (nonconst(&handle), violations(&handle)),
            (1, 1),
            "{:?}",
            handle
        );

        let value = check_ws(
            "class C { string Name() { return \"\"; } } void f(const C@ c) { auto s = c.Name(); s = \"x\"; }",
            &[],
            None,
        );
        assert_eq!(
            (nonconst(&value), violations(&value)),
            (1, 0),
            "{:?}",
            value
        );
    }

    /// FIELD access is a different question with a different answer and must
    /// be untouched. A field of a const object genuinely IS a view into it,
    /// whatever its shape — so a by-value field still propagates where a
    /// by-value method return no longer does. Getting this wrong would trade
    /// a false positive for a real false negative.
    #[test]
    fn field_access_still_propagates_regardless_of_shape() {
        for (label, src) in [
            (
                "array field by value",
                "class F { array<int> arr; } void k(const F@ f) { f.arr[0] = 5; }",
            ),
            (
                "class field by value",
                "class P { int x; } class F { P p; } void k(const F@ f) { f.p.x = 1; }",
            ),
        ] {
            let d = check_ws(src, &[], None);
            assert!(
                violations(&d) > 0,
                "`{label}`: a field of a const object is a const view, got {:?}",
                d
            );
        }
    }

    /// THE DISCRIMINATING TEST, against the real typedb rather than a
    /// hand-written stand-in. These are the exact engine methods behind
    /// B120's variant-C false positives. `Replace` / `ToLower` / `SubStr`
    /// return by value and the shape gate kills them; `Split` returns
    /// `string[]@` and it does not.
    #[test]
    fn the_fleet_false_positive_methods_are_classified_from_the_real_typedb() {
        let idx = typedb();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&idx));
        for (ty, method, may_alias) in [
            ("string", "Replace", false),
            ("string", "ToLower", false),
            ("string", "ToUpper", false),
            ("string", "SubStr", false),
            ("string", "Trim", false),
            // The residual: a FRESH array behind a handle. Shape says "may
            // alias" and shape is all we have.
            ("string", "Split", true),
        ] {
            let t = scope
                .lookup_method_return(ty, method)
                .unwrap_or_else(|| panic!("typedb must know {ty}::{method}"));
            assert_eq!(
                Checker::method_return_may_alias_receiver(&t),
                may_alias,
                "{ty}::{method} resolved to {:?}",
                t
            );
        }
    }

    /// The limit of the proxy, stated as a test so it cannot quietly become
    /// folklore. `string::Split` returns a fresh array and `Json::Value::Get`
    /// returns a view; both are handles, so the gate treats them alike. This
    /// is the 3 of 10 the fleet sweep left standing. If a future change can
    /// tell them apart, this test is the one that should fail.
    #[test]
    fn residual_fresh_handles_are_documented_not_fixed() {
        let idx = typedb();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&idx));
        let fresh = scope.lookup_method_return("string", "Split").unwrap();
        let view = scope.lookup_method_return("Json::Value", "Get").unwrap();
        assert!(
            Checker::method_return_may_alias_receiver(&fresh)
                && Checker::method_return_may_alias_receiver(&view),
            "the proxy cannot separate a fresh handle from an aliasing one: \
             Split -> {:?}, Json::Value::Get -> {:?}",
            fresh,
            view
        );
    }

    /// An ENGINE receiver still propagates nothing at all — B116/B120 excluded
    /// that population by provenance and B121 does not change it. The shape
    /// gate narrows what MAY propagate; it does not widen who can.
    #[test]
    fn engine_receivers_still_propagate_nothing() {
        let idx = typedb();
        for src in [
            "string f(const string &in raw) { auto r = raw.Replace(\"a\", \"b\"); r = r.Replace(\"c\", \"d\"); return r; }",
            "void f(const string &in term) { auto parts = term.Split(\" \"); parts[0] = \"x\"; }",
        ] {
            let d = check_ws(src, &[], Some(&idx));
            assert_eq!(
                violations(&d),
                0,
                "engine receiver must propagate nothing: {src} -> {:?}",
                d
            );
        }
    }
}

// ── B119: const receivers, inheritance, and where `const` binds ─────────────
//
// Two defects, measured against the live game compiler and fixed together
// because either one alone is a regression.
//
// The ticket asked which of two readings explained the ONE fleet site B116
// found (`tm-map-together/src/Socket.as:467`, `skins[i].WriteToNetworkBuffer`
// on a `const Editor::SetSkinSpec@[]@`): a missed error that wants a parent
// walk, or `const T@[]@` element constness not propagating as modelled. The
// probe settled BOTH, and they are independent — which is why the either/or
// framing would have produced half a fix.
//
//   scripts/probe_const_receiver.py, Trackmania + Openplanet, 21 variants,
//   4 controls, all controls passing:
//
//   | probe row                 | engine | checker before | after |
//   |---|---|---|---|
//   | ctl-own-nonconst          | REJECT | fires          | fires |
//   | inh-d1/d2/d4-nonconst     | REJECT | SILENT (FN)    | fires |
//   | inh-d2-const              | ACCEPT | silent         | silent|
//   | recv-const{ref,handleref,value} | REJECT | SILENT (FN) | fires |
//   | arr-own-nonconst          | ACCEPT | FIRES (FP)     | silent|
//   | arr-inherited-nonconst    | ACCEPT | silent         | silent|
//   | arr-value-array           | REJECT | fires          | fires |
//   | arr-generic-spelling      | ACCEPT | silent         | silent|
//
// The false POSITIVE was not in this rule at all: the parser bound a leading
// `const` to the first `@` in a suffix chain, so `const C@[]@` became an array
// of handles to CONST C instead of a handle to a CONST ARRAY of mutable
// handles — while rendering to the same display string as the correct
// `const array<C@>@`. See `Parser::const_binding_at`.
//
// The fleet site is silent before AND after, for different reasons each time:
// before, because two bugs cancelled; after, because the element genuinely is
// not a const receiver. That is why closing the parent walk alone would have
// turned the one site B116 measured into a false error.
#[cfg(test)]
mod b119_inherited_const_receiver_tests {
    use super::*;
    use crate::lexer::tokenize_filtered;
    use crate::parser::Parser;
    use crate::symbols::SymbolTable;

    fn check_ws(main: &str, siblings: &[&str]) -> Vec<TypeDiagnostic> {
        let mut ws = SymbolTable::new();
        for sibling in siblings {
            let tokens = tokenize_filtered(sibling);
            let mut parser = Parser::new(&tokens, sibling);
            let file = parser.parse_file();
            let fid = ws.allocate_file_id();
            let syms = SymbolTable::extract_symbols(fid, sibling, &file);
            ws.set_file_symbols(fid, syms);
        }
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

    /// How many `NonConstMethodOnConstReceiver` the rule produced.
    fn fired(d: &[TypeDiagnostic]) -> usize {
        d.iter()
            .filter(|x| {
                matches!(
                    &x.kind,
                    TypeDiagnosticKind::NonConstMethodOnConstReceiver { .. }
                )
            })
            .count()
    }

    fn const_violations(d: &[TypeDiagnostic]) -> usize {
        d.iter()
            .filter(|x| matches!(&x.kind, TypeDiagnosticKind::ConstViolation { .. }))
            .count()
    }

    /// Every case is stated TWICE — same-file and split across files —
    /// because B116 established the two arms are separate code paths and a
    /// fix to one is not a fix to the other.
    fn both_ways(decls: &[&str], body: &str) -> (usize, usize) {
        let same = format!("{} {}", decls.join(" "), body);
        (fired(&check_ws(&same, &[])), fired(&check_ws(body, decls)))
    }

    // ── part 2: where a leading `const` binds ────────────────────────────

    fn resolve(spelling: &str) -> TypeRepr {
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, None);
        let src = format!("void f({spelling} a) {{}}");
        let tokens = tokenize_filtered(&src);
        let mut parser = Parser::new(&tokens, &src);
        let file = parser.parse_file();
        let Item::Function(func) = &file.items[0] else {
            panic!("expected a function for `{spelling}`");
        };
        let mut r = TypeResolver::new(&scope, &src);
        r.resolve(&func.params[0].type_expr)
    }

    /// The two spellings of ONE type must produce ONE structure. Before
    /// B119 they did not, and both rendered as `const C@[]@`, which is
    /// exactly why the disagreement survived: every string-level check
    /// agreed while the trees differed.
    #[test]
    fn the_two_spellings_of_a_const_handle_array_agree() {
        assert_eq!(
            resolve("const C@[]@"),
            resolve("const array<C@>@"),
            "`const C@[]@` and `const array<C@>@` are the same type"
        );
    }

    /// Stated positively, so a future change that breaks BOTH spellings the
    /// same way cannot pass the equality test above.
    #[test]
    fn const_binds_to_the_array_not_to_the_element_pointee() {
        // Handle( Const( Array( Handle(C) ) ) ) — a handle to a CONST ARRAY
        // of MUTABLE handles. Measured: `arr.InsertLast(..)` is rejected
        // (the array is const) while `arr[0].Mutate()` and `arr[0].x = 1`
        // are accepted (the pointee is not).
        let repr = resolve("const C@[]@");
        let TypeRepr::Handle(outer) = &repr else {
            panic!("expected an outer Handle, got {repr:?}");
        };
        let TypeRepr::Const(arr) = outer.as_ref() else {
            panic!("expected Const directly inside the outer handle, got {outer:?}");
        };
        assert!(
            matches!(arr.as_ref(), TypeRepr::Array(_)),
            "the `const` must qualify the ARRAY, got {arr:?}"
        );
    }

    /// B119 x B121: the const REBIND must not change any ALIASING verdict.
    ///
    /// B121 gates constness propagation on whether a method's return type can
    /// alias the receiver, and its proxy is the OUTERMOST constructor being a
    /// handle. B119 moves where `const` sits INSIDE a suffix chain. Those two
    /// could have disagreed — a rebind that changed the outermost constructor
    /// would silently change B121's answer for every method returning that
    /// type, which is a const-model change neither ticket measured.
    ///
    /// They do not, and this pins it rather than arguing it: for every
    /// spelling B119 rebinds, the pre-B119 tree and the post-B119 tree give
    /// the SAME aliasing verdict. `const C@[]@` stays `Handle` at the top
    /// (true both ways) and `const C@[]` stays non-`Handle` (false both
    /// ways); the rebind only ever moves `Const` past an `Array`, never
    /// across the outermost `Handle`.
    #[test]
    fn the_const_rebind_never_changes_an_aliasing_verdict() {
        let named = || TypeRepr::Error("C".to_string());
        let handle = |t: TypeRepr| TypeRepr::Handle(Box::new(t));
        let konst = |t: TypeRepr| TypeRepr::Const(Box::new(t));
        let array = |t: TypeRepr| TypeRepr::Array(Box::new(t));

        // (spelling, the tree the parser built BEFORE B119)
        let rebound = [
            ("const C@[]@", handle(array(handle(konst(named()))))),
            ("const C@[]", array(handle(konst(named())))),
        ];
        for (spelling, before) in rebound {
            let after = resolve(spelling);
            assert_ne!(
                after, before,
                "`{spelling}` is supposed to have been rebound by B119"
            );
            assert_eq!(
                Checker::method_return_may_alias_receiver(&after),
                Checker::method_return_may_alias_receiver(&before),
                "B119's rebind changed B121's aliasing verdict for `{spelling}`: \
                 before {before:?}, after {after:?}"
            );
        }

        // And the spellings B119 does NOT touch are untouched, so the
        // invariant above is about the rebind and not about everything
        // trivially agreeing.
        for spelling in ["const C", "const C@", "const C[]"] {
            let _ = resolve(spelling);
        }
        assert!(
            Checker::method_return_may_alias_receiver(&resolve("const C@")),
            "a handle return still aliases"
        );
        assert!(
            !Checker::method_return_may_alias_receiver(&resolve("const C[]")),
            "a by-value array return still does not"
        );
    }

    /// The simple spellings are unchanged. `const C@` in particular still
    /// binds to the pointee — the engine rejects `c.Mutate()` on it, and a
    /// fix that moved every `const` outward would have broken the rule this
    /// ticket is about.
    #[test]
    fn const_binding_is_unchanged_for_the_simple_spellings() {
        assert!(
            matches!(resolve("const C"), TypeRepr::Const(_)),
            "`const C` stays Const(C)"
        );
        let repr = resolve("const C@");
        let TypeRepr::Handle(inner) = &repr else {
            panic!("expected Handle, got {repr:?}");
        };
        assert!(
            matches!(inner.as_ref(), TypeRepr::Const(_)),
            "`const C@` is a handle to CONST, got {inner:?}"
        );
        let repr = resolve("const C[]");
        let TypeRepr::Const(inner) = &repr else {
            panic!("`const C[]` is a const array, got {repr:?}");
        };
        assert!(matches!(inner.as_ref(), TypeRepr::Array(_)));
    }

    /// A trailing reference is transparent to the binding: `const C@ &in`
    /// still makes the POINTEE const, which is what makes
    /// `recv-consthandleref` a REJECT in the game.
    #[test]
    fn a_trailing_reference_does_not_move_the_const() {
        // `TypeRepr` has no reference variant -- the resolver drops the
        // parameter mode -- so what must be preserved is the shape UNDER it.
        let repr = resolve("const C@ &in");
        let TypeRepr::Handle(pointee) = &repr else {
            panic!("expected a Handle for `const C@ &in`, got {repr:?}");
        };
        assert!(
            matches!(pointee.as_ref(), TypeRepr::Const(_)),
            "`const C@ &in` keeps the const on the pointee, got {pointee:?}"
        );
        assert_eq!(
            resolve("const C@ &in"),
            resolve("const C@"),
            "the parameter mode must not move the const"
        );
    }

    /// `const C@[]` — no outer handle — puts the const on the array too.
    ///
    /// MEASURED, not reasoned. The first pass inferred this spelling from the
    /// `const C@[]@` result and said so; it is now its own probe row, because
    /// B119's rebind changes this spelling and an inference is not a
    /// measurement. `arr.InsertLast(null)` is REJECTED (the array is const)
    /// and `arr[0].Mutate()` is ACCEPTED (the element pointee is mutable) —
    /// the same shape as with the outer handle.
    #[test]
    fn a_const_handle_array_without_an_outer_handle_is_a_const_array() {
        let repr = resolve("const C@[]");
        let TypeRepr::Const(inner) = &repr else {
            panic!("expected an outer Const, got {repr:?}");
        };
        assert!(matches!(inner.as_ref(), TypeRepr::Array(_)));
    }

    /// The observable half of the row above: a non-const method on an element
    /// of a `const T@[]` is silent, because the element is a handle and the
    /// pointee is not const. The engine accepts it (`arrq-noouter-elem-method`).
    #[test]
    fn a_non_const_method_on_a_const_handle_array_element_without_an_outer_handle_is_silent() {
        let (same, cross) = both_ways(
            &["class C { void Mutate() {} }"],
            "void f(const C@[] arr) { arr[0].Mutate(); }",
        );
        assert_eq!((same, cross), (0, 0), "engine accepts this");
    }

    // ── part 2, observable: the false positive is gone ───────────────────

    /// THE FALSE POSITIVE. The engine ACCEPTS this (`arr-own-nonconst`), and
    /// the checker used to diagnose it. Not an inheritance case at all —
    /// the method is declared on the element's OWN class — which is what
    /// isolates it from the parent walk below.
    #[test]
    fn a_non_const_method_on_a_const_handle_array_element_is_silent() {
        let (same, cross) = both_ways(
            &["class C { void Mutate() {} }"],
            "void f(const C@[]@ arr) { arr[0].Mutate(); }",
        );
        assert_eq!((same, cross), (0, 0), "engine accepts this; we must too");
    }

    /// The generic spelling was already silent and must stay so — it is the
    /// control that shows the fix moved the shorthand INTO agreement rather
    /// than moving both out of it.
    #[test]
    fn the_generic_spelling_of_a_const_handle_array_stays_silent() {
        let (same, cross) = both_ways(
            &["class C { void Mutate() {} }"],
            "void f(const array<C@>@ arr) { arr[0].Mutate(); }",
        );
        assert_eq!((same, cross), (0, 0));
    }

    /// A const VALUE array is a different type and the engine REJECTS it
    /// (`arr-value-array`). Without this row, "stop firing on const arrays"
    /// would pass, and that would be a false negative.
    #[test]
    fn a_non_const_method_on_a_const_value_array_element_still_fires() {
        let (same, cross) = both_ways(
            &["class C { void Mutate() {} }"],
            "void f(const C[] arr) { arr[0].Mutate(); }",
        );
        assert_eq!((same, cross), (1, 1), "engine rejects this");
    }

    /// The const on the array is still LIVE after the rebind: assigning into
    /// an element slot is a `ConstViolation`, matching the engine's
    /// `Reference is read-only` on `@arr[0] = c`. This is the row that stops
    /// the false-positive fix from becoming a false negative.
    #[test]
    fn assigning_into_a_const_handle_array_element_still_violates() {
        let d = check_ws(
            "class C {} void f(const C@[]@ arr, C@ c) { @arr[0] = c; }",
            &[],
        );
        assert!(
            const_violations(&d) >= 1,
            "the element slot is read-only; got {d:?}"
        );
    }

    // ── part 1: the parent walk ──────────────────────────────────────────

    /// Depth 1, the exact shape of the B116 pin. The engine says
    /// `No matching signatures to 'D::Mutate() const'`.
    #[test]
    fn an_inherited_non_const_method_on_a_const_receiver_fires_at_depth_1() {
        let (same, cross) = both_ways(
            &["class B { void Mutate() {} }", "class D : B {}"],
            "void f(const D@ d) { d.Mutate(); }",
        );
        assert_eq!((same, cross), (1, 1));
    }

    /// Depth 2 — the fleet site's own depth
    /// (`Editor::SetSkinSpec` -> `NetworkSerializable`).
    #[test]
    fn an_inherited_non_const_method_fires_at_depth_2() {
        let (same, cross) = both_ways(
            &[
                "class A { void Mutate() {} }",
                "class B : A {}",
                "class D : B {}",
            ],
            "void f(const D@ d) { d.Mutate(); }",
        );
        assert_eq!((same, cross), (1, 1));
    }

    /// Depth 4 — MLFeed ships hierarchies this deep and B116 measured two
    /// inherited-const sites in them.
    #[test]
    fn an_inherited_non_const_method_fires_at_depth_4() {
        let (same, cross) = both_ways(
            &[
                "class A1 { void Mutate() {} }",
                "class A2 : A1 {}",
                "class A3 : A2 {}",
                "class A4 : A3 {}",
            ],
            "void f(const A4@ d) { d.Mutate(); }",
        );
        assert_eq!((same, cross), (1, 1));
    }

    /// THE GUARD. An inherited CONST method must stay silent — the engine
    /// accepts it (`inh-d2-const`). Without this row, a fix that fires on
    /// every inherited method passes everything above.
    #[test]
    fn an_inherited_const_method_stays_silent_at_every_depth() {
        for decls in [
            vec![
                "class A { bool Peek() const { return true; } }",
                "class D : A {}",
            ],
            vec![
                "class A { bool Peek() const { return true; } }",
                "class B : A {}",
                "class D : B {}",
            ],
        ] {
            let (same, cross) = both_ways(&decls, "void f(const D@ d) { d.Peek(); }");
            assert_eq!((same, cross), (0, 0), "inherited const, decls {decls:?}");
        }
    }

    /// Every receiver spelling B116 pairs, on the inherited path. The engine
    /// rejects all four.
    #[test]
    fn every_const_receiver_spelling_fires_on_an_inherited_method() {
        for recv in ["const D &in d", "const D@ &in d", "const D@ d", "const D d"] {
            let (same, cross) = both_ways(
                &["class B { void Mutate() {} }", "class D : B {}"],
                &format!("void f({recv}) {{ d.Mutate(); }}"),
            );
            assert_eq!((same, cross), (1, 1), "receiver `{recv}`");
        }
    }

    /// A NON-const receiver is untouched by any of this.
    #[test]
    fn a_mutable_receiver_never_fires_on_an_inherited_method() {
        let (same, cross) = both_ways(
            &["class B { void Mutate() {} }", "class D : B {}"],
            "void f(D@ d) { d.Mutate(); }",
        );
        assert_eq!((same, cross), (0, 0));
    }

    /// A derived override decides its own constness — a const override of a
    /// non-const base method makes the call legal, and the walk must not
    /// reach past it. This is the inherited counterpart of
    /// `cross_file_derived_override_decides_constness`.
    #[test]
    fn a_const_override_in_the_derived_class_wins_over_a_non_const_base() {
        let (same, cross) = both_ways(
            &[
                "class B { void Mutate() {} }",
                "class D : B { void Mutate() const {} }",
            ],
            "void f(const D@ d) { d.Mutate(); }",
        );
        assert_eq!(
            (same, cross),
            (0, 0),
            "the override is const, so this is legal"
        );
    }

    /// And the other direction: a non-const override of a const base method
    /// fires, and fires exactly ONCE — the own-class arm owns it and the
    /// inherited walk must not double-report.
    #[test]
    fn a_non_const_override_fires_exactly_once() {
        let (same, cross) = both_ways(
            &[
                "class B { bool Peek() const { return true; } }",
                "class D : B { bool Peek() { return false; } }",
            ],
            "void f(const D@ d) { d.Peek(); }",
        );
        assert_eq!((same, cross), (1, 1), "exactly one, not two");
    }

    /// A method declared on the class AND on an ancestor yields exactly ONE
    /// diagnostic.
    ///
    /// # What this pins, and what it does NOT
    ///
    /// It pins the OBSERVABLE. It does not prove the `!own_declares` guard on
    /// the B119 arm is what produces it — mutation testing showed that
    /// removing that guard leaves this test green, so stating it as the
    /// double-report guard would have been a vacuous claim of exactly the
    /// kind B118 found four of.
    ///
    /// The reason is B120, which landed between this ticket's measurement and
    /// its fix: the B116 cross-file arm now ends in
    /// `return Self::apply_receiver_const(t, true)` when
    /// `workspace_class_member` resolves the member, so the B119 arm below it
    /// is never reached on that path. Same-file is covered by its own arm
    /// returning early. Between them the two own-class arms already exit
    /// before the inherited walk can add a second diagnostic.
    ///
    /// `!own_declares` is kept anyway, for the residual path where the
    /// cross-file arm fires and `workspace_class_member` returns `None` — it
    /// then falls through rather than returning. That path is real but not
    /// reachable from any fixture here, so the guard is defence in depth and
    /// is documented as such rather than credited to this test.
    #[test]
    fn a_method_declared_on_both_the_class_and_an_ancestor_fires_once() {
        let (same, cross) = both_ways(
            &[
                "class B { void Mutate() {} }",
                "class D : B { void Mutate() {} }",
            ],
            "void f(const D@ d) { d.Mutate(); }",
        );
        assert_eq!(
            (same, cross),
            (1, 1),
            "one diagnostic, not one per declaration"
        );
    }

    /// An inherited FIELD read is not a method call.
    #[test]
    fn reading_an_inherited_field_through_a_const_receiver_is_silent() {
        let (same, cross) = both_ways(
            &["class B { int x; }", "class D : B {}"],
            "void f(const D@ d) { int y = d.x; }",
        );
        assert_eq!((same, cross), (0, 0));
    }

    /// Provenance: an unknown receiver type has no const metadata anywhere,
    /// so the walk answers `None` and stays silent rather than treating
    /// "not known const" as "non-const". B116 established this rule for the
    /// own-class arm; it has to hold one level out or the widening fires on
    /// every engine type in the fleet.
    #[test]
    fn an_unknown_receiver_type_is_never_diagnosed() {
        let d = check_ws("void f(const CSomeEngineThing@ c) { c.Whatever(); }", &[]);
        assert_eq!(
            fired(&d),
            0,
            "no const metadata means no verdict, got {d:?}"
        );
    }

    /// A cycle in the class graph is invalid AngelScript, but the walk must
    /// terminate on it rather than hang.
    #[test]
    fn a_cyclic_hierarchy_terminates() {
        let (same, cross) = both_ways(
            &["class A : B {}", "class B : A {}"],
            "void f(const A@ a) { a.Mutate(); }",
        );
        assert_eq!((same, cross), (0, 0), "no method exists anywhere");
    }

    // ── the two fixes together, at the fleet site ────────────────────────

    /// THE ONE FLEET SITE, in both its halves. `tm-map-together`'s
    /// `skins[i].WriteToNetworkBuffer(buf)` on a
    /// `const Editor::SetSkinSpec@[]@`, with the method inherited non-const
    /// from two levels up.
    ///
    /// It is silent before this ticket and silent after, and the reason
    /// changes completely: before, the parent walk was missing AND the
    /// element was wrongly const, two bugs cancelling; after, the walk
    /// exists and the element is correctly NOT a const receiver. Landing the
    /// walk alone would have turned the single site B116 measured across 151
    /// plugins into a false error.
    #[test]
    fn the_one_fleet_site_stays_silent_for_the_right_reason() {
        let decls = [
            "class NetworkSerializable { void WriteToNetworkBuffer(int b) {} }",
            "class Serializable : NetworkSerializable {}",
            "class SetSkinSpec : Serializable {}",
        ];
        let (same, cross) = both_ways(
            &decls,
            "void f(const SetSkinSpec@[]@ skins, int buf) { skins[0].WriteToNetworkBuffer(buf); }",
        );
        assert_eq!((same, cross), (0, 0), "the engine compiles this");

        // The same call on a NON-array const receiver DOES fire — which is
        // what proves the silence above comes from the element type and not
        // from the walk having quietly failed to find the method.
        let (same2, cross2) = both_ways(
            &decls,
            "void f(const SetSkinSpec@ s, int buf) { s.WriteToNetworkBuffer(buf); }",
        );
        assert_eq!(
            (same2, cross2),
            (1, 1),
            "the walk finds it two levels up when the receiver really is const"
        );
    }
}

// ── B130: the const-receiver rule reaches ENGINE receivers ──────────────────
//
// The last exclusion B116 put in place, removed only after both halves were
// measured: the false-positive surface on the fleet (807 sites, all const,
// none unknown, zero would-fire) and the correctness question against the game
// itself (`scripts/probe_engine_const_receiver.py`, three controls passing).
//
// The engine's own verdicts are pinned below as test expectations, so the
// rule's behaviour is anchored to what the compiler does rather than to what
// this checker was written to believe.
#[cfg(test)]
mod b130_engine_const_receiver_tests {
    use super::*;
    use crate::lexer::tokenize_filtered;
    use crate::parser::Parser;
    use crate::symbols::SymbolTable;

    fn typedb() -> crate::typedb::TypeIndex {
        let base =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
        crate::typedb::TypeIndex::load(
            &base.join("OpenplanetCore.json"),
            &base.join("OpenplanetNext.json"),
        )
        .expect("typedb fixtures must load")
    }

    fn check(src: &str, idx: &crate::typedb::TypeIndex) -> Vec<TypeDiagnostic> {
        let tokens = tokenize_filtered(src);
        let mut parser = Parser::new(&tokens, src);
        let file = parser.parse_file();
        let mut ws = SymbolTable::new();
        let fid = ws.allocate_file_id();
        let syms = SymbolTable::extract_symbols(fid, src, &file);
        ws.set_file_symbols(fid, syms);
        let scope = GlobalScope::new(&ws, Some(idx));
        let mut checker = Checker::new(src, &scope);
        checker.check_file(&file);
        checker.diagnostics
    }

    fn fired(diags: &[TypeDiagnostic]) -> usize {
        diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::NonConstMethodOnConstReceiver { .. }
                )
            })
            .count()
    }

    fn violations(diags: &[TypeDiagnostic]) -> usize {
        diags
            .iter()
            .filter(|d| {
                matches!(
                    &d.kind,
                    TypeDiagnosticKind::ConstViolation { .. }
                        | TypeDiagnosticKind::ReadOnlyReference
                )
            })
            .count()
    }

    /// THE GAME'S VERDICTS, transcribed. Each row was submitted to the live
    /// engine by `scripts/probe_engine_const_receiver.py` with three passing
    /// controls (`ctl-valid` ACCEPT, `ctl-bogus` REJECT, `ctl-own-nonconst`
    /// REJECT), and the engine's answer is what `want_fire` records.
    ///
    /// `MemoryBuffer::Seek` -> REJECT, "No matching signatures to
    ///     'MemoryBuffer::Seek(const int) const'"
    /// `MemoryBuffer::GetSize` -> ACCEPT
    /// `Json::Value::Add` -> REJECT, "No matching signatures to
    ///     'Value::Add(Json::Value@&) const'"
    /// `Json::Value::GetType` -> ACCEPT
    /// `string::ToLower` -> ACCEPT
    const ENGINE_ROWS: &[(&str, &str, bool)] = &[
        (
            "non-const engine method through a const handle",
            "void f(const MemoryBuffer@ b) { b.Seek(0); }",
            true,
        ),
        (
            "const engine method through a const handle",
            "void f(const MemoryBuffer@ b) { b.GetSize(); }",
            false,
        ),
        (
            "non-const engine method, second type family",
            "void f(const Json::Value@ j) { j.Add(Json::Value()); }",
            true,
        ),
        (
            "const engine method, second type family",
            "void f(const Json::Value@ j) { j.GetType(); }",
            false,
        ),
        (
            "const engine method on a const value receiver",
            "void f(const string &in s) { string t = s.ToLower(); }",
            false,
        ),
        (
            "non-const engine method on a NON-const receiver is fine",
            "void f(MemoryBuffer@ b) { b.Seek(0); }",
            false,
        ),
    ];

    #[test]
    fn engine_receivers_follow_the_engines_own_verdicts() {
        let idx = typedb();
        for (label, src, want) in ENGINE_ROWS {
            let d = check(src, &idx);
            assert_eq!(
                fired(&d) > 0,
                *want,
                "`{label}`: expected fire={want}\n  {src}\n  got {:?}",
                d
            );
        }
    }

    /// NON-VACUITY, both polarities with exact counts. The table above would
    /// pass with the rule deleted if every row expected `false`, and would
    /// pass with it hardwired on if every row expected `true`.
    #[test]
    fn the_two_polarities_produce_exactly_one_and_zero() {
        let idx = typedb();
        let fires = check("void f(const MemoryBuffer@ b) { b.Seek(0); }", &idx);
        assert_eq!(fired(&fires), 1, "{:?}", fires);
        let silent = check("void f(const MemoryBuffer@ b) { b.GetSize(); }", &idx);
        assert_eq!(fired(&silent), 0, "{:?}", silent);
    }

    /// An overload set carrying BOTH a const and a non-const member stays
    /// silent: the engine selects the const overload on a const receiver.
    /// `Json::Value::Get` and `opIndex` are exactly that shape in the shipped
    /// dump, and the fleet reaches `Get` 63 times — every one correctly quiet.
    #[test]
    fn a_const_overload_makes_the_call_legal() {
        let idx = typedb();
        for src in [
            "void f(const Json::Value@ j) { j.Get(\"k\"); }",
            "void f(const Json::Value@ j) { auto x = j[\"k\"]; }",
        ] {
            let d = check(src, &idx);
            assert_eq!(fired(&d), 0, "{src} -> {:?}", d);
        }
    }

    /// THE LINE THIS TICKET DOES NOT CROSS. Diagnosing the call and
    /// propagating constness out of its return value are separate questions
    /// with separate evidence. B121 measured propagation on engine returns at
    /// 10 false positives across 7 plugins — engine methods returning a FRESH
    /// value rather than a view — and its shape proxy removes only 7.
    ///
    /// So the fleet's own false-positive shapes must stay clean even now that
    /// the call check reaches engine receivers.
    #[test]
    fn engine_returns_still_propagate_no_constness() {
        let idx = typedb();
        for src in [
            "string f(const string &in raw) { auto r = raw.Replace(\"a\", \"b\"); r = r.Replace(\"c\", \"d\"); return r; }",
            "void f(const string &in term) { auto parts = term.Split(\" \"); parts[0] = \"x\"; }",
            "void f(const string &in s) { auto x = s.ToLower(); x = \"y\"; }",
        ] {
            let d = check(src, &idx);
            assert_eq!(
                violations(&d),
                0,
                "B121's fleet false positives must stay silent: {src} -> {:?}",
                d
            );
        }
    }

    /// A workspace class that SHADOWS an engine name is not an engine type,
    /// and its own constness must decide. `external_method_is_const` returns
    /// `None` for a shadowed name, so the workspace arms stay the authority.
    #[test]
    fn a_workspace_class_shadowing_an_engine_name_is_not_engine_checked() {
        let idx = typedb();
        // Workspace `MemoryBuffer` declares `Seek` as CONST — engine says
        // non-const. The workspace declaration must win, and stay silent.
        let d = check(
            "class MemoryBuffer { void Seek(int p) const {} } \
             void f(const MemoryBuffer@ b) { b.Seek(0); }",
            &idx,
        );
        assert_eq!(fired(&d), 0, "workspace shadow must decide: {:?}", d);
    }

    /// THE NADEO EXCLUSION, and the reason it is not "no metadata".
    ///
    /// The engine enforces const receivers for Nadeo types exactly as for
    /// Core — probed live, `const CMwNod@ n; n.MwAddRef()` gives
    /// "No matching signatures to 'CMwNod::MwAddRef() const'". So these are
    /// real errors we are choosing not to report.
    ///
    /// We choose that because the Nadeo LOADER hardcodes `is_const: false` on
    /// every method it builds (that dump carries no declaration text), so
    /// `false` there means UNKNOWN. Consuming it would fire on every
    /// correctly-written const Nadeo call — the same `bool`-defaults-to-a-lie
    /// defect B125 and B131 fixed on the Core side, surviving in the loader's
    /// other half.
    ///
    /// The fleet cannot police this: all 807 engine-receiver sites across 152
    /// plugins are Core types, so the Nadeo population has zero exposure
    /// there and a zero measured on it would mean nothing. This test is what
    /// holds the line instead.
    #[test]
    fn nadeo_receivers_are_excluded_because_their_constness_is_unknown() {
        let idx = typedb();
        for src in [
            "void f(const CMwNod@ n) { n.MwAddRef(); }",
            "void f(const CTrackMania@ app) { app.BackToMainMenu(); }",
        ] {
            let d = check(src, &idx);
            assert_eq!(
                fired(&d),
                0,
                "Nadeo `is_const` is hardcoded false = UNKNOWN: {src} -> {:?}",
                d
            );
        }
    }

    /// B137: WHY the Nadeo exclusion is right, stated more precisely than
    /// B130 managed — and correcting that ticket's reasoning.
    ///
    /// B130 said consuming the Nadeo `is_const` would fire "on every
    /// correctly-written const Nadeo call". Probed live, that population
    /// appears to be EMPTY: the engine rejected every Nadeo member reached
    /// through a const receiver, including two pure getters and a plain
    /// property read.
    ///
    ///   const CMwNod@              n.MwAddRef()        REJECT
    ///   CMwNod@ (non-const)        n.MwAddRef()        ACCEPT   <- isolates
    ///                                                              constness
    ///   const CGameCtnBlock@       b.IsGhostBlock()    REJECT
    ///   const CGameCtnMediaBlock@  b.GetKeysCount()    REJECT
    ///   const CGameCtnChallenge@   c.MapName (read)    REJECT
    ///        "Non-const method call on read-only object reference"
    ///
    /// So the old hardcoded `false` was, as it happens, factually right on
    /// every shape probed — consuming it would have produced true positives,
    /// not false ones. B130's danger was overstated and this records that.
    ///
    /// The exclusion stands anyway, for the reason that does survive: a value
    /// that is right by coincidence is not knowledge. Five probed members do
    /// not license a claim about 3084, and the dump genuinely does not say —
    /// so `None` is the honest state and a caller must decline on it. If a
    /// wider probe ever establishes that Openplanet registers NO Nadeo member
    /// as const, `Some(false)` becomes defensible and this rule can fire; that
    /// is a measurement nobody has made.
    ///
    /// The practical stakes are small in either direction, and the probe is
    /// what shows it: a `const CMwNod@` receiver cannot even be READ through,
    /// so almost no correct code has one. That is exactly why the fleet has
    /// zero Nadeo const-receiver sites — not an accident of the corpus.
    #[test]
    fn nadeo_constness_is_unknown_so_the_rule_declines() {
        let idx = typedb();
        for src in [
            "void f(const CGameCtnBlock@ b) { b.IsGhostBlock(); }",
            "void f(const CGameCtnMediaBlock@ b) { b.GetKeysCount(); }",
            "void f(const CMwNod@ n) { n.MwAddRef(); }",
        ] {
            let d = check(src, &idx);
            assert_eq!(
                fired(&d),
                0,
                "Nadeo constness is UNKNOWN, so the rule must decline rather                  than guess: {src} -> {:?}",
                d
            );
        }
    }

    /// The three-valued aggregation, at the surface that consumes it. Core
    /// answers; Nadeo declines; and the two are distinguishable, which is the
    /// whole point of B137's `Option<bool>`.
    #[test]
    fn external_method_is_const_distinguishes_known_from_unknown() {
        let idx = typedb();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&idx));
        // Core: a real answer, both polarities.
        assert_eq!(
            scope.external_method_is_const("string", "ToLower"),
            Some(true)
        );
        assert_eq!(
            scope.external_method_is_const("MemoryBuffer", "Seek"),
            Some(false)
        );
        // Nadeo: the database does not say.
        assert_eq!(
            scope.external_method_is_const("Game::CGameCtnBlock", "IsGhostBlock"),
            None,
            "the Nadeo dump carries no signature text, so this must be UNKNOWN              rather than `Some(false)`"
        );
        // Not a method of anything.
        assert_eq!(
            scope.external_method_is_const("string", "NoSuchThing"),
            None
        );
    }

    /// One diagnostic, not two. The engine arm sits downstream of the
    /// workspace own-class and inherited arms, and a receiver that is an
    /// engine type reaches none of them — but the count is asserted rather
    /// than assumed, because a double emit at one span is invisible through
    /// the CLI, which dedups by span+kind.
    #[test]
    fn an_engine_receiver_fires_exactly_once() {
        let idx = typedb();
        let d = check(
            "void f(const Json::Value@ j) { j.Add(Json::Value()); }",
            &idx,
        );
        assert_eq!(fired(&d), 1, "exactly one diagnostic expected: {:?}", d);
    }
}

/// B150: a bare member access is a PROPERTY READ, not a method reference,
/// whenever the receiver's class exposes a `get_<member>` accessor.
///
/// The eight `Mw*` engine containers register BOTH `uint Length() const` and
/// `uint get_Length() const`; the script `array`, `string` and `wstring`
/// register only `get_Length`; `vec2`/`vec3`/`vec4`/`quat` register only
/// `Length()`. That asymmetry is in the shipped typedb, not in any loader.
/// Because a real `Length` method exists on the containers, `method_ref_sigs`
/// used to hand B023 a callable — so `Math::Min(uint(20), buf.Length)` was
/// REJECTED outright, and B108 then spelled the argument `uint()`.
#[cfg(test)]
mod b150_container_length_property_tests {
    use super::*;
    use crate::lexer::tokenize_filtered;
    use crate::parser::Parser;
    use crate::symbols::SymbolTable;

    /// Engine containers that ship BOTH the method and the accessor — every
    /// one of them takes the same rule, so the fix cannot be special-cased
    /// to the one container Max happened to hit. This is the complete set in
    /// the shipped dump, enumerated from it rather than guessed: `MwRefBuffer`
    /// belongs even though `repr.rs`'s `is_array_generic_base` omits it, and
    /// `MwFastBufferCat` does NOT (it registers neither spelling), even
    /// though that list includes it.
    const BOTH_METHOD_AND_ACCESSOR: &[&str] = &[
        "MwFastBuffer",
        "MwFastArray",
        "MwSArray",
        "MwStridedArray",
        "MwNodPool",
        "MwVirtualArray",
        "MwArrayInPlaceDyn",
        "MwRefBuffer",
    ];

    fn typedb() -> crate::typedb::TypeIndex {
        let base =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
        crate::typedb::TypeIndex::load(
            &base.join("OpenplanetCore.json"),
            &base.join("OpenplanetNext.json"),
        )
        .expect("typedb fixtures must load")
    }

    fn check(src: &str, idx: &crate::typedb::TypeIndex) -> Vec<TypeDiagnostic> {
        let mut ws = SymbolTable::new();
        let tokens = tokenize_filtered(src);
        let mut parser = Parser::new(&tokens, src);
        let file = parser.parse_file();
        let fid = ws.allocate_file_id();
        let syms = SymbolTable::extract_symbols(fid, src, &file);
        ws.set_file_symbols(fid, syms);
        let scope = GlobalScope::new(&ws, Some(idx));
        let mut checker = Checker::new(src, &scope);
        checker.check_file(&file);
        checker.diagnostics
    }

    /// Every `No matching signatures` message the source produced.
    fn no_match_texts(diags: &[TypeDiagnostic]) -> Vec<String> {
        diags
            .iter()
            .filter_map(|d| match &d.kind {
                TypeDiagnosticKind::NoMatchingSignatures { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// Sanity: the fixture typedb really does carry the asymmetry this whole
    /// ticket rests on. Without this, a green run below could mean "the rule
    /// works" or "the fixture has no `Length` method to trip over", and those
    /// are not the same result.
    ///
    /// Asserted on the CONSTRUCTED `OverloadSig` — its return type and its
    /// parameter count — not on any rendered string (B136 / B144: a test that
    /// compares types by their rendering cannot detect a structural defect).
    /// This is also what rules out the B144-shaped explanation: `uint()` is
    /// not a mis-parsed type STRING. `funcdef_binding_source_display` builds
    /// it with `format!("{}({})", sig.return_type, sig.param_types.join(", "))`
    /// from a sig that is structurally correct — return `uint`, zero params —
    /// so `parse_type_string` is never on this path at all. The defect was
    /// upstream of the rendering: the checker decided a property read was a
    /// callable, and then rendered that decision faithfully.
    #[test]
    fn the_fixture_typedb_carries_the_method_and_accessor_asymmetry() {
        let idx = typedb();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&idx));
        for container in BOTH_METHOD_AND_ACCESSOR {
            let sigs = scope.callables_method(container, "Length");
            assert_eq!(
                sigs.len(),
                1,
                "{container} must ship exactly one real `Length()` method — \
                 that is what makes this a bug and not a non-event"
            );
            // The exact shape B108 renders as `uint()`: zero params, uint
            // return. Structural, so a rendering change cannot mask it.
            assert_eq!(
                sigs[0].param_types.len(),
                0,
                "{container}::Length must be the ZERO-ARG method whose \
                 signature renders as `uint()`"
            );
            assert_eq!(
                sigs[0].return_type, "uint",
                "{container}::Length must return uint"
            );
            assert!(
                !scope.callables_method(container, "get_Length").is_empty(),
                "{container} must ship a `get_Length` accessor"
            );
        }
        assert!(
            scope.callables_method("array", "Length").is_empty(),
            "script `array` must have NO `Length` method — it is the control"
        );
        assert!(
            !scope.callables_method("array", "get_Length").is_empty(),
            "script `array` must ship `get_Length`"
        );
        assert!(
            !scope.callables_method("vec3", "Length").is_empty(),
            "vec3 must ship a real `Length()` method"
        );
        assert!(
            scope.callables_method("vec3", "get_Length").is_empty(),
            "vec3 must have NO `get_Length` — it is the negative control that \
             keeps B108 honest"
        );
    }

    /// The ticket's live shape, generalised over every affected container and
    /// over several call sites: a `.Length` read passed as an argument is a
    /// uint VALUE, so nothing is rejected and nothing renders as `uint()`.
    #[test]
    fn a_container_length_argument_is_a_value_for_every_container_and_call() {
        let idx = typedb();
        for container in BOTH_METHOD_AND_ACCESSOR {
            for call in [
                "Math::Min(uint(20), b.Length)",
                "Math::Max(uint(20), b.Length)",
                "Math::Clamp(uint(1), uint(2), b.Length)",
            ] {
                let src = format!("void f({container}<int> b) {{ {call}; }}");
                let texts = no_match_texts(&check(&src, &idx));
                assert!(
                    texts.is_empty(),
                    "{src}\n  must not reject a uint property read, got {texts:?}"
                );
            }
        }
    }

    /// The same read in every non-argument position it already worked in —
    /// the no-regression rows. These were silent before B150 and must stay
    /// silent, or the fix traded one false positive for another.
    #[test]
    fn a_container_length_read_stays_a_value_in_every_other_position() {
        let idx = typedb();
        for container in BOTH_METHOD_AND_ACCESSOR {
            for body in [
                "uint n = b.Length;",
                "int n = int(b.Length);",
                "uint n = uint(b.Length);",
                "if (b.Length > 3) {}",
                "for (uint i = 0; i < b.Length; i++) {}",
                "uint n = b.Length + 1;",
                "while (b.Length > 0) { break; }",
            ] {
                let src = format!("void f({container}<int> b) {{ {body} }}");
                let texts = no_match_texts(&check(&src, &idx));
                assert!(texts.is_empty(), "{src}\n  must stay silent, got {texts:?}");
            }
        }
    }

    /// Max's own discriminator, kept as a row: assigning to a `uint` local
    /// first was already silent, and must remain so. If this ever fires, the
    /// fix has broken the shape that PROVED the bug.
    #[test]
    fn the_assign_to_a_local_discriminator_stays_silent() {
        let idx = typedb();
        for container in BOTH_METHOD_AND_ACCESSOR {
            let src = format!(
                "void f({container}<int> b) {{ uint total = b.Length; \
                 Math::Min(uint(20), total); }}"
            );
            assert!(
                no_match_texts(&check(&src, &idx)).is_empty(),
                "{src}\n  the discriminator must stay silent"
            );
        }
    }

    /// Control: containers that ship ONLY the accessor were never affected,
    /// and still are not.
    #[test]
    fn accessor_only_containers_were_never_affected() {
        let idx = typedb();
        for src in [
            "void f(array<int>@ a) { Math::Min(uint(20), a.Length); }",
            "void f(string s) { Math::Min(uint(20), s.Length); }",
            "void f(array<int>@ a) { for (uint i = 0; i < a.Length; i++) {} }",
        ] {
            assert!(
                no_match_texts(&check(src, &idx)).is_empty(),
                "{src}\n  accessor-only receivers must stay silent"
            );
        }
    }

    /// NEGATIVE CONTROL, asserted on presence rather than absence: a class
    /// with a real zero-arg method and NO accessor still yields a method
    /// reference, still rejects, and still renders B108's `type()` spelling.
    /// `vec2`/`vec3`/`vec4`/`quat` are shipped data, not a contrivance.
    #[test]
    fn a_genuine_zero_arg_method_reference_still_renders_its_signature() {
        let idx = typedb();
        for vec_ty in ["vec2", "vec3", "vec4", "quat"] {
            let src = format!("void f({vec_ty} v) {{ Math::Min(1.0f, v.Length); }}");
            let texts = no_match_texts(&check(&src, &idx));
            assert_eq!(
                texts.len(),
                1,
                "{src}\n  a genuine method reference must still be rejected, got {texts:?}"
            );
            assert!(
                texts[0].contains("float()"),
                "{src}\n  B108's signature spelling must survive, got {texts:?}"
            );
        }
    }

    /// `vec2`/`vec3`/`vec4`/`quat` are shipped data, not a contrivance.
    #[test]
    fn math_min_rejects_vector_arguments_without_a_vector_binding() {
        let idx = typedb();
        for vec_ty in ["vec2", "vec3", "vec4", "quat"] {
            let src = format!("void f() {{ {vec_ty} a, b; Math::Min(a, b); }}");
            let texts = no_match_texts(&check(&src, &idx));
            assert_eq!(texts.len(), 1, "{src} must reject: {texts:?}");
        }
        for src in [
            "void f() { int a, b; Math::Min(a, b); }",
            "void f() { float a, b; Math::Min(a, b); }",
        ] {
            assert!(
                no_match_texts(&check(src, &idx)).is_empty(),
                "{src} must bind"
            );
        }
    }

    /// workspace class with both spellings takes the property read, and one
    /// with only the method still yields a method reference. Stated for a
    /// non-`Length` name so nothing can be passing on the member's name.
    #[test]
    fn the_accessor_rule_is_the_same_for_workspace_classes() {
        let idx = typedb();

        let both = "class W { uint Size() { return 1; } uint get_Size() { return 2; } } \
                    void f(W@ w) { Math::Min(uint(1), w.Size); }";
        assert!(
            no_match_texts(&check(both, &idx)).is_empty(),
            "a workspace class exposing `get_Size` makes `w.Size` a property read"
        );

        let method_only = "class M { uint Size() { return 1; } } \
                           void f(M@ m) { Math::Min(uint(1), m.Size); }";
        let texts = no_match_texts(&check(method_only, &idx));
        assert_eq!(
            texts.len(),
            1,
            "with no accessor, `m.Size` is still a method reference, got {texts:?}"
        );
        assert!(
            texts[0].contains("uint()"),
            "and still renders its signature, got {texts:?}"
        );
    }

    /// The accessor SPELLING is untouched: `b.get_Length` names the accessor
    /// method itself, and whatever the checker made of that before B150 it
    /// must still make of it — the rule keys on `get_<member>` existing, and
    /// `get_get_Length` does not exist, so this arm can never be entered.
    #[test]
    fn an_explicit_accessor_reference_is_unchanged() {
        let idx = typedb();
        let ws = SymbolTable::new();
        let scope = GlobalScope::new(&ws, Some(&idx));
        assert!(
            scope
                .callables_method("MwFastBuffer", "get_get_Length")
                .is_empty(),
            "premise: there is no accessor for the accessor, so the new rule \
             cannot fire on this spelling"
        );
        let src = "void f(MwFastBuffer<int> b) { Math::Min(uint(20), b.get_Length); }";
        assert!(
            no_match_texts(&check(src, &idx)).is_empty(),
            "the explicit accessor spelling keeps its pre-B150 behaviour"
        );
    }
}
