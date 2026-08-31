use std::collections::HashSet;

use tower_lsp::lsp_types::{
    CodeAction, CodeActionKind, Command, Diagnostic, NumberOrString, Range,
};

use super::CodeActionRequest;
use crate::preprocessor::eval::{eval_condition, tokenize_condition, CondToken};
use crate::server::diagnostics::{offset_to_position, position_to_offset, DIAGNOSTIC_SOURCE};

const SET_DEFINE_COMMAND: &str = "openplanetLsp.defines.set";

#[derive(Clone, Debug)]
struct DefineOccurrence {
    name: String,
    start: usize,
    end: usize,
}

#[derive(Clone, Debug)]
struct ConditionalArm {
    directive_start: usize,
    body_start: usize,
    body_end: usize,
    active: bool,
    occurrences: Vec<DefineOccurrence>,
    /// For `#else`, the identifiers in the conditions which precede it.
    chain_occurrences: Vec<DefineOccurrence>,
    /// The one state change which is sufficient to make this arm active,
    /// when the arm is syntactically simple. Compound arms deliberately have
    /// no preferred state even if one toggle happens to suffice today.
    simple_activation: Option<(String, bool)>,
}

#[derive(Debug)]
struct ConditionalModel {
    arms: Vec<ConditionalArm>,
    directive_occurrences: Vec<DefineOccurrence>,
}

#[derive(Debug)]
struct Frame {
    parent_active: bool,
    branch_taken: bool,
    current_arm: usize,
    arm_count: usize,
    first_simple_true: Option<(String, bool)>,
    chain_occurrences: Vec<DefineOccurrence>,
}

#[derive(Clone, Copy, Debug)]
enum ParsedDirective<'a> {
    If { condition: &'a str, start: usize },
    Elif { condition: &'a str, start: usize },
    Else,
    Endif,
}

pub(super) fn define_actions(req: &CodeActionRequest<'_>) -> Vec<CodeAction> {
    let source = req.analysis.source.as_str();
    let defines = req.defines;
    let model = parse_conditionals(source, defines);
    let mut actions = Vec::new();

    // A cursor/selection on a directive identifier always gets the literal
    // state action. This path does not depend on a diagnostic, so live arms
    // are covered too.
    for occurrence in &model.directive_occurrences {
        let occurrence_range = byte_range(source, occurrence.start, occurrence.end);
        if occurrence_range.start <= req.range.start && req.range.end <= occurrence_range.end {
            push_action(
                &mut actions,
                define_action(occurrence, defines, None, false),
            );
        }
    }

    // A body diagnostic is useful only when it is our current structural
    // inactive-region record. Neither its English text nor an old range is
    // trusted. Associate it with the smallest inactive arm which contains it;
    // that is the nearest controlling arm for nested conditionals.
    for diagnostic in req.context.diagnostics.iter().filter(|diagnostic| {
        super::ranges_touch(&diagnostic.range, &req.range)
            && is_current_inactive_diagnostic(source, req.analysis, diagnostic)
    }) {
        let start = position_to_offset(source, diagnostic.range.start);
        let end = position_to_offset(source, diagnostic.range.end);
        let Some((arm_index, arm)) = model
            .arms
            .iter()
            .enumerate()
            .filter(|(_, arm)| !arm.active && arm.body_start <= start && end <= arm.body_end)
            .min_by_key(|(_, arm)| arm.body_end.saturating_sub(arm.body_start))
        else {
            continue;
        };

        let candidates = if arm.occurrences.is_empty() {
            &arm.chain_occurrences
        } else {
            &arm.occurrences
        };
        let mut names = HashSet::new();
        for occurrence in candidates {
            if !names.insert(occurrence.name.as_str()) {
                continue;
            }
            let currently_on = defines.contains(&occurrence.name);
            let proposed_on = !currently_on;
            let preferred = arm
                .simple_activation
                .as_ref()
                .is_some_and(|(name, on)| name == &occurrence.name && *on == proposed_on)
                && arm_becomes_active(source, defines, arm_index, &occurrence.name, proposed_on);
            push_action(
                &mut actions,
                define_action(occurrence, defines, Some(diagnostic.clone()), preferred),
            );
        }
    }

    actions
}

fn define_action(
    occurrence: &DefineOccurrence,
    defines: &HashSet<String>,
    diagnostic: Option<Diagnostic>,
    preferred: bool,
) -> CodeAction {
    let on = !defines.contains(&occurrence.name);
    let verb = if on { "Enable" } else { "Disable" };
    let title = format!("{verb} define {}", occurrence.name);
    CodeAction {
        title: title.clone(),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: diagnostic.map(|diagnostic| vec![diagnostic]),
        edit: None,
        command: Some(Command {
            title,
            command: SET_DEFINE_COMMAND.to_string(),
            arguments: Some(vec![serde_json::json!({
                "name": occurrence.name,
                "on": on,
            })]),
        }),
        is_preferred: preferred.then_some(true),
        disabled: None,
        data: None,
    }
}

fn push_action(actions: &mut Vec<CodeAction>, action: CodeAction) {
    let command = action.command.as_ref().map(|command| &command.command);
    let arguments = action
        .command
        .as_ref()
        .and_then(|command| command.arguments.as_ref());
    if let Some(existing) = actions.iter_mut().find(|existing| {
        existing.title == action.title
            && existing.command.as_ref().map(|command| &command.command) == command
            && existing
                .command
                .as_ref()
                .and_then(|command| command.arguments.as_ref())
                == arguments
    }) {
        if (action.is_preferred == Some(true) && existing.is_preferred != Some(true))
            || (action.diagnostics.is_some() && existing.diagnostics.is_none())
        {
            *existing = action;
        }
    } else {
        actions.push(action);
    }
}

fn is_current_inactive_diagnostic(
    source: &str,
    analysis: &crate::analysis::DocumentAnalysis,
    diagnostic: &Diagnostic,
) -> bool {
    diagnostic.source.as_deref() == Some(DIAGNOSTIC_SOURCE)
        && diagnostic.code == Some(NumberOrString::String("inactive-region".to_string()))
        && analysis.preprocess.inactive_regions.iter().any(|span| {
            byte_range(source, span.start as usize, span.end as usize) == diagnostic.range
        })
}

fn arm_becomes_active(
    source: &str,
    defines: &HashSet<String>,
    arm_index: usize,
    name: &str,
    on: bool,
) -> bool {
    let mut proposed = defines.clone();
    if on {
        proposed.insert(name.to_string());
    } else {
        proposed.remove(name);
    }
    parse_conditionals(source, &proposed)
        .arms
        .get(arm_index)
        .is_some_and(|arm| arm.active)
}

fn byte_range(source: &str, start: usize, end: usize) -> Range {
    Range::new(
        offset_to_position(source, start),
        offset_to_position(source, end),
    )
}

fn parse_conditionals(source: &str, defines: &HashSet<String>) -> ConditionalModel {
    let mut arms: Vec<ConditionalArm> = Vec::new();
    let mut directive_occurrences = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut active = true;
    let mut line_start = 0usize;

    for raw_line in source.split_inclusive('\n') {
        let line_end = line_start + raw_line.len();
        match parse_directive(raw_line, line_start) {
            Some(ParsedDirective::If { condition, start }) => {
                let occurrences = condition_occurrences(condition, start);
                directive_occurrences.extend(occurrences.iter().cloned());
                let simple = simple_true_state(condition);
                let branch_active = active && eval_condition(condition, defines);
                let arm_index = arms.len();
                arms.push(ConditionalArm {
                    directive_start: line_start,
                    body_start: line_end,
                    body_end: source.len(),
                    active: branch_active,
                    occurrences: occurrences.clone(),
                    chain_occurrences: occurrences.clone(),
                    simple_activation: simple.clone(),
                });
                stack.push(Frame {
                    parent_active: active,
                    branch_taken: branch_active,
                    current_arm: arm_index,
                    arm_count: 1,
                    first_simple_true: simple,
                    chain_occurrences: occurrences,
                });
                active = branch_active;
            }
            Some(ParsedDirective::Elif { condition, start }) => {
                let Some(frame) = stack.last_mut() else {
                    line_start = line_end;
                    continue;
                };
                arms[frame.current_arm].body_end = line_start;
                let occurrences = condition_occurrences(condition, start);
                directive_occurrences.extend(occurrences.iter().cloned());
                let branch_active = frame.parent_active
                    && !frame.branch_taken
                    && eval_condition(condition, defines);
                if branch_active {
                    frame.branch_taken = true;
                }
                frame.arm_count += 1;
                frame.chain_occurrences.extend(occurrences.iter().cloned());
                let arm_index = arms.len();
                arms.push(ConditionalArm {
                    directive_start: line_start,
                    body_start: line_end,
                    body_end: source.len(),
                    active: branch_active,
                    occurrences,
                    chain_occurrences: frame.chain_occurrences.clone(),
                    simple_activation: simple_true_state(condition),
                });
                frame.current_arm = arm_index;
                active = branch_active;
            }
            Some(ParsedDirective::Else) => {
                let Some(frame) = stack.last_mut() else {
                    line_start = line_end;
                    continue;
                };
                arms[frame.current_arm].body_end = line_start;
                let branch_active = frame.parent_active && !frame.branch_taken;
                let simple_activation = (frame.arm_count == 1)
                    .then(|| frame.first_simple_true.clone())
                    .flatten()
                    .map(|(name, on)| (name, !on));
                let arm_index = arms.len();
                arms.push(ConditionalArm {
                    directive_start: line_start,
                    body_start: line_end,
                    body_end: source.len(),
                    active: branch_active,
                    occurrences: Vec::new(),
                    chain_occurrences: frame.chain_occurrences.clone(),
                    simple_activation,
                });
                frame.branch_taken = true;
                frame.current_arm = arm_index;
                frame.arm_count += 1;
                active = branch_active;
            }
            Some(ParsedDirective::Endif) => {
                if let Some(frame) = stack.pop() {
                    arms[frame.current_arm].body_end = line_start;
                    active = frame.parent_active;
                }
            }
            None => {}
        }
        line_start = line_end;
    }
    for frame in stack {
        arms[frame.current_arm].body_end = source.len();
    }

    // The index is used to compare the same arm under one proposed define
    // change. Source parsing is deterministic, including malformed nesting.
    debug_assert!(arms
        .windows(2)
        .all(|pair| pair[0].directive_start <= pair[1].directive_start));
    ConditionalModel {
        arms,
        directive_occurrences,
    }
}

fn parse_directive(raw_line: &str, absolute_start: usize) -> Option<ParsedDirective<'_>> {
    let without_newline = raw_line.trim_end_matches(['\r', '\n']);
    let leading = without_newline.len() - without_newline.trim_start().len();
    let trimmed = &without_newline[leading..];
    let after_hash = trimmed.strip_prefix('#')?;
    let hash_offset = absolute_start + leading;
    let after_hash_ws = after_hash.len() - after_hash.trim_start().len();
    let rest = &after_hash[after_hash_ws..];
    let rest_offset = hash_offset + 1 + after_hash_ws;

    for (keyword, is_if) in [("elif", false), ("if", true)] {
        let Some(after_keyword) = rest.strip_prefix(keyword) else {
            continue;
        };
        if !after_keyword.is_empty()
            && !after_keyword.starts_with(|character: char| character.is_whitespace())
        {
            continue;
        }
        let condition_ws = after_keyword.len() - after_keyword.trim_start().len();
        let raw_condition = &after_keyword[condition_ws..];
        let condition = strip_condition_comment(raw_condition).trim_end();
        let start = rest_offset + keyword.len() + condition_ws;
        return Some(if is_if {
            ParsedDirective::If { condition, start }
        } else {
            ParsedDirective::Elif { condition, start }
        });
    }
    match rest.trim() {
        "else" => Some(ParsedDirective::Else),
        "endif" => Some(ParsedDirective::Endif),
        _ => None,
    }
}

fn strip_condition_comment(condition: &str) -> &str {
    let line_comment = condition.find("//");
    let block_comment = condition.find("/*");
    match (line_comment, block_comment) {
        (Some(a), Some(b)) => &condition[..a.min(b)],
        (Some(index), None) | (None, Some(index)) => &condition[..index],
        (None, None) => condition,
    }
}

fn condition_occurrences(condition: &str, absolute_start: usize) -> Vec<DefineOccurrence> {
    let mut occurrences = Vec::new();
    let mut start = None;
    for (index, character) in condition.char_indices() {
        if character.is_alphanumeric() || character == '_' {
            start.get_or_insert(index);
        } else if let Some(begin) = start.take() {
            occurrences.push(DefineOccurrence {
                name: condition[begin..index].to_string(),
                start: absolute_start + begin,
                end: absolute_start + index,
            });
        }
    }
    if let Some(begin) = start {
        occurrences.push(DefineOccurrence {
            name: condition[begin..].to_string(),
            start: absolute_start + begin,
            end: absolute_start + condition.len(),
        });
    }
    occurrences
}

fn simple_true_state(condition: &str) -> Option<(String, bool)> {
    match tokenize_condition(condition).as_slice() {
        [CondToken::Define(name)] => Some((name.clone(), true)),
        [CondToken::Not, CondToken::Define(name)] => Some((name.clone(), false)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use tower_lsp::lsp_types::{
        CodeActionContext, CodeActionKind, CodeActionOrCommand, NumberOrString, Position, Range,
        Url,
    };

    use super::*;
    use crate::analysis::DocumentAnalysis;
    use crate::config::InactiveLevel;
    use crate::server::code_actions::fix::WorkspaceFileResolver;
    use crate::server::diagnostics::inactive_region_diagnostics;
    use crate::symbols::SymbolTable;

    fn actions(
        source: &str,
        defines: &[&str],
        range: Range,
        with_diagnostics: bool,
    ) -> Vec<CodeAction> {
        let defines: HashSet<String> = defines.iter().map(|name| (*name).to_string()).collect();
        let analysis = DocumentAnalysis::analyze(source, &defines);
        let diagnostics = if with_diagnostics {
            inactive_region_diagnostics(source, &analysis, InactiveLevel::Hint)
        } else {
            Vec::new()
        };
        let context = CodeActionContext {
            diagnostics,
            only: Some(vec![CodeActionKind::QUICKFIX]),
            ..Default::default()
        };
        actions_with_context(source, defines, analysis, range, context)
    }

    fn actions_with_context(
        _source: &str,
        _defines: HashSet<String>,
        analysis: DocumentAnalysis,
        range: Range,
        context: CodeActionContext,
    ) -> Vec<CodeAction> {
        let workspace = SymbolTable::new();
        let uri = Url::parse("file:///tmp/B170.as").unwrap();
        let files = [(uri.clone(), &analysis)];
        crate::server::code_actions::code_actions(&CodeActionRequest {
            uri: &uri,
            analysis: &analysis,
            defines: &_defines,
            range,
            context: &context,
            workspace: &workspace,
            type_index: None,
            files: WorkspaceFileResolver::new(&files),
            lazy_edits: true,
        })
        .into_iter()
        .filter_map(|action| match action {
            CodeActionOrCommand::CodeAction(action) => Some(action),
            CodeActionOrCommand::Command(_) => None,
        })
        .collect()
    }

    fn actions_for_first_inactive(source: &str, defines: &[&str]) -> Vec<CodeAction> {
        let define_set: HashSet<String> = defines.iter().map(|name| (*name).to_string()).collect();
        let analysis = DocumentAnalysis::analyze(source, &define_set);
        let diagnostics = inactive_region_diagnostics(source, &analysis, InactiveLevel::Hint);
        let range = diagnostics.first().expect("inactive diagnostic").range;
        actions_with_context(
            source,
            define_set,
            analysis,
            range,
            CodeActionContext {
                diagnostics,
                only: Some(vec![CodeActionKind::QUICKFIX]),
                ..Default::default()
            },
        )
    }

    fn find<'a>(actions: &'a [CodeAction], title: &str) -> &'a CodeAction {
        actions
            .iter()
            .find(|action| action.title == title)
            .unwrap_or_else(|| {
                panic!(
                    "missing {title:?} in {:?}",
                    actions
                        .iter()
                        .map(|action| &action.title)
                        .collect::<Vec<_>>()
                )
            })
    }

    fn whole(source: &str) -> Range {
        Range::new(
            Position::new(0, 0),
            Position::new(source.lines().count() as u32, 0),
        )
    }

    #[test]
    fn inactive_simple_if_offers_preferred_enable_command() {
        let source = "#if DEV\nvoid DebugOnly() {}\n#endif\n";
        let actions = actions(source, &[], whole(source), true);
        let action = actions
            .iter()
            .find(|a| a.title == "Enable define DEV")
            .unwrap();
        assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
        assert_eq!(action.is_preferred, Some(true));
        assert!(action.edit.is_none());
        assert_eq!(
            action.command.as_ref().unwrap().command,
            "openplanetLsp.defines.set"
        );
        assert_eq!(
            action.command.as_ref().unwrap().arguments,
            Some(vec![serde_json::json!({"name": "DEV", "on": true})])
        );
        assert_eq!(
            action.diagnostics.as_ref().unwrap()[0].code,
            Some(NumberOrString::String("inactive-region".into()))
        );
    }

    #[test]
    fn live_directive_identifier_offers_literal_disable() {
        let source = "#if DEV\nvoid DebugOnly() {}\n#endif\n";
        let actions = actions(
            source,
            &["DEV"],
            Range::new(Position::new(0, 4), Position::new(0, 7)),
            false,
        );
        assert!(actions.iter().any(|a| a.title == "Disable define DEV"));
    }

    #[test]
    fn negated_if_and_simple_else_offer_the_sufficient_literal_toggle() {
        let negated = "#if !DEV\nvoid ReleaseOnly() {}\n#endif\n";
        let actions = actions_for_first_inactive(negated, &["DEV"]);
        assert_eq!(
            find(&actions, "Disable define DEV").is_preferred,
            Some(true)
        );

        let else_arm = "#if DEV\nvoid DebugOnly() {}\n#else\nvoid ReleaseOnly() {}\n#endif\n";
        let actions = actions_for_first_inactive(else_arm, &["DEV"]);
        assert_eq!(
            find(&actions, "Disable define DEV").is_preferred,
            Some(true)
        );

        let negated_else = "#if !DEV\nvoid ReleaseOnly() {}\n#else\nvoid DebugOnly() {}\n#endif\n";
        let actions = actions_for_first_inactive(negated_else, &[]);
        assert_eq!(find(&actions, "Enable define DEV").is_preferred, Some(true));
    }

    #[test]
    fn elif_is_preferred_only_when_the_toggle_really_selects_that_arm() {
        let source =
            "#if A\nvoid AOnly() {}\n#elif B\nvoid BOnly() {}\n#else\nvoid Neither() {}\n#endif\n";
        for (defines, expected_preferred) in [(&[][..], Some(true)), (&["A"][..], None)] {
            let define_set: HashSet<String> =
                defines.iter().map(|name| (*name).to_string()).collect();
            let analysis = DocumentAnalysis::analyze(source, &define_set);
            let diagnostics = inactive_region_diagnostics(source, &analysis, InactiveLevel::Hint);
            let b_diag = diagnostics
                .iter()
                .find(|diagnostic| diagnostic.range.start.line == 3)
                .expect("B arm diagnostic")
                .clone();
            let actions = actions_with_context(
                source,
                define_set,
                analysis,
                b_diag.range,
                CodeActionContext {
                    diagnostics: vec![b_diag],
                    only: Some(vec![CodeActionKind::QUICKFIX]),
                    ..Default::default()
                },
            );
            assert_eq!(
                find(&actions, "Enable define B").is_preferred,
                expected_preferred
            );
        }
    }

    #[test]
    fn compound_conditions_offer_each_literal_state_without_preference() {
        for condition in ["A && B", "(A || B)"] {
            let source = format!("#if {condition}\nvoid Conditional() {{}}\n#endif\n");
            let actions = actions_for_first_inactive(&source, &[]);
            for title in ["Enable define A", "Enable define B"] {
                let action = find(&actions, title);
                assert_eq!(action.is_preferred, None, "{condition}: {title}");
                assert!(action.diagnostics.is_some(), "{condition}: {title}");
            }
        }
    }

    #[test]
    fn nested_inactive_region_uses_only_the_nearest_controlling_arm() {
        let source = "#if OUTER\nvoid A() {}\n#if INNER\nvoid B() {}\n#endif\n#endif\n";
        let actions = actions_for_first_inactive(source, &["INNER"]);
        assert_eq!(
            find(&actions, "Enable define OUTER").is_preferred,
            Some(true)
        );
        assert!(
            actions
                .iter()
                .all(|action| action.title != "Disable define INNER"),
            "nested directive inside a broad body diagnostic is not the controller"
        );
    }

    #[test]
    fn directive_tokens_preserve_case_utf16_and_ignore_comments() {
        let source = "// 🚀\r\n#if dev || DEV /* COMMENT_DEFINE */\r\nvoid F() {}\r\n#endif\r\n";
        let dev = actions(
            source,
            &[],
            Range::new(Position::new(1, 4), Position::new(1, 7)),
            false,
        );
        assert!(dev.iter().any(|action| action.title == "Enable define dev"));
        assert!(dev
            .iter()
            .all(|action| !action.title.contains("COMMENT_DEFINE")));

        let upper = actions(
            source,
            &["DEV"],
            Range::new(Position::new(1, 11), Position::new(1, 14)),
            false,
        );
        assert!(upper
            .iter()
            .any(|action| action.title == "Disable define DEV"));
        assert!(upper
            .iter()
            .all(|action| action.title != "Enable define dev"));
    }

    #[test]
    fn repeated_define_occurrences_deduplicate_the_body_action() {
        let source = "#if DEV && DEV\nvoid F() {}\n#endif\n";
        let actions = actions_for_first_inactive(source, &[]);
        assert_eq!(
            actions
                .iter()
                .filter(|action| action.title == "Enable define DEV")
                .count(),
            1
        );
    }

    #[test]
    fn foreign_stale_and_non_quickfix_requests_are_safe() {
        let source = "#if DEV\nvoid F() {}\n#endif\n";
        let define_set = HashSet::new();
        let analysis = DocumentAnalysis::analyze(source, &define_set);
        let current = inactive_region_diagnostics(source, &analysis, InactiveLevel::Hint)
            .into_iter()
            .next()
            .unwrap();
        let mut foreign = current.clone();
        foreign.source = Some("another-server".into());
        let mut stale = current.clone();
        stale.range.start.line += 1;
        let context = CodeActionContext {
            diagnostics: vec![foreign, stale],
            only: Some(vec![CodeActionKind::REFACTOR]),
            ..Default::default()
        };
        let actions = actions_with_context(source, define_set, analysis, current.range, context);
        assert!(actions.iter().all(|action| {
            action
                .command
                .as_ref()
                .map(|command| command.command.as_str())
                != Some(SET_DEFINE_COMMAND)
        }));
    }

    #[test]
    fn command_only_action_never_mutates_source_when_the_client_has_no_handler() {
        let source = "#if DEV\nvoid F() {}\n#endif\n";
        let actions = actions_for_first_inactive(source, &[]);
        let action = find(&actions, "Enable define DEV");
        assert!(action.edit.is_none());
        assert!(action.data.is_none());
        assert!(action.command.is_some());
    }
}
