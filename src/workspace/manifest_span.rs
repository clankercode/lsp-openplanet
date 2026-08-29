//! Byte ranges for manifest diagnostics.
//!
//! [`Manifest::validate`](crate::workspace::manifest::Manifest::validate)
//! reports problems by key path (`meta.version`), because it works on the
//! serde-deserialized manifest and serde keeps no positions. This module
//! re-parses the SAME source with `toml_edit`, which preserves spans, and
//! turns a key path into the range an editor should squiggle.
//!
//! The lookup degrades deliberately, because the most important diagnostic —
//! a missing required key — has nothing to point at by definition:
//!
//! 1. key present  → the key through the end of its value (`version = "1.0"`)
//! 2. key absent   → the header key of the table that should hold it (`[meta]`)
//! 3. table absent → the start of the file
//!
//! Every range returned is clamped into `source` and to char boundaries, so a
//! caller can convert it to an LSP position without checking anything.

use std::ops::Range;

use toml_edit::{Document, TableLike};

use crate::workspace::manifest::ManifestAnchor;

/// Resolve `anchor` to a byte range inside `source`.
pub fn anchor_range(source: &str, anchor: &ManifestAnchor) -> Range<usize> {
    let range = match anchor {
        ManifestAnchor::Span(span) => span.clone(),
        ManifestAnchor::Key { path, index } => key_range(source, path, *index),
    };
    clamp(source, range)
}

/// Byte range of the dotted `path` in `source`, following the degradation
/// order documented at the module level.
fn key_range(source: &str, path: &str, index: Option<usize>) -> Range<usize> {
    let Ok(doc) = Document::parse(source) else {
        // Unparseable TOML has no keys to point at. A parse error is
        // published separately, with its own span.
        return 0..0;
    };

    let mut segments = path.split('.').peekable();
    let mut table: &dyn TableLike = doc.as_table();
    // Innermost table we actually reached — the fallback anchor when the
    // final key turns out to be absent.
    let mut table_anchor = 0..0;

    while let Some(segment) = segments.next() {
        let Some((key, item)) = table.get_key_value(segment) else {
            return table_anchor;
        };
        if segments.peek().is_none() {
            return value_range(key.span(), item, index).unwrap_or(table_anchor);
        }
        let Some(next) = item.as_table_like() else {
            // A path segment that is not a table (`script = 3`): the best we
            // can do is point at the key that broke the walk.
            return key.span().unwrap_or(table_anchor);
        };
        table_anchor = key
            .span()
            .or_else(|| item.span())
            .unwrap_or(table_anchor.clone());
        table = next;
    }

    table_anchor
}

/// Range for a key that IS present: the key through the end of its value, or
/// just the one array element when `index` names one.
fn value_range(
    key_span: Option<Range<usize>>,
    item: &toml_edit::Item,
    index: Option<usize>,
) -> Option<Range<usize>> {
    if let Some(index) = index {
        // An element span is strictly better than the whole array: with ten
        // exports listed, the third one being missing should squiggle the
        // third one.
        if let Some(element) = item.as_array().and_then(|array| array.get(index)) {
            if let Some(span) = element.span() {
                return Some(span);
            }
        }
    }
    let key_span = key_span?;
    match item.span() {
        Some(value) if value.end > key_span.start => Some(key_span.start..value.end),
        _ => Some(key_span),
    }
}

/// Keep `range` inside `source` and on char boundaries, and never inverted.
fn clamp(source: &str, range: Range<usize>) -> Range<usize> {
    let floor = |mut offset: usize| {
        offset = offset.min(source.len());
        while !source.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    };
    let start = floor(range.start);
    let end = floor(range.end).max(start);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "\
# a leading comment
[meta]
name    = \"Test\"
version = \"1.0.0\"
perms   = \"full\"

[script]
exports = [\"A.as\", \"B.as\"]
shared_exports = [\"S.as\"]
";

    fn text(source: &str, range: Range<usize>) -> &str {
        &source[range]
    }

    #[test]
    fn present_key_range_covers_the_key_and_its_value() {
        let range = anchor_range(FULL, &ManifestAnchor::key("meta.version"));
        assert_eq!(text(FULL, range), "version = \"1.0.0\"");
    }

    /// Every scalar key in the manifest resolves the same way — the rule is
    /// not special-cased for `version`.
    #[test]
    fn present_key_range_generalises_over_keys_and_tables() {
        for (path, want) in [
            ("meta.name", "name    = \"Test\""),
            ("meta.version", "version = \"1.0.0\""),
            ("meta.perms", "perms   = \"full\""),
            ("script.exports", "exports = [\"A.as\", \"B.as\"]"),
            ("script.shared_exports", "shared_exports = [\"S.as\"]"),
        ] {
            let range = anchor_range(FULL, &ManifestAnchor::key(path));
            assert_eq!(text(FULL, range), want, "{path}");
        }
    }

    #[test]
    fn array_element_range_covers_only_that_element() {
        for (index, want) in [(0, "\"A.as\""), (1, "\"B.as\"")] {
            let range = anchor_range(FULL, &ManifestAnchor::element("script.exports", index));
            assert_eq!(text(FULL, range), want, "element {index}");
        }
    }

    /// An index past the end of the array (or on a key that is not an array)
    /// falls back to the whole key rather than producing nothing.
    #[test]
    fn out_of_range_element_falls_back_to_the_key() {
        let range = anchor_range(FULL, &ManifestAnchor::element("script.exports", 9));
        assert_eq!(text(FULL, range), "exports = [\"A.as\", \"B.as\"]");
        let range = anchor_range(FULL, &ManifestAnchor::element("meta.version", 0));
        assert_eq!(text(FULL, range), "version = \"1.0.0\"");
    }

    #[test]
    fn absent_key_anchors_to_the_table_header() {
        let source = "# leading\n[meta]\nname = \"Test\"\n";
        let range = anchor_range(source, &ManifestAnchor::key("meta.version"));
        assert_eq!(text(source, range.clone()), "meta");
        // ...and it is the header, not the top of the file.
        assert!(range.start > 0, "must not collapse to the file start");
    }

    /// The header fallback applies to every table, not just `[meta]`.
    #[test]
    fn absent_key_anchors_to_its_own_table_header() {
        let source = "[meta]\nversion = \"1\"\n\n[script]\nmodule = \"M\"\n";
        for path in ["meta.perms", "script.exports"] {
            let table = path.split('.').next().unwrap();
            let range = anchor_range(source, &ManifestAnchor::key(path));
            assert_eq!(text(source, range.clone()), table, "{path}");
            // The header of its OWN table, not of the first one in the file.
            assert_eq!(
                range.start,
                source.find(&format!("[{table}]")).unwrap() + 1,
                "{path}"
            );
        }
    }

    #[test]
    fn absent_table_anchors_to_the_file_start() {
        for source in ["", "\n", "[script]\nmodule = \"M\"\n"] {
            let range = anchor_range(source, &ManifestAnchor::key("meta.version"));
            assert_eq!(range, 0..0, "{source:?}");
        }
    }

    #[test]
    fn unparseable_source_yields_the_file_start_for_a_key_anchor() {
        let range = anchor_range("this is not toml [[[", &ManifestAnchor::key("meta.version"));
        assert_eq!(range, 0..0);
    }

    /// Inline tables carry spans too — `meta = { version = "1" }` must
    /// resolve exactly like a `[meta]` section.
    #[test]
    fn inline_tables_resolve_like_section_tables() {
        let source = "meta = { name = \"T\", version = \"1.0\" }\n";
        let range = anchor_range(source, &ManifestAnchor::key("meta.version"));
        assert_eq!(text(source, range), "version = \"1.0\"");
        let source = "meta = { name = \"T\" }\n";
        let range = anchor_range(source, &ManifestAnchor::key("meta.version"));
        assert_eq!(text(source, range), "meta");
    }

    /// A path segment that is not a table at all must not panic or run off
    /// the end — it anchors on the key that broke the walk.
    #[test]
    fn non_table_segment_anchors_on_that_key() {
        let source = "meta = 3\n";
        let range = anchor_range(source, &ManifestAnchor::key("meta.version"));
        assert_eq!(text(source, range), "meta");
    }

    /// Every anchor, on every source, must land inside the document — the
    /// range is handed straight to the LSP position mapper.
    #[test]
    fn every_range_is_within_document_bounds() {
        let sources = [
            "",
            "\n\n\n",
            FULL,
            "this is not toml [[[",
            "[meta]\n",
            "meta = 3\n",
            // Multi-byte content: the clamp must land on char boundaries.
            "[meta]\nname = \"héllo ✅\"\n",
        ];
        let anchors = [
            ManifestAnchor::key("meta.version"),
            ManifestAnchor::key("meta.perms"),
            ManifestAnchor::key("script.exports"),
            ManifestAnchor::key("nope.nothing.here"),
            ManifestAnchor::element("script.exports", 0),
            ManifestAnchor::element("script.exports", 99),
            ManifestAnchor::Span(0..1_000_000),
            // Deliberately inverted: `Range` is just two numbers here, and
            // a bad one must be normalised rather than panic a slice index.
            #[allow(clippy::reversed_empty_ranges)]
            ManifestAnchor::Span(500..3),
        ];
        for source in sources {
            for anchor in &anchors {
                let range = anchor_range(source, anchor);
                assert!(
                    range.start <= range.end,
                    "{source:?} {anchor:?}: inverted range {range:?}"
                );
                assert!(
                    range.end <= source.len(),
                    "{source:?} {anchor:?}: {range:?} past end {}",
                    source.len()
                );
                assert!(
                    source.is_char_boundary(range.start) && source.is_char_boundary(range.end),
                    "{source:?} {anchor:?}: {range:?} splits a character"
                );
            }
        }
    }

    #[test]
    fn multibyte_keys_resolve_on_char_boundaries() {
        let source = "[meta]\nname = \"héllo ✅\"\nversion = \"1\"\n";
        let range = anchor_range(source, &ManifestAnchor::key("meta.version"));
        assert_eq!(text(source, range), "version = \"1\"");
    }
}
