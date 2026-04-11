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
            TypeRepr::Const(inner) => format!("const {}", inner.display()),
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
        let inner = self.unwrap_const().unwrap_handle();
        match inner {
            TypeRepr::Array(elem) => Some(elem),
            TypeRepr::Generic { base, args } if base == "array" && args.len() == 1 => {
                Some(&args[0])
            }
            _ => None,
        }
    }

    /// True if `self` (after stripping `Const`/`Handle`) is an array-like
    /// generic, regardless of whether the element type is resolved.
    pub fn is_array_like(&self) -> bool {
        let inner = self.unwrap_const().unwrap_handle();
        matches!(inner, TypeRepr::Array(_))
            || matches!(inner, TypeRepr::Generic { base, .. } if base == "array")
    }

    /// True if `self` (after stripping `Const`/`Handle`) is a dictionary
    /// type (`Generic { base: "dictionary", .. }` or a bare
    /// `Named("dictionary")` that slipped through).
    pub fn is_dictionary_like(&self) -> bool {
        let inner = self.unwrap_const().unwrap_handle();
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
            TypeRepr::Handle(inner)
            | TypeRepr::Const(inner)
            | TypeRepr::Array(inner) => inner.is_error(),
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

fn parse_type_string_inner(s: &str) -> TypeRepr {
    let s = s.trim();

    // `const X` → Const(parse(X))
    if let Some(rest) = s.strip_prefix("const ") {
        return TypeRepr::Const(Box::new(parse_type_string_inner(rest.trim())));
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

    #[test]
    fn display_const_handle() {
        let t = TypeRepr::Const(Box::new(TypeRepr::Handle(Box::new(TypeRepr::Named(
            "Foo".into(),
        )))));
        assert_eq!(t.display(), "const Foo@");
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
        assert_eq!(
            TypeRepr::parse_type_string("const Foo@"),
            TypeRepr::Const(Box::new(TypeRepr::Handle(Box::new(TypeRepr::Named(
                "Foo".into()
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
            TypeRepr::Array(Box::new(TypeRepr::Array(Box::new(
                TypeRepr::Primitive(PrimitiveType::Int)
            ))))
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

    #[test]
    fn array_element_type_accessor() {
        let arr =
            TypeRepr::Array(Box::new(TypeRepr::Primitive(PrimitiveType::Int)));
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
