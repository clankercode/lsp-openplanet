//! `textDocument/codeAction` handlers.
//!
//! Provides two starter kinds:
//!
//! 1. **"Did you mean X?" quick-fixes** driven by `unknown type` and
//!    `undefined identifier` diagnostics. Candidates come from the workspace
//!    symbol table and the external type index (short-name map). Similarity
//!    is scored with Levenshtein distance (≤ 2), plus cheap fallbacks:
//!    case-insensitive substring containment and common-prefix length ≥ 3.
//! 2. **"Wrap in try/catch" refactor** — always offered regardless of the
//!    diagnostic context.

use std::collections::{HashMap, HashSet};

use tower_lsp::lsp_types::*;

use crate::server::diagnostics::position_to_offset;
use crate::symbols::SymbolTable;
use crate::typedb::TypeIndex;

/// Entry point invoked by the LSP `code_action` handler.
pub fn code_actions(
    uri: &Url,
    source: &str,
    range: Range,
    diagnostics: &[Diagnostic],
    workspace: &SymbolTable,
    type_index: Option<&TypeIndex>,
) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();
    actions.extend(quick_fix_did_you_mean(
        uri,
        diagnostics,
        workspace,
        type_index,
    ));
    actions.extend(refactor_wrap_in_try_catch(uri, source, range));
    actions
}

fn quick_fix_did_you_mean(
    uri: &Url,
    diagnostics: &[Diagnostic],
    workspace: &SymbolTable,
    type_index: Option<&TypeIndex>,
) -> Vec<CodeActionOrCommand> {
    let mut out = Vec::new();
    for diag in diagnostics {
        if !is_did_you_mean_candidate(&diag.message) {
            continue;
        }
        let Some(needle) = extract_name_from_diagnostic(&diag.message) else {
            continue;
        };
        if needle.is_empty() {
            continue;
        }
        let candidates = find_candidates(needle, workspace, type_index);
        for (i, (name, _dist)) in candidates.iter().enumerate() {
            let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
            changes.insert(
                uri.clone(),
                vec![TextEdit {
                    range: diag.range,
                    new_text: name.clone(),
                }],
            );
            let action = CodeAction {
                title: format!("Did you mean '{}'?", name),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diag.clone()]),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    document_changes: None,
                    change_annotations: None,
                }),
                command: None,
                is_preferred: Some(i == 0),
                disabled: None,
                data: None,
            };
            out.push(CodeActionOrCommand::CodeAction(action));
        }
    }
    out
}

fn refactor_wrap_in_try_catch(
    uri: &Url,
    source: &str,
    range: Range,
) -> Vec<CodeActionOrCommand> {
    let start_off = position_to_offset(source, range.start);
    let end_off = position_to_offset(source, range.end);
    let (start_off, end_off) = if start_off <= end_off {
        (start_off, end_off)
    } else {
        (end_off, start_off)
    };
    let selected = source.get(start_off..end_off).unwrap_or("");
    let new_text = format!("try {{\n    {}\n}} catch {{ }}\n", selected);

    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    changes.insert(
        uri.clone(),
        vec![TextEdit {
            range,
            new_text,
        }],
    );
    let action = CodeAction {
        title: "Wrap in try/catch".to_string(),
        kind: Some(CodeActionKind::REFACTOR_REWRITE),
        diagnostics: None,
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }),
        command: None,
        is_preferred: None,
        disabled: None,
        data: None,
    };
    vec![CodeActionOrCommand::CodeAction(action)]
}

fn is_did_you_mean_candidate(message: &str) -> bool {
    message.starts_with("unknown type `") || message.starts_with("undefined identifier `")
}

fn extract_name_from_diagnostic(message: &str) -> Option<&str> {
    let start = message.find('`')? + 1;
    let rest = &message[start..];
    let end = rest.find('`')?;
    Some(&rest[..end])
}

/// Bounded Levenshtein distance. Returns a large sentinel for over-long
/// inputs so the caller naturally skips them.
fn levenshtein(a: &str, b: &str) -> usize {
    if a.len() > 30 || b.len() > 30 {
        return 999;
    }
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 0..m {
        curr[0] = i + 1;
        for j in 0..n {
            let cost = if a_chars[i] == b_chars[j] { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1)
                .min(curr[j] + 1)
                .min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

fn is_similar(needle: &str, candidate: &str) -> Option<usize> {
    let needle_lower = needle.to_lowercase();
    let cand_lower = candidate.to_lowercase();
    let dist = levenshtein(&needle_lower, &cand_lower);
    if dist <= 2 {
        return Some(dist);
    }
    if needle.len() >= 3
        && candidate.len() >= 3
        && cand_lower.contains(&needle_lower)
    {
        return Some(dist);
    }
    if common_prefix_len(&needle_lower, &cand_lower) >= 3 {
        return Some(dist);
    }
    None
}

fn find_candidates(
    needle: &str,
    workspace: &SymbolTable,
    type_index: Option<&TypeIndex>,
) -> Vec<(String, usize)> {
    let mut candidates: Vec<(String, usize)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // Workspace symbols: use the tail segment ("Foo" from "Ns::Foo").
    for sym in workspace.all_symbols() {
        let bare = sym.name.rsplit("::").next().unwrap_or(&sym.name);
        if bare == needle {
            continue;
        }
        if let Some(dist) = is_similar(needle, bare) {
            if seen.insert(bare.to_string()) {
                candidates.push((bare.to_string(), dist));
            }
        }
    }

    // External type index short names (types + enums).
    if let Some(idx) = type_index {
        for short in idx.all_short_names() {
            if short == needle {
                continue;
            }
            if let Some(dist) = is_similar(needle, &short) {
                if seen.insert(short.clone()) {
                    candidates.push((short, dist));
                }
            }
        }
    }

    candidates.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    candidates.truncate(3);
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Span;
    use crate::symbols::scope::{Symbol, SymbolKind};

    fn mk_range(l0: u32, c0: u32, l1: u32, c1: u32) -> Range {
        Range::new(Position::new(l0, c0), Position::new(l1, c1))
    }

    fn mk_diag(range: Range, message: &str) -> Diagnostic {
        Diagnostic {
            range,
            severity: Some(DiagnosticSeverity::ERROR),
            message: message.to_string(),
            source: Some("openplanet-lsp".to_string()),
            ..Default::default()
        }
    }

    fn workspace_with_class(name: &str) -> SymbolTable {
        let mut table = SymbolTable::new();
        let fid = table.allocate_file_id();
        let sym = Symbol {
            name: name.to_string(),
            kind: SymbolKind::Class {
                parent: None,
                members: Vec::new(),
            },
            span: Span { start: 0, end: 0 },
            file_id: fid,
            doc: None,
        };
        table.set_file_symbols(fid, vec![sym]);
        table
    }

    #[test]
    fn extract_name_works() {
        assert_eq!(
            extract_name_from_diagnostic("unknown type `Foo`"),
            Some("Foo")
        );
        assert_eq!(
            extract_name_from_diagnostic("undefined identifier `bar`"),
            Some("bar")
        );
        assert_eq!(extract_name_from_diagnostic("no backtick here"), None);
    }

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("foo", "foo"), 0);
        assert_eq!(levenshtein("foo", "fop"), 1);
        assert_eq!(levenshtein("foo", "bar"), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn is_did_you_mean_candidate_detects_messages() {
        assert!(is_did_you_mean_candidate("unknown type `Foo`"));
        assert!(is_did_you_mean_candidate("undefined identifier `bar`"));
        assert!(!is_did_you_mean_candidate("expected ; after expression"));
    }

    #[test]
    fn quickfix_did_you_mean_finds_typo() {
        let table = workspace_with_class("Foo");
        let uri = Url::parse("file:///tmp/a.as").unwrap();
        let diag_range = mk_range(1, 4, 1, 7);
        let diag = mk_diag(diag_range, "unknown type `Foa`");
        let actions = code_actions(&uri, "", mk_range(0, 0, 0, 0), &[diag], &table, None);

        let has_foo = actions.iter().any(|a| match a {
            CodeActionOrCommand::CodeAction(ca) => ca.title.contains("Foo"),
            _ => false,
        });
        assert!(
            has_foo,
            "expected a 'Did you mean Foo?' action in {:?}",
            actions
                .iter()
                .filter_map(|a| match a {
                    CodeActionOrCommand::CodeAction(ca) => Some(ca.title.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        );

        // The 'Foo' quickfix should also carry a WorkspaceEdit replacing the
        // diagnostic range with "Foo" and be marked as preferred.
        let foo_action = actions.iter().find_map(|a| match a {
            CodeActionOrCommand::CodeAction(ca) if ca.title.contains("Foo") => Some(ca),
            _ => None,
        });
        let foo_action = foo_action.expect("Foo action exists");
        assert_eq!(foo_action.is_preferred, Some(true));
        let edit = foo_action.edit.as_ref().expect("edit set");
        let changes = edit.changes.as_ref().expect("changes set");
        let edits = changes.get(&uri).expect("edits for uri");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "Foo");
        assert_eq!(edits[0].range, diag_range);
    }

    #[test]
    fn quickfix_did_you_mean_ignores_unrelated_diagnostics() {
        let table = workspace_with_class("Foo");
        let uri = Url::parse("file:///tmp/a.as").unwrap();
        let diag = mk_diag(mk_range(0, 0, 0, 3), "some other error");
        let actions = quick_fix_did_you_mean(&uri, &[diag], &table, None);
        assert!(actions.is_empty());
    }

    #[test]
    fn wrap_in_try_catch_always_offered() {
        let table = SymbolTable::new();
        let uri = Url::parse("file:///tmp/a.as").unwrap();
        // Empty diagnostics; some arbitrary range over a tiny source.
        let source = "print(x);";
        let actions = code_actions(
            &uri,
            source,
            mk_range(0, 0, 0, 9),
            &[],
            &table,
            None,
        );
        let refactors = actions
            .iter()
            .filter_map(|a| match a {
                CodeActionOrCommand::CodeAction(ca)
                    if ca.kind == Some(CodeActionKind::REFACTOR_REWRITE) =>
                {
                    Some(ca)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(refactors.len(), 1);
        let action = refactors[0];
        assert_eq!(action.title, "Wrap in try/catch");
        let edits = action
            .edit
            .as_ref()
            .and_then(|e| e.changes.as_ref())
            .and_then(|c| c.get(&uri))
            .expect("workspace edit for uri");
        assert_eq!(edits.len(), 1);
        assert!(edits[0].new_text.contains("try {"));
        assert!(edits[0].new_text.contains("print(x);"));
        assert!(edits[0].new_text.contains("} catch { }"));
    }

    #[test]
    fn common_prefix_matches_long_prefix() {
        assert!(is_similar("GetFoo", "GetFooBar").is_some());
    }
}
