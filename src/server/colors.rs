//! Colour swatches and the colour picker (B041).
//!
//! Two forms, both Openplanet's rather than AngelScript's:
//!
//! * `vec3(r, g, b)` / `vec4(r, g, b, a)` with literal arguments, in the
//!   0..1 space the game uses.
//! * `$xxx` escape codes inside string literals — three hex digits, the
//!   ManiaPlanet text-formatting convention Openplanet inherits.
//!
//! Everything here is a pure function over `&str`. No `Backend`, no snapshot,
//! no type index: the whole feature is decidable from the token stream, and
//! keeping it that way means the tests are the real thing rather than a
//! server harness approximating it.
//!
//! # The `$` grammar, confirmed rather than assumed
//!
//! From Openplanet's own API docs — `Text::FormatGameColor(const vec3&in rgb)`
//! is documented as "Formats a color in Maniaplanet-style color formatting,
//! for example: `$f00`" — and the ManiaPlanet text-formatting reference:
//!
//! * A colour is `$` followed by **exactly three** hex digits, one per RGB
//!   channel, `0`-`f`. There is **no** six-digit form.
//! * `$$` is a literal dollar. `"$$f00"` therefore contains NO colour: the
//!   game renders a `$` and the text `f00`.
//! * Every other `$` code is a style or control code, none of them colours:
//!   `$z` reset, `$g` default colour, `$o` bold, `$i` italic, `$s` shadow,
//!   `$t` caps, `$w` wide, `$n` narrow, `$m` normal, `$<`/`$>` isolate, and
//!   `$l[url]` / `$h[link]` / `$p[params]` which take bracketed arguments.
//!
//! Conveniently the control letters (`g h i l m n o p s t w z < >`) are all
//! outside the hex alphabet, so "the character after `$` is a hex digit"
//! already means "this is a colour, not a code" — there is no ambiguity to
//! resolve. That is a property of the grammar worth stating, because it is
//! what makes a three-character lookahead sufficient.
//!
//! # Which `vec3`/`vec4` get a swatch — the scoping decision
//!
//! The ticket asked for the narrow rule where possible: swatch only a vec3 in
//! argument position of a parameter *known* to be a colour, and fall back to
//! reporting every literal vec3/vec4 if the type database cannot say. **The
//! type database cannot say, so this is the broad rule plus a range gate.**
//! Both halves are measured, not assumed:
//!
//! *Why not the narrow rule.* There is no colour type — a colour is a `vec3`
//! or `vec4` and nothing distinguishes it at the type level. The only signal
//! is the parameter NAME, and across the shipped Core database those names are
//! `color`, `col`, `color1`, `color2`, `rgb`, `bg_col`, `tint_col`,
//! `colBackground`, `fill`, `outline` — a set no simple predicate covers, and
//! one that says nothing at all about user-defined functions. Worse, the
//! single largest bucket of real colour literals is not in argument position:
//! of 578 literal vec3/vec4 constructor calls across this repo's plugin
//! fixtures, 223 are initialisers like `const vec4 C = vec4(1, 0, 0, 1);`,
//! which have no parameter to consult. A narrow rule would miss most colours
//! in real code.
//!
//! *Why the range gate.* Reporting every literal vec3/vec4 would be dominated
//! by false positives: of those 578, 259 have a component outside 0..1 and are
//! world coordinates — `vec3(696, 1799.8, 534)`, `vec3(225, 225, 225)` — which
//! would render as identical clamped-white swatches. Requiring every component
//! to lie within 0..1 removes all 259, and removes **zero** of the sites that
//! look like colours by any other measure. That is not a heuristic bolted on:
//! this feature is defined in the 0..1 space the game uses, and a vector with
//! a component of 696 is not a colour in that space.
//!
//! The consequence, and a deliberate departure from the ticket's test plan: an
//! out-of-range literal such as `vec3(2, -1, 0)` gets **no swatch**, rather
//! than a clamped one. Clamping for display would assert that a world
//! coordinate is a colour.
//!
//! Residual false positives are in-range non-colours — `vec3(0, 1, 0)` as an
//! UP direction is the honest example. Those stay, and are the accepted cost
//! of covering initialisers.

use tower_lsp::lsp_types::{Color, ColorInformation, ColorPresentation, Position, Range, TextEdit};

use crate::lexer::{token::TokenKind, tokenize};

/// How a colour was written, so the picker can write it back the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Form {
    /// `$xxx` in a string or comment. `upper` preserves the case of EACH
    /// digit independently: one bool for all three turned `$aB3` into `$AB3`,
    /// which is the picker rewriting source the user never touched.
    Dollar { upper: [bool; 3] },
    /// `vec3(..)` / `vec4(..)`, with each argument's literal text kept so the
    /// picker can preserve `1` vs `1.0` vs `1.0f` vs `.5`.
    Vec { arity: usize, args: Vec<String> },
}

/// One colour found in the source: where it is, what it is, how it was
/// written.
#[derive(Debug, Clone)]
struct ColorSite {
    /// Byte range in the source, for matching a presentation request.
    start: usize,
    end: usize,
    color: Color,
    form: Form,
}

/// Every colour in `source`, in source order.
pub fn document_colors(source: &str) -> Vec<ColorInformation> {
    let lines = LineIndex::new(source);
    color_sites(source)
        .into_iter()
        .map(|site| ColorInformation {
            range: lines.range(source, site.start, site.end),
            color: site.color,
        })
        .collect()
}

/// How the picker should write `color` back at `range`.
///
/// The form is re-derived from the source at that range rather than carried
/// in the request, so a `$f00` can only ever come back as a `$`-form and a
/// `vec3` can only ever come back as a `vec3`. Round-tripping the user's
/// representation is the entire usability point of the feature: a picker that
/// turns their `$f00` into a `vec4` has damaged their string.
pub fn color_presentations(source: &str, range: Range, color: Color) -> Vec<ColorPresentation> {
    let lines = LineIndex::new(source);
    let Some(site) = color_sites(source)
        .into_iter()
        .find(|site| lines.range(source, site.start, site.end) == range)
    else {
        // A range we do not recognise: offer nothing rather than guess a
        // representation and overwrite something that was never a colour.
        return Vec::new();
    };
    let label = match &site.form {
        Form::Dollar { upper } => format_dollar(color, upper),
        Form::Vec { arity, args } => format_vec(color, *arity, args),
    };
    vec![ColorPresentation {
        label: label.clone(),
        text_edit: Some(TextEdit {
            range,
            new_text: label,
        }),
        additional_text_edits: None,
    }]
}

/// Byte offsets of the start of every line, built once per request.
///
/// [`offset_to_position`] rescans from the start of the file on every call,
/// which is fine for a handful of diagnostics and quadratic here: a string or
/// comment can hold hundreds of `$xxx` codes, and each site costs two calls.
/// This keeps the whole-file scan but does it exactly once, then binary
/// searches. The UTF-16 count still runs per site, but only over its own line.
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(source: &str) -> Self {
        let mut starts = vec![0usize];
        starts.extend(source.match_indices('\n').map(|(offset, _)| offset + 1));
        Self { starts }
    }

    fn position(&self, source: &str, offset: usize) -> Position {
        let mut offset = offset.min(source.len());
        while !source.is_char_boundary(offset) {
            offset -= 1;
        }
        // The last line whose start is <= offset.
        let line = self.starts.partition_point(|&start| start <= offset) - 1;
        let col = source[self.starts[line]..offset].encode_utf16().count();
        Position::new(line as u32, col as u32)
    }

    fn range(&self, source: &str, start: usize, end: usize) -> Range {
        Range::new(self.position(source, start), self.position(source, end))
    }
}

// ===== extraction ========================================================

fn color_sites(source: &str) -> Vec<ColorSite> {
    let tokens = tokenize(source);
    let mut sites = Vec::new();

    // `$xxx` lives in string literals and in comments. Comments are included
    // deliberately: commented-out UI code and "`$f00` is red" notes are
    // exactly where a reader wants the swatch, and the ticket asked for that
    // call to be made explicitly. The cost is that `$` followed by three hex
    // characters in PROSE is also reported — `// costs $100` gets a swatch —
    // which is pinned by a test so it stays a known trade rather than a
    // surprise.
    for token in &tokens {
        if matches!(
            token.kind,
            TokenKind::StringLit | TokenKind::LineComment | TokenKind::BlockComment
        ) {
            let start = token.span.start as usize;
            let end = (token.span.end as usize).min(source.len());
            if start < end {
                scan_dollar_codes(&source[start..end], start, &mut sites);
            }
        }
    }

    // `vec3(..)` / `vec4(..)` over the code tokens only.
    let code: Vec<_> = tokens.iter().filter(|t| !t.kind.is_trivia()).collect();
    for i in 0..code.len() {
        if let Some(site) = vec_call_at(source, &code, i) {
            sites.push(site);
        }
    }

    sites.sort_by_key(|site| (site.start, site.end));
    sites
}

/// Scan one string-literal or comment slice for `$xxx`.
///
/// `base` is the slice's byte offset in the whole source, so the ranges this
/// produces are absolute. The slice is the RAW source text (quotes included,
/// escapes un-processed), which is what makes the offsets usable: the byte
/// positions of a `$f00` in the file are what the editor needs, not its
/// position in the unescaped value.
fn scan_dollar_codes(slice: &str, base: usize, out: &mut Vec<ColorSite>) {
    let bytes = slice.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            // Advance a whole UTF-8 character. Stepping a byte at a time
            // would still work (a continuation byte is never `$`), but this
            // keeps the slicing below on character boundaries.
            i += utf8_len(bytes[i]);
            continue;
        }
        // `$$` is a literal dollar: consume both so the SECOND `$` cannot be
        // read as the start of a code. This is what stops `"$$f00"` from
        // reporting a colour that the game will never render.
        if bytes.get(i + 1) == Some(&b'$') {
            i += 2;
            continue;
        }
        // `$l[url]`, `$h[link]`, `$p[params]`: the bracketed argument is a
        // URL or a parameter list, NOT formatted text, so the game renders no
        // colour inside it. Skipping the payload is not an optimisation --
        // reporting a `$f00` in a URL would offer the user a picker whose
        // edit silently rewrites their link.
        //
        // Only a `[` starts a payload. A bare `$l` is the CLOSING form
        // (`"$l[url]text$l"`) and must fall through to the normal scan.
        if matches!(
            bytes.get(i + 1),
            Some(b'l' | b'L' | b'h' | b'H' | b'p' | b'P')
        ) && bytes.get(i + 2) == Some(&b'[')
        {
            i = match bytes[i + 3..].iter().position(|&b| b == b']') {
                Some(offset) => i + 3 + offset + 1,
                // Unterminated. The slice is one string or comment token, so
                // this consumes to the end of that token and no further.
                None => bytes.len(),
            };
            continue;
        }
        match parse_dollar_color(&bytes[i + 1..]) {
            Some((color, upper)) => {
                out.push(ColorSite {
                    start: base + i,
                    end: base + i + 4,
                    color,
                    form: Form::Dollar { upper },
                });
                i += 4;
            }
            // A style/control code (`$z`, `$o`, ...) or a `$` that begins
            // nothing. Either way it is not a colour.
            None => i += 1,
        }
    }
}

/// Exactly three hex digits, or nothing. Returns the colour and the case of
/// each digit, so the picker can write back the same spelling.
fn parse_dollar_color(rest: &[u8]) -> Option<(Color, [bool; 3])> {
    if rest.len() < 3 {
        return None;
    }
    let digits = &rest[..3];
    let mut values = [0u8; 3];
    // A digit `0`-`9` has no case; keep it lower so a later edit that turns it
    // into a letter matches the surrounding style rather than shouting.
    let mut upper = [false; 3];
    for (idx, (slot, byte)) in values.iter_mut().zip(digits).enumerate() {
        *slot = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => {
                upper[idx] = true;
                byte - b'A' + 10
            }
            _ => return None,
        };
    }
    Some((
        Color {
            red: values[0] as f32 / 15.0,
            green: values[1] as f32 / 15.0,
            blue: values[2] as f32 / 15.0,
            alpha: 1.0,
        },
        upper,
    ))
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Recognise `vec3(<lit>, <lit>, <lit>)` / `vec4(..)` starting at `code[i]`.
///
/// Every argument must be a plain numeric literal, optionally signed. That
/// single requirement is what implements the ticket's rule that a swatch must
/// be editable: `vec3(x, 0, 0)` and `vec3(f(1), 0, 0)` fail it and are not
/// reported, because a picker cannot write a value back into an expression.
fn vec_call_at(source: &str, code: &[&crate::lexer::token::Token], i: usize) -> Option<ColorSite> {
    let name_token = code[i];
    if name_token.kind != TokenKind::Ident {
        return None;
    }
    let name = &source[name_token.span.start as usize..name_token.span.end as usize];
    let arity = match name {
        "vec3" => 3,
        "vec4" => 4,
        _ => return None,
    };
    // `a.vec3(..)` / `Ns::vec3(..)` is somebody else's function, not the
    // built-in constructor.
    if i > 0 && matches!(code[i - 1].kind, TokenKind::Dot | TokenKind::ColonColon) {
        return None;
    }
    if code.get(i + 1)?.kind != TokenKind::LParen {
        return None;
    }

    let mut values = Vec::with_capacity(arity);
    let mut texts = Vec::with_capacity(arity);
    let mut cursor = i + 2;
    for arg in 0..arity {
        if arg > 0 {
            if code.get(cursor)?.kind != TokenKind::Comma {
                return None;
            }
            cursor += 1;
        }
        let (value, text, next) = numeric_literal(source, code, cursor)?;
        values.push(value);
        texts.push(text);
        cursor = next;
    }
    let close = code.get(cursor)?;
    if close.kind != TokenKind::RParen {
        return None;
    }

    // The range gate. See the module header: out-of-range means "not a colour
    // in the space this feature is defined in", not "a colour to clamp".
    if values.iter().any(|v| !(0.0..=1.0).contains(v)) {
        return None;
    }

    Some(ColorSite {
        start: name_token.span.start as usize,
        end: close.span.end as usize,
        color: Color {
            red: values[0],
            green: values[1],
            blue: values[2],
            alpha: if arity == 4 { values[3] } else { 1.0 },
        },
        form: Form::Vec { arity, args: texts },
    })
}

/// One optionally-signed numeric literal. Returns its value, its source text
/// (sign included, so the style survives the round trip) and the index after
/// it.
fn numeric_literal(
    source: &str,
    code: &[&crate::lexer::token::Token],
    at: usize,
) -> Option<(f32, String, usize)> {
    let mut idx = at;
    let mut negative = false;
    let start = code.get(idx)?.span.start as usize;
    match code[idx].kind {
        TokenKind::Minus => {
            negative = true;
            idx += 1;
        }
        TokenKind::Plus => idx += 1,
        _ => {}
    }
    let lit = code.get(idx)?;
    if !matches!(
        lit.kind,
        TokenKind::IntLit | TokenKind::FloatLit | TokenKind::HexLit
    ) {
        return None;
    }
    let text = &source[lit.span.start as usize..lit.span.end as usize];
    let value = parse_numeric(text)?;
    Some((
        if negative { -value } else { value },
        source[start..lit.span.end as usize].to_string(),
        idx + 1,
    ))
}

/// The value of one AngelScript numeric literal.
///
/// Not just `str::parse`: `0x1` lexes as `HexLit` and `0b1`/`0o1` lex as
/// `IntLit`, and all three fail a decimal parse, so every one of them used to
/// lose its swatch even when the value sat squarely inside 0..1. The radix
/// prefix has to be handled BEFORE the float suffix is trimmed, or `0xf`
/// would lose its last digit to the `f` rule.
fn parse_numeric(text: &str) -> Option<f32> {
    let (radix, digits) = match text.as_bytes() {
        [b'0', b'x' | b'X', rest @ ..] => (16, rest),
        [b'0', b'b' | b'B', rest @ ..] => (2, rest),
        [b'0', b'o' | b'O', rest @ ..] => (8, rest),
        _ => {
            // `1.0f` / `1.f`: the AngelScript float suffix is not part of the
            // number.
            return text.trim_end_matches(['f', 'F']).parse().ok();
        }
    };
    let digits: String = std::str::from_utf8(digits)
        .ok()?
        .chars()
        .filter(|c| *c != '_')
        .collect();
    if digits.is_empty() {
        return None;
    }
    Some(u64::from_str_radix(&digits, radix).ok()? as f32)
}

// ===== presentation ======================================================

fn format_dollar(color: Color, upper: &[bool; 3]) -> String {
    let digit = |channel: f32, upper: bool| -> char {
        let v = (channel.clamp(0.0, 1.0) * 15.0).round() as u8;
        let c = std::char::from_digit(v as u32, 16).unwrap_or('0');
        if upper {
            c.to_ascii_uppercase()
        } else {
            c
        }
    };
    format!(
        "${}{}{}",
        digit(color.red, upper[0]),
        digit(color.green, upper[1]),
        digit(color.blue, upper[2])
    )
}

fn format_vec(color: Color, arity: usize, args: &[String]) -> String {
    let channels = [color.red, color.green, color.blue, color.alpha];
    let rendered: Vec<String> = (0..arity)
        .map(|i| {
            let style = args.get(i).map(String::as_str).unwrap_or("0.0");
            format_channel(channels[i], style)
        })
        .collect();
    format!("vec{}({})", arity, rendered.join(", "))
}

/// Render one channel in the style the file already uses.
///
/// A picker that writes `0.5019608` for 128/255 is worse than useless in
/// source, so values are rounded to three decimals — three significant
/// figures across 0..1, and enough to survive a round trip through 8-bit
/// (0.502 * 255 = 128.01). The style of the literal being replaced is
/// preserved: an `f` suffix stays, a bare `1` stays bare when the new value is
/// still whole, and a leading-dot `.5` keeps its leading dot.
fn format_channel(value: f32, style: &str) -> String {
    let v = value.clamp(0.0, 1.0);
    let suffix = if style.ends_with(['f', 'F']) { "f" } else { "" };
    let rounded = (v * 1000.0).round() / 1000.0;

    // An integer-looking literal stays integer-looking only while the value
    // is whole; 0.5 cannot be written as an int without lying about it.
    let bare_int = !style.contains('.') && !style.ends_with(['f', 'F']);
    if bare_int && (rounded - rounded.round()).abs() < f32::EPSILON {
        return format!("{}{}", rounded.round() as i64, suffix);
    }

    let mut text = format!("{rounded:.3}");
    // Trim to the shortest exact form: 1.000 -> 1.0, 0.500 -> 0.5.
    while text.ends_with('0') && !text.ends_with(".0") {
        text.pop();
    }
    // `.5` style: drop the leading zero when the file did, and only when
    // there is a fractional part to lead with.
    if style.starts_with('.') && text.starts_with("0.") && text != "0.0" {
        text.remove(0);
    }
    format!("{text}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color = Color {
        red: 1.0,
        green: 0.0,
        blue: 0.0,
        alpha: 1.0,
    };

    fn colors(source: &str) -> Vec<ColorInformation> {
        document_colors(source)
    }

    /// The text a range actually covers, so a range assertion is readable and
    /// an off-by-one shows up as the wrong substring rather than a number.
    fn covered(source: &str, range: Range) -> String {
        let lines: Vec<&str> = source.split('\n').collect();
        let cut = |pos: tower_lsp::lsp_types::Position| -> usize {
            let line = lines[pos.line as usize];
            let mut utf16 = 0u32;
            for (byte, ch) in line.char_indices() {
                if utf16 == pos.character {
                    return byte;
                }
                utf16 += ch.len_utf16() as u32;
            }
            line.len()
        };
        let a = &lines[range.start.line as usize];
        assert_eq!(range.start.line, range.end.line, "single-line ranges only");
        a[cut(range.start)..cut(range.end)].to_string()
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.001
    }

    fn assert_color(found: &Color, want: &Color) {
        assert!(
            approx(found.red, want.red)
                && approx(found.green, want.green)
                && approx(found.blue, want.blue)
                && approx(found.alpha, want.alpha),
            "colour mismatch: {found:?} vs {want:?}"
        );
    }

    // === the three the ticket names ==================================

    #[test]
    fn document_color_finds_vec3_literal() {
        let src = "void f() { nvg::FillColor(vec3(1, 0, 0)); }";
        let found = colors(src);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_color(&found[0].color, &RED);
        assert_eq!(covered(src, found[0].range), "vec3(1, 0, 0)");
    }

    #[test]
    fn document_color_finds_dollar_escape_in_string() {
        let src = "void f() { print(\"$f00hi\"); }";
        let found = colors(src);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_color(&found[0].color, &RED);
        // Exactly the code, not the string.
        assert_eq!(covered(src, found[0].range), "$f00");
    }

    #[test]
    fn color_presentation_round_trips_form() {
        let dollar = "void f() { print(\"$f00\"); }";
        let at = colors(dollar)[0].range;
        let out = color_presentations(dollar, at, RED);
        assert_eq!(out.len(), 1);
        assert!(
            out[0].label.starts_with('$'),
            "a $-form must stay a $-form: {:?}",
            out[0].label
        );

        let vec3 = "void f() { g(vec3(1, 0, 0)); }";
        let at = colors(vec3)[0].range;
        let out = color_presentations(vec3, at, RED);
        assert_eq!(out.len(), 1);
        assert!(
            out[0].label.starts_with("vec3("),
            "a vec3 must not become a vec4: {:?}",
            out[0].label
        );
    }

    // === vec3 / vec4 variants ========================================

    #[test]
    fn vec4_is_found_and_its_alpha_survives_the_round_trip() {
        let src = "void f() { g(vec4(1, 0, 0, 0.5)); }";
        let found = colors(src);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(approx(found[0].color.alpha, 0.5));

        let out = color_presentations(src, found[0].range, found[0].color);
        assert_eq!(out[0].label, "vec4(1, 0, 0, 0.5)");
    }

    #[test]
    fn a_vec3_reports_opaque_alpha_and_never_grows_an_alpha_channel() {
        let src = "void f() { g(vec3(1, 0, 0)); }";
        let found = colors(src);
        assert!(approx(found[0].color.alpha, 1.0));
        // Even asked for a translucent colour, a vec3 stays three-component:
        // the alpha has nowhere to go and inventing a vec4 rewrites the call.
        let translucent = Color { alpha: 0.25, ..RED };
        let out = color_presentations(src, found[0].range, translucent);
        assert_eq!(out[0].label, "vec3(1, 0, 0)");
    }

    #[test]
    fn every_literal_style_parses() {
        for (src, what) in [
            ("g(vec3(1, 0, 0));", "bare ints"),
            ("g(vec3(1.0, 0.0, 0.0));", "trailing .0"),
            ("g(vec3(1.0f, 0.0f, 0.0f));", "f suffix"),
            ("g(vec3(1.f, 0.f, 0.f));", "1.f"),
            ("g(vec3(1., 0., 0.));", "trailing dot"),
            ("g(vec3(1, .0, .0));", "leading dot"),
        ] {
            let found = colors(&format!("void f() {{ {src} }}"));
            assert_eq!(found.len(), 1, "{what} not parsed: {src}");
            assert_color(&found[0].color, &RED);
        }
    }

    #[test]
    fn the_presentation_preserves_the_style_already_in_the_call() {
        let half = Color {
            red: 0.5,
            green: 0.5,
            blue: 0.5,
            alpha: 1.0,
        };
        for (src, want) in [
            ("g(vec3(1.0, 1.0, 1.0));", "vec3(0.5, 0.5, 0.5)"),
            ("g(vec3(1.0f, 1.0f, 1.0f));", "vec3(0.5f, 0.5f, 0.5f)"),
            ("g(vec3(.5, .5, .5));", "vec3(.5, .5, .5)"),
        ] {
            let full = format!("void f() {{ {src} }}");
            let found = colors(&full);
            let out = color_presentations(&full, found[0].range, half);
            assert_eq!(out[0].label, want, "style not preserved for {src}");
        }
    }

    #[test]
    fn a_whole_value_keeps_a_bare_int_but_a_fraction_does_not() {
        let src = "void f() { g(vec3(1, 0, 0)); }";
        let found = colors(src);
        // Still whole: stays bare.
        let out = color_presentations(src, found[0].range, RED);
        assert_eq!(out[0].label, "vec3(1, 0, 0)");
        // No longer whole: an int literal would be a lie, so it gains a point.
        let grey = Color {
            red: 0.5,
            green: 0.0,
            blue: 0.0,
            alpha: 1.0,
        };
        let out = color_presentations(src, found[0].range, grey);
        assert_eq!(out[0].label, "vec3(0.5, 0, 0)");
    }

    #[test]
    fn precision_is_readable_rather_than_exact() {
        // 128/255 = 0.50196... A picker writing that into source is worse
        // than useless; three decimals still round-trips through 8-bit.
        let src = "void f() { g(vec3(1.0, 1.0, 1.0)); }";
        let found = colors(src);
        let c = Color {
            red: 128.0 / 255.0,
            green: 1.0,
            blue: 0.0,
            alpha: 1.0,
        };
        let out = color_presentations(src, found[0].range, c);
        assert_eq!(out[0].label, "vec3(0.502, 1.0, 0.0)");
    }

    #[test]
    fn non_literal_arguments_get_no_swatch() {
        for src in [
            "g(vec3(x, 0, 0));",
            "g(vec3(f(1), 0, 0));",
            "g(vec3(a + b, 0, 0));",
            "g(vec3(1, 0));",
            "g(vec4(1, 0, 0));",
            "g(vec3(1, 0, 0, 1));",
        ] {
            let found = colors(&format!("void f() {{ {src} }}"));
            assert!(
                found.is_empty(),
                "{src} must not report a colour: {found:?}"
            );
        }
    }

    #[test]
    fn out_of_range_components_get_no_swatch() {
        // The documented departure from the ticket's plan. A vector with a
        // component outside 0..1 is not a colour in the space this feature is
        // defined in — it is a world coordinate, and clamping it to white
        // would assert something false about the code.
        for src in [
            "g(vec3(2, -1, 0));",
            "g(vec3(696, 1799.8, 534));",
            "g(vec3(225, 225, 225));",
            "g(vec4(1, 1, 1, 255));",
        ] {
            let found = colors(&format!("void f() {{ {src} }}"));
            assert!(found.is_empty(), "{src} should not be a colour: {found:?}");
        }
    }

    #[test]
    fn a_vec3_that_is_not_the_constructor_is_not_a_colour() {
        for src in ["g(a.vec3(1, 0, 0));", "g(Ns::vec3(1, 0, 0));"] {
            let found = colors(&format!("void f() {{ {src} }}"));
            assert!(found.is_empty(), "{src}: {found:?}");
        }
    }

    #[test]
    fn several_colours_on_one_line_get_correct_non_overlapping_ranges() {
        let src = "void f() { g(vec3(1, 0, 0), vec4(0, 1, 0, 1)); }";
        let found = colors(src);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(covered(src, found[0].range), "vec3(1, 0, 0)");
        assert_eq!(covered(src, found[1].range), "vec4(0, 1, 0, 1)");
        assert!(
            found[0].range.end.character <= found[1].range.start.character,
            "ranges overlap"
        );
    }

    // === $xxx variants ===============================================

    // === codex review of e200d4b: four confirmed defects ==============

    /// `$l[url]`, `$h[link]` and `$p[params]` take a BRACKETED ARGUMENT, and
    /// the text inside those brackets is a URL, not formatted text. The game
    /// does not render a colour there, so reporting one is a false positive --
    /// and worse than most, because accepting the picker's edit rewrites
    /// somebody's URL.
    ///
    /// The original scan had no bracket state at all. Knowing that `l` is a
    /// control letter and not a hex digit told it "not a colour HERE", which
    /// is true and useless: the payload still got scanned.
    #[test]
    fn link_payloads_are_not_scanned_for_colours() {
        for (src, what) in [
            ("\"$l[https://x.example/$f00]label\"", "lower l"),
            ("\"$L[https://x.example/$f00]label\"", "upper L"),
            ("\"$h[$f00]label\"", "h"),
            ("\"$H[$f00]label\"", "upper H"),
            ("\"$p[$f00]label\"", "p"),
            ("\"$P[$f00]label\"", "upper P"),
        ] {
            let full = format!("void f() {{ print({src}); }}");
            let found = colors(&full);
            assert!(
                found.is_empty(),
                "{what}: a colour inside a link payload is not a colour, and \
                 offering an edit there corrupts the URL: {found:?}"
            );
        }
    }

    /// The other half of the same rule: closing the bracket must RESUME
    /// scanning, or the fix would trade a false positive for a false negative
    /// and the test above would pass for the wrong reason.
    #[test]
    fn a_colour_after_a_link_payload_is_still_found() {
        let src = "void f() { print(\"$l[https://x.example/]$f00label$l\"); }";
        let found = colors(src);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_color(&found[0].color, &RED);
        assert_eq!(covered(src, found[0].range), "$f00");
    }

    /// An unterminated `$l[` must not swallow the rest of the file, and must
    /// not panic.
    #[test]
    fn an_unclosed_link_payload_is_bounded_by_the_string() {
        let src = "void f() { print(\"$l[unterminated $f00\"); }";
        assert!(
            colors(src).is_empty(),
            "payload runs to the end of the token"
        );
    }

    /// `upper` was ONE bool for all three digits, set if ANY digit was upper,
    /// then applied to every digit on the way out. So `$aB3` came back `$AB3`:
    /// the picker rewrote source the user had not changed.
    #[test]
    fn mixed_case_dollar_digits_round_trip_exactly() {
        for src in ["\"$aB3\"", "\"$Ab3\"", "\"$abC\"", "\"$AbC\""] {
            let full = format!("void f() {{ print({src}); }}");
            let found = colors(&full);
            assert_eq!(found.len(), 1, "{src}: {found:?}");
            let out = color_presentations(&full, found[0].range, found[0].color);
            assert_eq!(out.len(), 1, "{src}");
            // Same colour in, same TEXT out -- a no-op edit must be a no-op.
            let want = src.trim_matches('"');
            assert_eq!(out[0].label, want, "{src}: case not preserved per digit");
        }
    }

    /// `0x…` lexes as `HexLit` and was rejected outright; `0b…`/`0o…` lex as
    /// `IntLit` but fail a decimal parse. All three are legal AngelScript and
    /// can sit inside 0..1, so all three were silently missing a swatch.
    #[test]
    fn non_decimal_literal_components_are_swatched() {
        for (src, what) in [
            ("vec3(0x1, 0x0, 0x0)", "hex"),
            ("vec3(0X1, 0X0, 0X0)", "hex upper"),
            ("vec3(0b1, 0b0, 0b0)", "binary"),
            ("vec3(0o1, 0o0, 0o0)", "octal"),
        ] {
            let full = format!("void f() {{ g({src}); }}");
            let found = colors(&full);
            assert_eq!(found.len(), 1, "{what}: {found:?}");
            assert_color(&found[0].color, &RED);
            assert_eq!(covered(&full, found[0].range), src, "{what}");
        }
    }

    /// A non-decimal component still has to respect the 0..1 gate.
    #[test]
    fn out_of_range_non_decimal_components_are_still_rejected() {
        let src = "void f() { g(vec3(0xff, 0x0, 0x0)); }";
        assert!(colors(src).is_empty(), "0xff is 255, not a colour channel");
    }

    #[test]
    fn dollar_codes_accept_any_hex_case() {
        for (src, what) in [
            ("\"$0f0\"", "lower"),
            ("\"$0F0\"", "upper"),
            // Genuinely mixed. This slot used to repeat the uppercase sample
            // verbatim, so it asserted nothing the row above had not already
            // asserted, and it hid the per-digit case bug.
            ("\"$0Fa\"", "mixed"),
        ] {
            let full = format!("void f() {{ print({src}); }}");
            let found = colors(&full);
            assert_eq!(found.len(), 1, "{what}: {found:?}");
            assert!(approx(found[0].color.green, 1.0), "{what}");
        }
    }

    #[test]
    fn the_presentation_preserves_hex_case() {
        let lower = "void f() { print(\"$f00\"); }";
        let out = color_presentations(lower, colors(lower)[0].range, RED);
        assert_eq!(out[0].label, "$f00");

        let upper = "void f() { print(\"$F00\"); }";
        let out = color_presentations(upper, colors(upper)[0].range, RED);
        assert_eq!(out[0].label, "$F00");
    }

    #[test]
    fn a_non_hex_dollar_code_is_not_a_colour() {
        // `$zzz` is not hex; `$z`, `$g`, `$o` and friends are style codes,
        // and none of the control letters are hex digits, which is what makes
        // three characters of lookahead sufficient.
        for src in [
            "\"$zzz\"",
            "\"$z\"",
            "\"$g red\"",
            "\"$o bold\"",
            "\"$\"",
            "\"$f0\"",
        ] {
            let full = format!("void f() {{ print({src}); }}");
            let found = colors(&full);
            assert!(found.is_empty(), "{src} is not a colour: {found:?}");
        }
    }

    #[test]
    fn a_doubled_dollar_is_a_literal_dollar_not_a_colour() {
        // `$$` renders one `$`; `"$$f00"` shows the text `f00`.
        let src = "void f() { print(\"$$f00\"); }";
        assert!(colors(src).is_empty(), "{:?}", colors(src));

        // But a code FOLLOWING an escaped dollar is still a code.
        let src = "void f() { print(\"$$$f00\"); }";
        let found = colors(src);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(covered(src, found[0].range), "$f00");
    }

    #[test]
    fn several_codes_in_one_string_each_get_their_own_range() {
        let src = "void f() { print(\"$f00red$0f0green\"); }";
        let found = colors(src);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(covered(src, found[0].range), "$f00");
        assert_eq!(covered(src, found[1].range), "$0f0");
    }

    #[test]
    fn a_code_at_the_very_start_and_very_end_of_a_string_is_found() {
        let src = "void f() { print(\"$f00\"); }";
        assert_eq!(colors(src).len(), 1);
        let src = "void f() { print(\"hi$f00\"); }";
        let found = colors(src);
        assert_eq!(found.len(), 1);
        assert_eq!(covered(src, found[0].range), "$f00");
    }

    /// The one the coordinator singled out. LSP ranges are UTF-16 code units,
    /// and this repo's TUI measures DISPLAY WIDTH elsewhere — a deliberate
    /// contrast, not a model to copy here.
    #[test]
    fn ranges_are_utf16_code_units_not_bytes_or_display_columns() {
        // 'é' is 2 bytes / 1 UTF-16 unit; '🎨' is 4 bytes / 2 UTF-16 units
        // and 2 display columns. All three measures disagree, so a wrong one
        // cannot pass by coincidence.
        let src = "void f() { print(\"é🎨$f00\"); }";
        let found = colors(src);
        assert_eq!(found.len(), 1, "{found:?}");

        let prefix = "void f() { print(\"é🎨";
        let expected = prefix.encode_utf16().count() as u32;
        assert_eq!(
            found[0].range.start.character,
            expected,
            "start must be UTF-16 units ({expected}), not bytes ({}) \
             or display columns",
            prefix.len()
        );
        assert_eq!(found[0].range.end.character, expected + 4);
        assert_eq!(covered(src, found[0].range), "$f00");
    }

    #[test]
    fn a_code_in_a_comment_is_reported() {
        // Deliberate: commented-out UI code and "$f00 is red" notes are
        // exactly where a reader wants the swatch.
        let line = "void f() { } // red is $f00";
        let found = colors(line);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(covered(line, found[0].range), "$f00");

        let block = "/* $0f0 green */ void f() { }";
        assert_eq!(colors(block).len(), 1);
    }

    #[test]
    fn prose_that_looks_like_a_code_is_reported_too_and_that_is_the_trade() {
        // The accepted cost of scanning comments. Pinned so it stays a known
        // trade rather than a surprise: if this ever becomes intolerable the
        // fix is to require a quoted string inside the comment, and this test
        // is where that decision gets revisited.
        let src = "void f() { } // the licence costs $100 per seat";
        assert_eq!(colors(src).len(), 1);
    }

    #[test]
    fn an_unknown_range_yields_no_presentation() {
        let src = "void f() { g(vec3(1, 0, 0)); }";
        let nowhere = Range::new(
            tower_lsp::lsp_types::Position::new(0, 0),
            tower_lsp::lsp_types::Position::new(0, 4),
        );
        assert!(
            color_presentations(src, nowhere, RED).is_empty(),
            "a range that is not a colour must not be overwritten"
        );
    }

    #[test]
    fn a_colour_in_a_preprocessor_inactive_branch_is_still_reported() {
        // This reads the RAW source, not the preprocessor's masked output, so
        // a colour inside a dead `#if` branch keeps its swatch. Deliberate:
        // the text is on screen and editable, and greying it out is the
        // inactive-region feature's job, not a reason to hide the colour.
        let src = "#if DEV\nvoid f() { g(vec3(1, 0, 0)); }\n#endif\n";
        let found = colors(src);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_color(&found[0].color, &RED);
    }

    #[test]
    fn colours_come_back_in_source_order() {
        let src = "void f() {\n  print(\"$f00\");\n  g(vec3(0, 1, 0));\n}";
        let found = colors(src);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].range.start.line < found[1].range.start.line);
    }
}
