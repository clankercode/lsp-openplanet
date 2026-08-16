//! Pragmatic AST-based pretty-printer for AngelScript.
//!
//! The formatter controls top-level structure (declaration shapes, class/
//! namespace/function body wrapping, and indentation) and falls back to the
//! original source text for sub-expressions and most statements via a
//! `span_text` shortcut. This keeps the implementation compact while still
//! producing a canonical, roundtrip-stable form on the parsed subset of the
//! language.
//!
//! Known limitations:
//! - Comments are dropped (the lexer strips them before parsing).
//! - Expressions inside statements use the original source text verbatim; we
//!   only rewrite whitespace at the statement boundary.
//! - Malformed files are passed through unchanged.

use crate::lexer::Span;
use crate::parser::ast::*;

/// Format `source` into a canonical representation. If the parser reports any
/// errors we return the input unchanged to avoid corrupting the user's file.
pub fn format_source(source: &str) -> String {
    let tokens = crate::lexer::tokenize_filtered(source);
    let mut parser = crate::parser::Parser::new(&tokens, source);
    let file = parser.parse_file();
    if !parser.errors.is_empty() {
        return source.to_string();
    }
    let mut out = String::new();
    {
        let mut fmt = Formatter {
            source,
            out: &mut out,
            indent: 0,
        };
        for (i, item) in file.items.iter().enumerate() {
            if i > 0 {
                fmt.out.push('\n');
            }
            fmt.item(item);
            fmt.out.push('\n');
        }
    }
    out
}

struct Formatter<'a> {
    source: &'a str,
    out: &'a mut String,
    indent: usize,
}

impl<'a> Formatter<'a> {
    fn write(&mut self, s: &str) {
        self.out.push_str(s);
    }

    fn write_indent(&mut self) {
        for _ in 0..self.indent {
            self.out.push_str("    ");
        }
    }

    fn newline(&mut self) {
        self.out.push('\n');
        self.write_indent();
    }

    fn span_text(&self, span: Span) -> &'a str {
        span.text(self.source)
    }

    /// Write a raw source slice, collapsing any internal runs of ASCII
    /// whitespace to a single space and trimming leading/trailing whitespace.
    /// This gives us a predictable canonical spelling for declaration headers
    /// (return type, name, parameter list) without walking the full expression
    /// tree.
    fn write_condensed(&mut self, text: &str) {
        let mut prev_ws = false;
        let mut started = false;
        for ch in text.chars() {
            if ch.is_ascii_whitespace() {
                if started {
                    prev_ws = true;
                }
                continue;
            }
            if prev_ws {
                self.out.push(' ');
                prev_ws = false;
            }
            self.out.push(ch);
            started = true;
        }
    }

    fn item(&mut self, item: &Item) {
        match item {
            Item::Function(f) => self.function_decl(f),
            Item::Class(c) => self.class_decl(c),
            Item::Interface(i) => self.interface_decl(i),
            Item::Enum(e) => self.enum_decl(e),
            Item::Namespace(n) => self.namespace_decl(n),
            Item::VarDecl(v) => {
                self.write_indent();
                self.write_condensed(self.span_text(v.span));
            }
            Item::Funcdef(fd) => {
                self.write_indent();
                self.write_condensed(self.span_text(fd.span));
            }
            Item::Property(p) => self.property_decl(p),
            Item::Import(imp) => {
                self.write_indent();
                self.write_condensed(self.span_text(imp.span));
            }
            Item::Error(span) => {
                self.write_indent();
                self.write_condensed(self.span_text(*span));
            }
        }
    }

    /// Emit a full function/method declaration. The header (attributes,
    /// return type, name, parameter list, post-modifiers) is reproduced by
    /// condensing the source slice from the start of the decl up to just
    /// before the opening brace of the body. The body itself is re-indented
    /// so nested functions inherit the current indent level.
    fn function_decl(&mut self, f: &FunctionDecl) {
        self.write_indent();
        let header_end = match &f.body {
            Some(body) => body.span.start as usize,
            None => f.span.end as usize,
        };
        let header = &self.source[f.span.start as usize..header_end];
        self.write_condensed(header.trim_end());
        match &f.body {
            Some(body) => {
                self.write(" ");
                self.function_body(body);
            }
            None => {
                self.write(";");
            }
        }
    }

    fn function_body(&mut self, body: &FunctionBody) {
        if body.stmts.is_empty() {
            self.write("{\n");
            self.write_indent();
            self.write("}");
            return;
        }
        self.write("{");
        self.indent += 1;
        for stmt in &body.stmts {
            self.newline();
            self.stmt(stmt);
        }
        self.indent -= 1;
        self.newline();
        self.write("}");
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Block(stmts) => {
                if stmts.is_empty() {
                    self.write("{\n");
                    self.write_indent();
                    self.write("}");
                    return;
                }
                self.write("{");
                self.indent += 1;
                for s in stmts {
                    self.newline();
                    self.stmt(s);
                }
                self.indent -= 1;
                self.newline();
                self.write("}");
            }
            _ => {
                // Pragmatic: condense the original source span for this
                // statement. Expressions, loops, ifs, etc. are reproduced
                // verbatim modulo whitespace collapsing.
                let text = self.span_text(stmt.span);
                self.write_condensed(text);
            }
        }
    }

    fn class_decl(&mut self, c: &ClassDecl) {
        self.write_indent();
        // Condense the header up to the first member's start, or the closing
        // brace if empty. We can't easily find the opening brace without
        // re-scanning, so condense up to the first member if present.
        let header_end = if let Some(first) = c.members.first() {
            match first {
                ClassMember::Field(v) => v.span.start as usize,
                ClassMember::Method(f) | ClassMember::Constructor(f) | ClassMember::Destructor(f) => {
                    f.span.start as usize
                }
                ClassMember::Property(p) => p.span.start as usize,
            }
        } else {
            // Empty class: condense whole thing, then override.
            let text = self.span_text(c.span);
            // Build canonical `class Name {}` form.
            let name = c.name.text(self.source);
            let mut prefix = String::new();
            if c.is_shared {
                prefix.push_str("shared ");
            }
            if c.is_abstract {
                prefix.push_str("abstract ");
            }
            if c.is_mixin {
                prefix.push_str("mixin ");
            }
            // Preserve bases text (between ':' and '{') if present.
            let bases_text = extract_bases_text(text);
            self.write(&format!("{}class {}", prefix, name));
            if let Some(b) = bases_text {
                self.write(" : ");
                self.write_condensed(&b);
            }
            self.write(" {\n");
            self.write_indent();
            self.write("}");
            return;
        };

        let header = &self.source[c.span.start as usize..header_end];
        // Strip trailing `{` and whitespace.
        let header = header.trim_end();
        let header = header.strip_suffix('{').unwrap_or(header).trim_end();
        self.write_condensed(header);
        self.write(" {");
        self.indent += 1;
        for member in &c.members {
            self.newline();
            self.class_member(member);
        }
        self.indent -= 1;
        self.newline();
        self.write("}");
    }

    fn class_member(&mut self, member: &ClassMember) {
        match member {
            ClassMember::Field(v) => {
                self.write_condensed(self.span_text(v.span));
            }
            ClassMember::Method(f)
            | ClassMember::Constructor(f)
            | ClassMember::Destructor(f) => {
                // Re-use function_decl but strip its leading indent since
                // we've already emitted the member indent via newline().
                let saved = self.indent;
                self.indent = 0;
                self.function_decl(f);
                self.indent = saved;
            }
            ClassMember::Property(p) => {
                let saved = self.indent;
                self.indent = 0;
                self.property_decl(p);
                self.indent = saved;
            }
        }
    }

    fn interface_decl(&mut self, i: &InterfaceDecl) {
        self.write_indent();
        let name = i.name.text(self.source);
        self.write(&format!("interface {}", name));
        if !i.bases.is_empty() {
            self.write(" : ");
            for (idx, b) in i.bases.iter().enumerate() {
                if idx > 0 {
                    self.write(", ");
                }
                self.write_condensed(self.span_text(b.span));
            }
        }
        if i.methods.is_empty() {
            self.write(" {\n");
            self.write_indent();
            self.write("}");
            return;
        }
        self.write(" {");
        self.indent += 1;
        for m in &i.methods {
            self.newline();
            let saved = self.indent;
            self.indent = 0;
            self.function_decl(m);
            self.indent = saved;
        }
        self.indent -= 1;
        self.newline();
        self.write("}");
    }

    fn enum_decl(&mut self, e: &EnumDecl) {
        self.write_indent();
        let name = e.name.text(self.source);
        self.write(&format!("enum {}", name));
        if e.values.is_empty() {
            self.write(" {\n");
            self.write_indent();
            self.write("}");
            return;
        }
        self.write(" {");
        self.indent += 1;
        for (idx, v) in e.values.iter().enumerate() {
            self.newline();
            self.write(v.name.text(self.source));
            if let Some(val) = &v.value {
                self.write(" = ");
                self.write_condensed(self.span_text(val.span));
            }
            if idx + 1 < e.values.len() {
                self.write(",");
            }
        }
        self.indent -= 1;
        self.newline();
        self.write("}");
    }

    fn namespace_decl(&mut self, n: &NamespaceDecl) {
        self.write_indent();
        let name = n.name.text(self.source);
        self.write(&format!("namespace {}", name));
        if n.items.is_empty() {
            self.write(" {\n");
            self.write_indent();
            self.write("}");
            return;
        }
        self.write(" {");
        self.indent += 1;
        for (idx, item) in n.items.iter().enumerate() {
            if idx > 0 {
                self.out.push('\n');
            }
            self.newline();
            // item() writes its own indent via write_indent(); newline()
            // already added indent, so back it out.
            // Simpler: rewind the indent we just wrote, then item() writes it
            // fresh. But that's fragile — instead strip the indent from newline.
            // Trick: newline wrote indent; we then call item() which calls
            // write_indent() again. Avoid doubling by truncating.
            let strip = self.indent * 4;
            for _ in 0..strip {
                self.out.pop();
            }
            self.item(item);
        }
        self.indent -= 1;
        self.newline();
        self.write("}");
    }

    fn property_decl(&mut self, p: &PropertyDecl) {
        self.write_indent();
        self.write_condensed(self.span_text(p.span));
    }
}

/// Extract the `: Base1, Base2` section from a class header string, if any.
/// Returns the text between `:` and the first `{`, trimmed.
fn extract_bases_text(class_text: &str) -> Option<String> {
    let colon = class_text.find(':')?;
    let rest = &class_text[colon + 1..];
    let brace = rest.find('{')?;
    let bases = rest[..brace].trim();
    if bases.is_empty() {
        None
    } else {
        Some(bases.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_simple_function() {
        let src = "void foo(){}";
        let out = format_source(src);
        assert_eq!(out, "void foo() {\n}\n");
    }

    #[test]
    fn format_class_with_field() {
        let src = "class C { int x; }";
        let out = format_source(src);
        // Expect indented field inside braces.
        assert!(out.starts_with("class C {"), "got: {:?}", out);
        assert!(out.contains("\n    int x;"), "got: {:?}", out);
        assert!(out.trim_end().ends_with('}'), "got: {:?}", out);
    }

    #[test]
    fn format_roundtrip_stable() {
        // Parse, format, format again — must be byte-equal the second time.
        let src = "class C { int x; void f() { int y = 1; } }\nvoid g() {}\n";
        let once = format_source(src);
        let twice = format_source(&once);
        assert_eq!(once, twice, "formatter is not idempotent:\n--once--\n{}\n--twice--\n{}", once, twice);
    }

    #[test]
    fn format_namespace_with_function() {
        let src = "namespace N { void f() {} }";
        let out = format_source(src);
        let twice = format_source(&out);
        assert_eq!(out, twice, "namespace format not idempotent: {:?}", out);
        assert!(out.contains("namespace N {"), "got: {:?}", out);
    }

    #[test]
    fn format_preserves_parse() {
        // The formatted output must still parse cleanly.
        let src = "int global = 1;\nvoid main() { int x = 2; x = x + 1; }\n";
        let out = format_source(src);
        let tokens = crate::lexer::tokenize_filtered(&out);
        let mut parser = crate::parser::Parser::new(&tokens, &out);
        let _file = parser.parse_file();
        assert!(parser.errors.is_empty(), "formatted output failed to parse: {:?}\n{}", parser.errors, out);
    }
}
