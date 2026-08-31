//! Canonical internal representation of resolved types.
//!
//! This is the shape the (future) type-checker works with. `TypeExpr` is the
//! syntactic form from the parser; `TypeRepr` is the semantic form after
//! name-resolution has happened. Round-trippable to source via `display()`.

use crate::lexer::TokenKind;

/// Canonical internal representation of a resolved type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeRepr {
    /// Primitive keyword types. Use the same kinds AngelScript exposes.
    Primitive(PrimitiveType),
    /// void — only valid as a return type.
    Void,
    /// An anonymous null value (the `null` literal before handle coercion).
    Null,
    /// A handle to another type: `T@`.
    Handle(Box<TypeRepr>),
    /// A const-qualified view of a type.
    Const(Box<TypeRepr>),
    /// A user-defined or external named type, referenced by fully qualified name.
    /// e.g. "Net::HttpRequest", "vec3", "MyClass".
    Named(String),
    /// A generic instantiation: e.g. `array<int>`, `dictionary<string,int>`.
    Generic { base: String, args: Vec<TypeRepr> },
    /// Plain array shorthand: `T[]` — equivalent to
    /// `Generic { base:"array", args:[T] }` but kept distinct so we can
    /// round-trip source form.
    Array(Box<TypeRepr>),
    /// A funcdef / function-pointer type referenced by qualified name.
    Funcdef(String),
    /// Resolution failed — contains the attempted name for diagnostics.
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrimitiveType {
    Bool,
    Int8,
    Int16,
    Int,
    Int64,
    Uint8,
    Uint16,
    Uint,
    Uint64,
    Float,
    Double,
    String,
}

impl PrimitiveType {
    /// Canonical source-form keyword for this primitive.
    pub fn as_str(self) -> &'static str {
        match self {
            PrimitiveType::Bool => "bool",
            PrimitiveType::Int8 => "int8",
            PrimitiveType::Int16 => "int16",
            PrimitiveType::Int => "int",
            PrimitiveType::Int64 => "int64",
            PrimitiveType::Uint8 => "uint8",
            PrimitiveType::Uint16 => "uint16",
            PrimitiveType::Uint => "uint",
            PrimitiveType::Uint64 => "uint64",
            PrimitiveType::Float => "float",
            PrimitiveType::Double => "double",
            PrimitiveType::String => "string",
        }
    }

    /// Map a lexer `TokenKind` to its primitive enum, if one exists.
    ///
    /// `KwVoid` and `KwAuto` are intentionally not mapped here — they aren't
    /// primitive value types in the `TypeRepr` sense and have their own
    /// variants / placeholders.
    ///
    /// Note: `int32`/`uint32` are treated as aliases for `int`/`uint`,
    /// matching AngelScript's semantics where the unsized forms are 32-bit.
    pub fn from_token_kind(kind: TokenKind) -> Option<Self> {
        Some(match kind {
            TokenKind::KwBool => PrimitiveType::Bool,
            TokenKind::KwInt8 => PrimitiveType::Int8,
            TokenKind::KwInt16 => PrimitiveType::Int16,
            TokenKind::KwInt => PrimitiveType::Int,
            TokenKind::KwInt32 => PrimitiveType::Int,
            TokenKind::KwInt64 => PrimitiveType::Int64,
            TokenKind::KwUint8 => PrimitiveType::Uint8,
            TokenKind::KwUint16 => PrimitiveType::Uint16,
            TokenKind::KwUint => PrimitiveType::Uint,
            TokenKind::KwUint32 => PrimitiveType::Uint,
            TokenKind::KwUint64 => PrimitiveType::Uint64,
            TokenKind::KwFloat => PrimitiveType::Float,
            TokenKind::KwDouble => PrimitiveType::Double,
            TokenKind::KwString => PrimitiveType::String,
            _ => return None,
        })
    }

    /// Map an identifier-as-text to its primitive enum, if one exists.
    /// Useful when an external type database references primitives by name
    /// instead of by token.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "bool" => PrimitiveType::Bool,
            "int8" => PrimitiveType::Int8,
            "int16" => PrimitiveType::Int16,
            "int" | "int32" => PrimitiveType::Int,
            "int64" => PrimitiveType::Int64,
            "uint8" => PrimitiveType::Uint8,
            "uint16" => PrimitiveType::Uint16,
            "uint" | "uint32" => PrimitiveType::Uint,
            "uint64" => PrimitiveType::Uint64,
            "float" => PrimitiveType::Float,
            "double" => PrimitiveType::Double,
            "string" => PrimitiveType::String,
            _ => return None,
        })
    }
}

impl TypeRepr {
    /// Display the canonical source form: `int`, `string@`, `array<int>`, etc.
    pub fn display(&self) -> String {
        match self {
            TypeRepr::Primitive(p) => p.as_str().to_string(),
            TypeRepr::Void => "void".to_string(),
            TypeRepr::Null => "null".to_string(),
            TypeRepr::Handle(inner) => format!("{}@", inner.display()),
            // B144: AngelScript spells a const HANDLE with the `const`
            // AFTER the `@`. Rendering `Const(Handle(T))` as `const T@`
            // printed the OTHER type — `Handle(Const(T))`, a handle to a
            // const object — so the two collapsed onto one string and no
            // display-based assertion could tell them apart. See B136: a
            // test that compares types by rendered string cannot detect a
            // structural type bug, and this was the rendering that made
            // that possible.
            TypeRepr::Const(inner) => match inner.as_ref() {
                TypeRepr::Handle(_) => format!("{} const", inner.display()),
                _ => format!("const {}", inner.display()),
            },
            TypeRepr::Named(name) => name.clone(),
            TypeRepr::Generic { base, args } => {
                let joined = args
                    .iter()
                    .map(|a| a.display())
                    .collect::<Vec<_>>()
                    .join(", ");
                if args.is_empty() {
                    base.clone()
                } else {
                    format!("{}<{}>", base, joined)
                }
            }
            TypeRepr::Array(inner) => format!("{}[]", inner.display()),
            TypeRepr::Funcdef(name) => name.clone(),
            TypeRepr::Error(name) => {
                if name.is_empty() {
                    "<error>".to_string()
                } else {
                    format!("<error:{}>", name)
                }
            }
        }
    }

    /// Strip a `Handle` wrapper to get the referent type. For non-handles
    /// returns `self` unchanged.
    pub fn unwrap_handle(&self) -> &TypeRepr {
        match self {
            TypeRepr::Handle(inner) => inner,
            other => other,
        }
    }

    /// Strip a `Const` wrapper. For non-const types returns `self` unchanged.
    pub fn unwrap_const(&self) -> &TypeRepr {
        match self {
            TypeRepr::Const(inner) => inner,
            other => other,
        }
    }

    /// If `self` (after stripping `Const`/`Handle`) is an array type —
    /// either `Array(T)` or `Generic { base: "array", args: [T] }` —
    /// return the element type. Otherwise `None`.
    pub fn array_element_type(&self) -> Option<&TypeRepr> {
        // Peel both orderings of `Const`/`Handle` (see AC20). `const Foo@`
        // becomes `Handle(Const(Foo))`, whereas `Foo@ const` becomes
        // `Const(Handle(Foo))`.
        let inner = self
            .unwrap_const()
            .unwrap_handle()
            .unwrap_const()
            .unwrap_handle();
        match inner {
            TypeRepr::Array(elem) => Some(elem),
            TypeRepr::Generic { base, args } if is_array_generic_base(base) && args.len() == 1 => {
                Some(&args[0])
            }
            _ => None,
        }
    }

    /// True if `self` (after stripping `Const`/`Handle`) is an array-like
    /// generic, regardless of whether the element type is resolved.
    pub fn is_array_like(&self) -> bool {
        let inner = self
            .unwrap_const()
            .unwrap_handle()
            .unwrap_const()
            .unwrap_handle();
        matches!(inner, TypeRepr::Array(_))
            || matches!(inner, TypeRepr::Generic { base, .. } if is_array_generic_base(base))
    }

    /// True if `self` (after stripping `Const`/`Handle`) is a dictionary
    /// type (`Generic { base: "dictionary", .. }` or a bare
    /// `Named("dictionary")` that slipped through).
    pub fn is_dictionary_like(&self) -> bool {
        let inner = self
            .unwrap_const()
            .unwrap_handle()
            .unwrap_const()
            .unwrap_handle();
        match inner {
            TypeRepr::Generic { base, .. } if base == "dictionary" => true,
            TypeRepr::Named(n) if n == "dictionary" => true,
            _ => false,
        }
    }

    /// True for `Null` and for handles to an errored type. A nullish value
    /// is coercible to any handle slot when filling in holes.
    pub fn is_nullish(&self) -> bool {
        match self {
            TypeRepr::Null => true,
            TypeRepr::Handle(inner) => matches!(inner.as_ref(), TypeRepr::Error(_)),
            _ => false,
        }
    }

    /// True if this is (or wraps) a resolution `Error`.
    pub fn is_error(&self) -> bool {
        match self {
            TypeRepr::Error(_) => true,
            TypeRepr::Handle(inner) | TypeRepr::Const(inner) | TypeRepr::Array(inner) => {
                inner.is_error()
            }
            TypeRepr::Generic { args, .. } => args.iter().any(|a| a.is_error()),
            _ => false,
        }
    }

    /// Pragmatic parser for the `type_name` strings produced by the external
    /// type database (e.g. `"int"`, `"const Foo@"`, `"array<int>"`, `"Foo[]"`,
    /// `"dictionary<string,int>"`).
    ///
    /// This is not a full type-expression parser — it's meant to recognize the
    /// shapes that show up in the Core / Nadeo JSON dumps and fall back to
    /// `Named(s)` on anything unrecognized. It never fails.
    pub fn parse_type_string(s: &str) -> TypeRepr {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return TypeRepr::Error(String::new());
        }
        parse_type_string_inner(trimmed)
    }
}

/// Parameter passing mode (B027). Delegate construction requires EXACT
/// mode equality — game probe (RemoteBuild 2026-08-29, OP 1.29.5):
/// plain, `&in`, `&out` and object-typed `&inout` are all enforced;
/// `int &inout` is not even declarable ("Only object types that support
/// object handles can use &inout").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamMode {
    Value,
    In,
    Out,
    Inout,
}

/// Mode from param type text. Workspace param text carries the source
/// `&in`/`&out`/`&inout` decoration; typedb `typedecl` text does not —
/// its mode comes from the `typeflags` bits, surfaced as flags on
/// `typedb::ParamInfo`.
pub fn param_mode_from_text(text: &str) -> ParamMode {
    if text.contains("&inout") {
        ParamMode::Inout
    } else if text.contains("&out") {
        ParamMode::Out
    } else if text.contains("&in") {
        ParamMode::In
    } else {
        ParamMode::Value
    }
}

/// True for generic base names that behave as AngelScript arrays when they
/// appear in typedb member type strings: the script-side `array<T>` plus
/// the Nadeo engine container generics (`MwFastArray<CControlBase@>` etc.)
/// exposed on engine classes. Indexing yields the element type and
/// `Length` / `IsEmpty` are the array accessors (GH #50).
///
/// B156: membership is decided by the dump, not by judgement — a class
/// belongs iff it exposes an `opIndex` returning the template parameter `T`,
/// which is this doc's own "indexing yields the element type" read off the
/// data. `b156_array_generic_list_audit` re-derives the set on every run and
/// fails if [`ARRAY_GENERIC_BASES`] drifts from it in either direction.
/// The set itself, as a named value rather than a `matches!` arm, so the
/// B156 audit below can compare THE VALUE THE CHECKER USES against the dump.
/// A source-parsed or hand-copied list could drift from the one in force;
/// this one cannot.
pub(crate) const ARRAY_GENERIC_BASES: &[&str] = &[
    "array",
    "MwArrayInPlaceDyn",
    "MwFastArray",
    "MwFastBuffer",
    "MwNodPool",
    // B156: the dump registers `T& opIndex(uint)` here, same as its
    // siblings. It was missing, so indexing an `MwRefBuffer<T>` yielded no
    // element type at all.
    "MwRefBuffer",
    "MwSArray",
    "MwStridedArray",
    "MwVirtualArray",
    // B156: `MwFastBufferCat` was here and is NOT a container — the dump
    // gives it no methods whatsoever, only a constructor and a destructor.
    // Listing it meant `cat[0]` was confidently typed as `T` for a class the
    // engine cannot index at all.
];

fn is_array_generic_base(base: &str) -> bool {
    ARRAY_GENERIC_BASES.contains(&base)
}

fn parse_type_string_inner(s: &str) -> TypeRepr {
    let s = s.trim();

    // `const X` → const binds to the underlying type. If `X` names a
    // handle, the const sticks to the pointee: `const Foo@` →
    // `Handle(Const(Foo))`. If `X` has a trailing reference modifier, the
    // const sticks inside the reference for parity with the parser's
    // `Reference` wrapper (which we strip below). Otherwise, plain
    // `const X` → `Const(X)`.
    if let Some(rest) = s.strip_prefix("const ") {
        let inner = parse_type_string_inner(rest.trim());
        return match inner {
            TypeRepr::Handle(pointee) => TypeRepr::Handle(Box::new(TypeRepr::Const(pointee))),
            // B144: `const Foo@ const`. The recursion has already produced
            // the CONST HANDLE `Const(Handle(Foo))`; this leading `const`
            // is the OTHER qualifier and binds to the POINTEE, so it goes
            // inside the handle while the const-handle wrapper stays
            // outside. Without this arm the two would stack as
            // `Const(Const(Handle(Foo)))` and the pointee — the half that
            // actually restricts what may be done through the handle —
            // would be silently unqualified.
            TypeRepr::Const(held) => match *held {
                TypeRepr::Handle(pointee) => TypeRepr::Const(Box::new(TypeRepr::Handle(Box::new(
                    TypeRepr::Const(pointee),
                )))),
                already_const => TypeRepr::Const(Box::new(already_const)),
            },
            other => TypeRepr::Const(Box::new(other)),
        };
    }
    // A trailing reference modifier (`T &in`, `T &out`, `T &inout`, `T&`) is
    // a calling-convention decoration, not part of the value type. Strip it.
    if let Some(idx) = s.rfind('&') {
        // Only strip if what follows is empty or a known in/out tag.
        let tail = s[idx + 1..].trim();
        if tail.is_empty() || tail == "in" || tail == "out" || tail == "inout" {
            return parse_type_string_inner(s[..idx].trim());
        }
    }
    // B144: a TRAILING `const` is a CONST HANDLE — a handle that may not be
    // rebound, pointing at an object that may still be mutated. It is a
    // different qualifier from the LEADING `const`, which makes the POINTEE
    // read-only, and AngelScript can spell both at once
    // (`const Foo@ const`). `Const(Handle(T))` is the shape the source
    // parser has produced for it since AC20, and which `receiver_is_const`,
    // `base_type_name` and `array_element_type` were all written against;
    // this string path simply never built it, so `Foo@ const` fell through
    // to the `Named` arm below as `Named("Foo@ const")` — a type carrying
    // its own qualifiers in its NAME, and therefore equal to nothing. That
    // is B144: every funcdef signature comparison against a const-handle
    // parameter failed, on code the engine compiles.
    //
    // Only legal after a `@`: the engine rejects `int const &in` and
    // `vec3 const &in` at the parser ("Instead found reserved keyword
    // 'const'", 2026-09-01, OP 1.29.14), so requiring the handle also stops
    // this rule from eating a type whose name merely ends in those letters.
    if let Some(head) = s.strip_suffix("const") {
        let head = head.trim_end();
        if head.ends_with('@') {
            return TypeRepr::Const(Box::new(parse_type_string_inner(head)));
        }
    }
    // Trailing `@` → Handle
    if let Some(stripped) = s.strip_suffix('@') {
        return TypeRepr::Handle(Box::new(parse_type_string_inner(stripped.trim())));
    }
    // Trailing `[]` → Array
    if let Some(stripped) = s.strip_suffix("[]") {
        return TypeRepr::Array(Box::new(parse_type_string_inner(stripped.trim())));
    }
    // `base<...>` → Generic. Only recognize when the outer brackets are
    // balanced and the whole string ends with `>`.
    if s.ends_with('>') {
        if let Some(lt) = s.find('<') {
            let base = s[..lt].trim().to_string();
            let inner = &s[lt + 1..s.len() - 1];
            // Split by commas at depth 0.
            let mut args: Vec<TypeRepr> = Vec::new();
            let mut depth: i32 = 0;
            let mut last = 0usize;
            let bytes = inner.as_bytes();
            for (i, b) in bytes.iter().enumerate() {
                match b {
                    b'<' => depth += 1,
                    b'>' => depth -= 1,
                    b',' if depth == 0 => {
                        args.push(parse_type_string_inner(inner[last..i].trim()));
                        last = i + 1;
                    }
                    _ => {}
                }
            }
            if !inner[last..].trim().is_empty() {
                args.push(parse_type_string_inner(inner[last..].trim()));
            }
            if !base.is_empty() {
                // Canonicalize `array<T>` → `Array(T)` so downstream
                // code only has to handle one shape for the built-in
                // generic array. `dictionary<K,V>` stays as `Generic`.
                if base == "array" && args.len() == 1 {
                    let mut args = args;
                    return TypeRepr::Array(Box::new(args.remove(0)));
                }
                return TypeRepr::Generic { base, args };
            }
        }
    }
    // Void
    if s == "void" {
        return TypeRepr::Void;
    }
    // Primitive?
    if let Some(prim) = PrimitiveType::from_name(s) {
        return TypeRepr::Primitive(prim);
    }
    // Validate that what's left looks like a qualified name (letters /
    // digits / `_` / `::`). If not, emit as `Named` anyway — pragmatism
    // over strictness.
    TypeRepr::Named(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_primitive_int() {
        assert_eq!(TypeRepr::Primitive(PrimitiveType::Int).display(), "int");
    }

    #[test]
    fn display_handle_named() {
        let t = TypeRepr::Handle(Box::new(TypeRepr::Named("Foo".into())));
        assert_eq!(t.display(), "Foo@");
    }

    #[test]
    fn display_generic_array_int() {
        let t = TypeRepr::Generic {
            base: "array".into(),
            args: vec![TypeRepr::Primitive(PrimitiveType::Int)],
        };
        assert_eq!(t.display(), "array<int>");
    }

    #[test]
    fn display_array_of_handle() {
        let t = TypeRepr::Array(Box::new(TypeRepr::Handle(Box::new(TypeRepr::Named(
            "Foo".into(),
        )))));
        assert_eq!(t.display(), "Foo@[]");
    }

    /// B144. `Const(Handle(T))` is a CONST HANDLE, which AngelScript spells
    /// with the `const` AFTER the `@`. Rendering it as `const Foo@` printed
    /// the OTHER type — handle to a const object — so the two positions
    /// collapsed onto one string and no display-based assertion anywhere
    /// could tell them apart. Round-tripping is the property that matters:
    /// `parse_type_string(t.display()) == t` for every const position.
    #[test]
    fn display_const_handle() {
        let t = TypeRepr::Const(Box::new(TypeRepr::Handle(Box::new(TypeRepr::Named(
            "Foo".into(),
        )))));
        assert_eq!(t.display(), "Foo@ const");

        let handle_to_const = TypeRepr::Handle(Box::new(TypeRepr::Const(Box::new(
            TypeRepr::Named("Foo".into()),
        ))));
        assert_eq!(handle_to_const.display(), "const Foo@");

        let both = TypeRepr::Const(Box::new(TypeRepr::Handle(Box::new(TypeRepr::Const(
            Box::new(TypeRepr::Named("Foo".into())),
        )))));
        assert_eq!(both.display(), "const Foo@ const");

        for t in [t, handle_to_const, both] {
            assert_eq!(
                TypeRepr::parse_type_string(&t.display()),
                t,
                "display must round-trip through parse_type_string"
            );
        }
    }

    #[test]
    fn display_dictionary_two_args() {
        let t = TypeRepr::Generic {
            base: "dictionary".into(),
            args: vec![
                TypeRepr::Primitive(PrimitiveType::String),
                TypeRepr::Primitive(PrimitiveType::Int),
            ],
        };
        assert_eq!(t.display(), "dictionary<string, int>");
    }

    #[test]
    fn display_void_null_error() {
        assert_eq!(TypeRepr::Void.display(), "void");
        assert_eq!(TypeRepr::Null.display(), "null");
        assert_eq!(TypeRepr::Error(String::new()).display(), "<error>");
        assert_eq!(TypeRepr::Error("Nope".into()).display(), "<error:Nope>");
    }

    #[test]
    fn primitive_from_token_kind_matches() {
        assert_eq!(
            PrimitiveType::from_token_kind(TokenKind::KwInt),
            Some(PrimitiveType::Int)
        );
        assert_eq!(
            PrimitiveType::from_token_kind(TokenKind::KwBool),
            Some(PrimitiveType::Bool)
        );
        assert_eq!(
            PrimitiveType::from_token_kind(TokenKind::KwInt64),
            Some(PrimitiveType::Int64)
        );
        assert_eq!(
            PrimitiveType::from_token_kind(TokenKind::KwUint32),
            Some(PrimitiveType::Uint)
        );
        assert_eq!(
            PrimitiveType::from_token_kind(TokenKind::KwString),
            Some(PrimitiveType::String)
        );
    }

    #[test]
    fn primitive_from_token_kind_rejects_non_primitive() {
        assert_eq!(PrimitiveType::from_token_kind(TokenKind::KwVoid), None);
        assert_eq!(PrimitiveType::from_token_kind(TokenKind::KwAuto), None);
        assert_eq!(PrimitiveType::from_token_kind(TokenKind::Ident), None);
    }

    #[test]
    fn primitive_from_name_matches() {
        assert_eq!(PrimitiveType::from_name("int"), Some(PrimitiveType::Int));
        assert_eq!(PrimitiveType::from_name("int32"), Some(PrimitiveType::Int));
        assert_eq!(
            PrimitiveType::from_name("string"),
            Some(PrimitiveType::String)
        );
        assert_eq!(PrimitiveType::from_name("Foo"), None);
    }

    #[test]
    fn unwrap_helpers() {
        let named = TypeRepr::Named("Foo".into());
        let handle = TypeRepr::Handle(Box::new(named.clone()));
        assert_eq!(handle.unwrap_handle(), &named);
        assert_eq!(named.unwrap_handle(), &named);

        let cst = TypeRepr::Const(Box::new(named.clone()));
        assert_eq!(cst.unwrap_const(), &named);
        assert_eq!(named.unwrap_const(), &named);
    }

    #[test]
    fn parse_type_string_primitive_int() {
        assert_eq!(
            TypeRepr::parse_type_string("int"),
            TypeRepr::Primitive(PrimitiveType::Int)
        );
    }

    #[test]
    fn parse_type_string_void() {
        assert_eq!(TypeRepr::parse_type_string("void"), TypeRepr::Void);
    }

    #[test]
    fn parse_type_string_handle_primitive() {
        assert_eq!(
            TypeRepr::parse_type_string("string@"),
            TypeRepr::Handle(Box::new(TypeRepr::Primitive(PrimitiveType::String)))
        );
    }

    #[test]
    fn parse_type_string_const_handle_named() {
        // `const Foo@` → handle to a const object (pointee is const,
        // handle is mutable). See AC20.
        assert_eq!(
            TypeRepr::parse_type_string("const Foo@"),
            TypeRepr::Handle(Box::new(TypeRepr::Const(Box::new(TypeRepr::Named(
                "Foo".into()
            )))))
        );
    }

    /// B144. This test used to be named for the trailing-`const` case and
    /// then assert `Foo@` instead — "we just verify the default handle path
    /// stays lossless". It was the test that should have caught the defect,
    /// and it sidestepped it because `display()` renders `Const(Handle(T))`
    /// and `Handle(Const(T))` to strings that were assumed equal. That is
    /// B136's caution exactly: a test that compares types by rendered string
    /// cannot see a structural type bug, and one that declines to assert the
    /// structure at all cannot see anything.
    ///
    /// So this asserts the TREE. `Foo@ const` is a const HANDLE to a mutable
    /// object -> `Const(Handle(Foo))`, the shape `receiver_is_const` and
    /// `base_type_name` were both already written against.
    #[test]
    fn parse_type_string_handle_trailing_const() {
        assert_eq!(
            TypeRepr::parse_type_string("Foo@"),
            TypeRepr::Handle(Box::new(TypeRepr::Named("Foo".into())))
        );
        assert_eq!(
            TypeRepr::parse_type_string("Foo@ const"),
            TypeRepr::Const(Box::new(TypeRepr::Handle(Box::new(TypeRepr::Named(
                "Foo".into()
            )))))
        );
    }

    /// B144. The two `const`s mean different things and the type string
    /// parser must land them in different places:
    ///
    /// * leading `const Foo@` is a handle to a const OBJECT, so the const
    ///   goes INSIDE the handle: `Handle(Const(T))`.
    /// * trailing `Foo@ const` is a const HANDLE to a mutable object, so it
    ///   goes OUTSIDE: `Const(Handle(T))`.
    /// * both, `const Foo@ const`, is a const handle to a const object:
    ///   `Const(Handle(Const(T)))`.
    ///
    /// Before the fix the trailing one was not recognised at all: the whole
    /// string fell through to the `Named` arm as `Named("Foo@ const")`, a
    /// type whose NAME contains its own qualifiers and which is therefore
    /// equal to nothing. Every funcdef signature comparison against it
    /// failed, which is the false positive Max reported against a shipped
    /// plugin (`array::Sort(SortGhosts)`).
    #[test]
    fn parse_type_string_distinguishes_the_two_const_positions() {
        let foo = || TypeRepr::Named("Foo".into());
        let leading = TypeRepr::Handle(Box::new(TypeRepr::Const(Box::new(foo()))));
        let trailing = TypeRepr::Const(Box::new(TypeRepr::Handle(Box::new(foo()))));
        let both = TypeRepr::Const(Box::new(TypeRepr::Handle(Box::new(TypeRepr::Const(
            Box::new(foo()),
        )))));

        assert_eq!(TypeRepr::parse_type_string("const Foo@"), leading);
        assert_eq!(TypeRepr::parse_type_string("Foo@ const"), trailing);
        assert_eq!(TypeRepr::parse_type_string("const Foo@ const"), both);

        // Non-vacuity: the three really are three distinct trees, so the
        // three assertions above cannot all be satisfied by one shape.
        assert_ne!(leading, trailing);
        assert_ne!(leading, both);
        assert_ne!(trailing, both);
    }

    /// B144. The reference modifier is stripped from all four spellings, and
    /// the engine's OWN rendering — which writes no spaces around the
    /// trailing `const` (`const Json::Value@const&in`, measured 2026-09-01,
    /// OP 1.29.14) — parses to the same tree as the spaced source form.
    #[test]
    fn parse_type_string_const_positions_survive_reference_modifiers() {
        let want = TypeRepr::Const(Box::new(TypeRepr::Handle(Box::new(TypeRepr::Const(
            Box::new(TypeRepr::Named("Json::Value".into())),
        )))));
        for spelling in [
            "const Json::Value@ const &in",
            "const Json::Value@ const &out",
            "const Json::Value@ const&in",
            "const Json::Value@const&in",
            "const Json::Value@const",
        ] {
            assert_eq!(
                TypeRepr::parse_type_string(spelling),
                want,
                "spelling {spelling:?}"
            );
        }
    }

    /// B144 negative control. A trailing `const` is legal ONLY after a `@`
    /// — the engine rejects `int const &in` and `vec3 const &in` outright
    /// ("Expected ')' or ','; Instead found reserved keyword 'const'",
    /// measured 2026-09-01, OP 1.29.14). So the new suffix rule must not
    /// fire on a bare value type, and must not eat a type whose NAME merely
    /// ends in those letters.
    #[test]
    fn parse_type_string_trailing_const_needs_a_handle() {
        assert_eq!(
            TypeRepr::parse_type_string("Myconst"),
            TypeRepr::Named("Myconst".into())
        );
        assert_eq!(
            TypeRepr::parse_type_string("Ns::const"),
            TypeRepr::Named("Ns::const".into())
        );
    }

    /// B144. An array OF const handles keeps the element structure.
    #[test]
    fn parse_type_string_array_of_const_handles() {
        assert_eq!(
            TypeRepr::parse_type_string("Foo@ const[]"),
            TypeRepr::Array(Box::new(TypeRepr::Const(Box::new(TypeRepr::Handle(
                Box::new(TypeRepr::Named("Foo".into()))
            )))))
        );
    }

    #[test]
    fn parse_type_string_generic_array_int() {
        // `array<T>` canonicalizes to the dedicated `Array(T)` shape so
        // downstream consumers only see one form of the built-in generic.
        assert_eq!(
            TypeRepr::parse_type_string("array<int>"),
            TypeRepr::Array(Box::new(TypeRepr::Primitive(PrimitiveType::Int)))
        );
    }

    #[test]
    fn parse_type_string_nested_array_of_array() {
        assert_eq!(
            TypeRepr::parse_type_string("array<array<int>>"),
            TypeRepr::Array(Box::new(TypeRepr::Array(Box::new(TypeRepr::Primitive(
                PrimitiveType::Int
            )))))
        );
    }

    #[test]
    fn parse_type_string_array_handle() {
        assert_eq!(
            TypeRepr::parse_type_string("array<Foo@>"),
            TypeRepr::Array(Box::new(TypeRepr::Handle(Box::new(TypeRepr::Named(
                "Foo".into()
            )))))
        );
    }

    /// B156: every base in the list indexes to its element type and reads as
    /// array-like. Driven off `ARRAY_GENERIC_BASES` itself, so a base added to
    /// the list can never escape this behavioural check the way `MwRefBuffer`
    /// escaped being in the list at all.
    #[test]
    fn every_listed_array_generic_indexes_to_its_element_type() {
        for base in ARRAY_GENERIC_BASES {
            let t = TypeRepr::parse_type_string(&format!("{base}<int>"));
            assert!(
                t.is_array_like(),
                "`{base}<int>` must read as array-like, got {t:?}"
            );
            assert_eq!(
                t.array_element_type(),
                Some(&TypeRepr::Primitive(PrimitiveType::Int)),
                "`{base}<int>` must index to int"
            );
            // And through a handle, which is how engine members arrive.
            let h = TypeRepr::parse_type_string(&format!("const {base}<int>@"));
            assert!(
                h.is_array_like(),
                "`const {base}<int>@` must read as array-like"
            );
        }
        assert!(
            ARRAY_GENERIC_BASES.len() >= 9,
            "the loop must have had something to check, saw {}",
            ARRAY_GENERIC_BASES.len()
        );
    }

    /// B156, the specific correction: `MwRefBuffer` is an element-typed
    /// container in the dump and was missing from the list, so indexing it
    /// yielded nothing.
    #[test]
    fn mw_ref_buffer_is_an_array_generic() {
        let t = TypeRepr::parse_type_string("MwRefBuffer<CGameCtnBlock@>");
        assert!(t.is_array_like(), "MwRefBuffer must read as array-like");
        assert_eq!(
            t.array_element_type(),
            Some(&TypeRepr::Handle(Box::new(TypeRepr::Named(
                "CGameCtnBlock".into()
            )))),
            "MwRefBuffer<T> must index to T"
        );
    }

    /// B156, the other direction: `MwFastBufferCat` registers NO methods in
    /// the dump — no `opIndex`, no `Length` — so it is not a container and
    /// must not be treated as one. Asserted as a real absence, alongside
    /// shapes that were never array-like, so the row cannot pass merely
    /// because nothing is array-like.
    #[test]
    fn non_container_generics_are_not_array_like() {
        for spelling in [
            "MwFastBufferCat<int>",
            "dictionary<int>",
            "NotAContainer<int>",
            "vec3",
        ] {
            let t = TypeRepr::parse_type_string(spelling);
            assert!(
                !t.is_array_like(),
                "`{spelling}` must not read as array-like"
            );
            assert_eq!(
                t.array_element_type(),
                None,
                "`{spelling}` must not yield an element type"
            );
        }
        // Control: the assertion above is capable of failing — a real
        // container in the same loop shape does read as array-like.
        assert!(TypeRepr::parse_type_string("MwFastBuffer<int>").is_array_like());
    }

    #[test]
    fn array_element_type_accessor() {
        let arr = TypeRepr::Array(Box::new(TypeRepr::Primitive(PrimitiveType::Int)));
        assert_eq!(
            arr.array_element_type(),
            Some(&TypeRepr::Primitive(PrimitiveType::Int))
        );
        let wrapped = TypeRepr::Const(Box::new(arr.clone()));
        assert_eq!(
            wrapped.array_element_type(),
            Some(&TypeRepr::Primitive(PrimitiveType::Int))
        );
        let generic = TypeRepr::Generic {
            base: "array".into(),
            args: vec![TypeRepr::Primitive(PrimitiveType::String)],
        };
        assert_eq!(
            generic.array_element_type(),
            Some(&TypeRepr::Primitive(PrimitiveType::String))
        );
        assert!(TypeRepr::Primitive(PrimitiveType::Int)
            .array_element_type()
            .is_none());
    }

    #[test]
    fn parse_type_string_generic_dictionary_two_args() {
        assert_eq!(
            TypeRepr::parse_type_string("dictionary<string,int>"),
            TypeRepr::Generic {
                base: "dictionary".into(),
                args: vec![
                    TypeRepr::Primitive(PrimitiveType::String),
                    TypeRepr::Primitive(PrimitiveType::Int),
                ],
            }
        );
    }

    #[test]
    fn parse_type_string_array_shorthand() {
        assert_eq!(
            TypeRepr::parse_type_string("Foo[]"),
            TypeRepr::Array(Box::new(TypeRepr::Named("Foo".into())))
        );
    }

    /// GH #50: Nadeo engine container generics behave as arrays —
    /// `MwFastArray<CControlBase@>` must be array-like and index to
    /// `CControlBase@`.
    #[test]
    fn engine_array_generics_are_array_like() {
        let t = TypeRepr::parse_type_string("MwFastArray<CControlBase@>");
        assert!(t.is_array_like());
        assert_eq!(
            t.array_element_type(),
            Some(&TypeRepr::Handle(Box::new(TypeRepr::Named(
                "CControlBase".into()
            ))))
        );
        // Non-array generics stay opaque.
        let d = TypeRepr::parse_type_string("dictionary<string,int>");
        assert!(!d.is_array_like());
    }

    #[test]
    fn parse_type_string_named_fallback() {
        assert_eq!(
            TypeRepr::parse_type_string("NotAType"),
            TypeRepr::Named("NotAType".into())
        );
    }

    #[test]
    fn parse_type_string_qualified_named() {
        assert_eq!(
            TypeRepr::parse_type_string("Net::HttpRequest"),
            TypeRepr::Named("Net::HttpRequest".into())
        );
    }

    #[test]
    fn parse_type_string_strips_reference_modifier() {
        assert_eq!(
            TypeRepr::parse_type_string("int &in"),
            TypeRepr::Primitive(PrimitiveType::Int)
        );
        assert_eq!(
            TypeRepr::parse_type_string("Foo &out"),
            TypeRepr::Named("Foo".into())
        );
    }

    #[test]
    fn parse_type_string_empty_is_error() {
        assert_eq!(
            TypeRepr::parse_type_string(""),
            TypeRepr::Error(String::new())
        );
    }

    #[test]
    fn is_nullish_and_is_error() {
        assert!(TypeRepr::Null.is_nullish());
        assert!(TypeRepr::Handle(Box::new(TypeRepr::Error("X".into()))).is_nullish());
        assert!(!TypeRepr::Named("Foo".into()).is_nullish());

        assert!(TypeRepr::Error("X".into()).is_error());
        assert!(TypeRepr::Handle(Box::new(TypeRepr::Error("X".into()))).is_error());
        assert!(TypeRepr::Generic {
            base: "array".into(),
            args: vec![TypeRepr::Error("X".into())],
        }
        .is_error());
        assert!(!TypeRepr::Primitive(PrimitiveType::Int).is_error());
    }
}

// ── B156: is the array-generic list what the shipped dump actually says? ────
//
// `ARRAY_GENERIC_BASES` was a hand-written literal, and it had drifted in BOTH
// directions at once: it omitted `MwRefBuffer` (which the dump registers with
// `T& opIndex(uint)` and both `Length` spellings) and included
// `MwFastBufferCat` (which the dump registers with NO methods at all — only a
// constructor and a destructor, so it can be neither indexed nor measured).
//
// The list is consumed by a PURE function with no access to a loaded
// `TypeIndex`, so it cannot be derived at run time without plumbing the index
// through `TypeRepr`. It stays a literal — but a CHECKED one: this audit
// derives the set from the dump on every run and fails if the two disagree,
// which is the difference between a hardcoded value and a hardcoded value
// nobody is watching. Five hardcoded-vs-dump disagreements have been found in
// the typedb layer in one night (B125, B131, B137, B142, B146); this is the
// sixth, and the audit is what stops it being the last one found by accident.
//
// The derivation rule is the list's OWN documented contract — "indexing yields
// the element type" — read off the dump as "a class with an `opIndex` whose
// return type is the template parameter `T`". Deliberately not "has a
// `Length`": `vec2`/`vec3`/`vec4`/`quat` have `Length` and are not containers,
// and `MwFastBufferCat` has neither, so `Length` would derive the wrong set.
#[cfg(test)]
mod b156_array_generic_list_audit {
    use super::ARRAY_GENERIC_BASES;
    use std::collections::BTreeSet;

    fn core_dump() -> serde_json::Value {
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typedb/OpenplanetCore.json");
        serde_json::from_str(&std::fs::read_to_string(p).expect("fixture must exist"))
            .expect("fixture must parse")
    }

    /// Classes the dump says are element-typed containers: they expose an
    /// `opIndex` returning the template parameter `T`. Read off the dump, not
    /// copied from anywhere.
    fn containers_per_the_dump(db: &serde_json::Value) -> BTreeSet<String> {
        db["classes"]
            .as_array()
            .expect("classes array")
            .iter()
            .filter(|c| {
                c["methods"]
                    .as_array()
                    .map(|ms| {
                        ms.iter().any(|m| {
                            m["name"].as_str() == Some("opIndex")
                                && m["returntypedecl"].as_str() == Some("T")
                        })
                    })
                    .unwrap_or(false)
            })
            .filter_map(|c| c["name"].as_str().map(str::to_string))
            .collect()
    }

    /// The audit's own premise, asserted rather than assumed: the dump really
    /// does distinguish these classes this way. Without this, an empty or
    /// broken derivation would make the comparison below pass by matching
    /// nothing against nothing.
    #[test]
    fn the_dump_really_does_mark_containers_by_their_opindex() {
        let db = core_dump();
        let derived = containers_per_the_dump(&db);
        assert!(
            derived.len() >= 9,
            "the derivation found only {} containers — it is broken, not the \
             list: {derived:?}",
            derived.len()
        );
        // Positive: the script array is one, and it is the case every
        // consumer of this list was originally written for.
        assert!(derived.contains("array"), "derived={derived:?}");
        // Negative: types that carry `Length` but are NOT containers must
        // stay out, which is what rules out "has a Length" as the rule.
        for not_a_container in ["vec2", "vec3", "vec4", "quat", "string", "wstring"] {
            assert!(
                !derived.contains(not_a_container),
                "`{not_a_container}` is not an element-typed container, but the \
                 derivation claimed it: {derived:?}"
            );
        }
    }

    /// The audit proper: the literal the checker consumes must be exactly the
    /// set the dump describes, in both directions.
    #[test]
    fn the_array_generic_list_matches_the_shipped_dump() {
        let db = core_dump();
        let derived = containers_per_the_dump(&db);
        let declared: BTreeSet<String> =
            ARRAY_GENERIC_BASES.iter().map(|s| s.to_string()).collect();

        let missing: Vec<_> = derived.difference(&declared).collect();
        let extra: Vec<_> = declared.difference(&derived).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "ARRAY_GENERIC_BASES disagrees with the shipped dump.\n  \
             missing (dump says container, list omits): {missing:?}\n  \
             extra (list claims container, dump does not): {extra:?}"
        );
    }
}
