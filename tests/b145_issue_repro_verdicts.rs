//! B145: the game compiler's verdict on every issue-repro fixture, committed.
//!
//! `docs/probes/b145-issue-repro-verdicts.tsv` records, per fixture, what the
//! ENGINE said about it (B118's four-valued build status, plus the engine's own
//! message text) and what THIS LSP said about the same bytes. Both sides are
//! machine-measured; the only hand-authored field is `divergence`.
//!
//! Before it existed, 64 fixture directories carried zero committed engine
//! evidence: `scripts/issue_repro_game.py` printed the engine's verdict, a
//! human read it, and the message was hand-typed into a doc comment beside a
//! Rust assertion. The fixture README's own rule -- *a fixture's LSP assertion
//! is only meaningful against the game compiler's verdict on the same bytes* --
//! therefore held at the moment someone ran the probe and not afterwards.
//!
//! These tests are the "and afterwards" half. They cannot re-run the engine, so
//! they do the two things that do not need it:
//!
//! * re-measure the LSP side and require it to match what the corpus recorded,
//!   so a checker change that moves a fixture shows up HERE rather than being
//!   silently absorbed;
//! * require every engine/LSP disagreement to carry a written reason, and
//!   require every declared reason to correspond to a real disagreement -- so
//!   neither an unexplained gap nor a stale note about a fixed one can sit in
//!   the artifact quietly.
//!
//! The corpus is registered in `docs/probes/MANIFEST.tsv`, so B129's
//! registration test already fails if it is deleted or renamed.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn corpus_path() -> PathBuf {
    repo_root().join("docs/probes/b145-issue-repro-verdicts.tsv")
}

#[derive(Debug, Clone)]
struct Row {
    fixture: String,
    verdict: String,
    build_status: String,
    engine_compiled: String,
    engine_message: String,
    lsp_today: String,
    divergence_for: String,
    engine_version: String,
    probe_date: String,
    divergence: String,
}

impl Row {
    /// Did the ENGINE say anything at all about this code?
    ///
    /// Not the same question as `build_status`, which is about ERRORS. The
    /// engine warns as well as rejects, and this LSP is a linter as well as a
    /// compiler front end, so comparing error-polarity alone reports a fixture
    /// the engine warned about and the LSP flagged as a disagreement. It is
    /// not one.
    fn engine_spoke(&self) -> bool {
        self.verdict != "ACCEPT"
    }

    fn lsp_spoke(&self) -> Option<bool> {
        match self.lsp_today.as_str() {
            "UNMEASURED" => None,
            "CLEAN" => Some(false),
            _ => Some(true),
        }
    }

    fn engine_was_measured(&self) -> bool {
        self.build_status == "CLEAN" || self.build_status == "REJECTED"
    }

    fn declares_divergence(&self) -> bool {
        !self.divergence.trim().is_empty() && self.divergence.trim() != "-"
    }

    /// The measurement a divergence note was written against.
    fn state(&self) -> String {
        format!("{}|{}", self.verdict, self.lsp_today)
    }

    /// Did the engine reach a single function body?
    ///
    /// Read ONLY beside a REJECTED row. A clean compile usually logs no
    /// per-function lines at all, so a clean `0` means nothing.
    fn reached_no_body(&self) -> bool {
        self.engine_compiled.trim() == "0"
    }
}

fn parse_corpus() -> Vec<Row> {
    let path = corpus_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let mut header: Option<Vec<String>> = None;
    let mut rows = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let cells: Vec<String> = line.split('\t').map(|s| s.to_string()).collect();
        let Some(cols) = &header else {
            header = Some(cells);
            continue;
        };
        assert_eq!(
            cells.len(),
            cols.len(),
            "row has {} cells but the header declares {}; a tab inside a field \
             shifts every column after it: {line}",
            cells.len(),
            cols.len()
        );
        let get = |name: &str| -> String {
            let idx = cols
                .iter()
                .position(|c| c == name)
                .unwrap_or_else(|| panic!("corpus has no `{name}` column"));
            cells[idx].clone()
        };
        rows.push(Row {
            fixture: get("fixture"),
            verdict: get("verdict"),
            build_status: get("build_status"),
            engine_compiled: get("engine_compiled"),
            engine_message: get("engine_message"),
            lsp_today: get("lsp_today"),
            divergence_for: get("divergence_for"),
            engine_version: get("engine_version"),
            probe_date: get("probe_date"),
            divergence: get("divergence"),
        });
    }
    rows
}

/// Every summary shape the CLI can print. B145 measured a second one the
/// probe's parser did not know (B090 gave the zero case its own wording), and
/// the consequence was three fixtures recorded as "unmeasured" that had in
/// fact been measured and were clean. `None` here means the same thing it
/// means there: I could not tell, which is never the same as zero.
fn parse_lsp_count(out: &str) -> Option<usize> {
    for line in out.lines() {
        if let Some(idx) = line.find(" diagnostic") {
            let head: String = line[..idx]
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !head.is_empty() {
                return head.chars().rev().collect::<String>().parse().ok();
            }
        }
        if line.contains("0 errors/warnings") {
            return Some(0);
        }
    }
    None
}

fn run_lsp(fixture: &str) -> Option<usize> {
    let fx = repo_root()
        .join("tests/fixtures/issue-repros")
        .join(fixture);
    let typedb = repo_root().join("tests/fixtures/typedb");
    let out = Command::new(env!("CARGO_BIN_EXE_openplanet-lsp"))
        .env_remove("FORCE_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .arg("check")
        .arg("--typedb-dir")
        .arg(&typedb)
        .arg(&fx)
        .output()
        .expect("failed to run the LSP CLI");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    parse_lsp_count(&text)
}

fn render(count: Option<usize>) -> String {
    match count {
        None => "UNMEASURED".to_string(),
        Some(0) => "CLEAN".to_string(),
        Some(n) => format!("DIAGNOSES({n})"),
    }
}

#[test]
fn the_corpus_covers_every_fixture_that_can_be_probed() {
    let rows = parse_corpus();
    assert!(
        rows.len() >= 60,
        "the corpus has only {} rows; it is supposed to cover every fixture \
         under tests/fixtures/issue-repros/",
        rows.len()
    );
    let recorded: BTreeSet<String> = rows.iter().map(|r| r.fixture.clone()).collect();
    assert_eq!(
        recorded.len(),
        rows.len(),
        "a fixture appears twice in the corpus, so one measurement silently \
         replaced another"
    );
    let dir = repo_root().join("tests/fixtures/issue-repros");
    let mut on_disk = BTreeSet::new();
    for entry in std::fs::read_dir(&dir).expect("issue-repros dir") {
        let entry = entry.expect("dir entry");
        if !entry.path().is_dir() {
            continue;
        }
        // A directory with no info.toml is not a plugin and cannot be staged
        // into the game, so it is not probeable and is not expected here.
        if entry.path().join("info.toml").exists() {
            on_disk.insert(entry.file_name().to_string_lossy().to_string());
        }
    }
    let missing: Vec<_> = on_disk.difference(&recorded).cloned().collect();
    assert!(
        missing.is_empty(),
        "these fixtures exist but carry NO committed engine verdict, which is \
         the condition B145 exists to end -- re-run the generator named in \
         docs/probes/MANIFEST.tsv: {missing:?}"
    );
    let stale: Vec<_> = recorded.difference(&on_disk).cloned().collect();
    assert!(
        stale.is_empty(),
        "the corpus records fixtures that no longer exist: {stale:?}"
    );
}

#[test]
fn every_row_carries_the_engine_message_and_a_version_stamp() {
    let rows = parse_corpus();
    let mut checked = 0;
    for row in &rows {
        assert!(
            !row.engine_message.trim().is_empty(),
            "{}: the engine message is EMPTY. The message text is the part \
             hand-transcription loses and the part that moves between engine \
             versions -- a row without it records a polarity and no evidence",
            row.fixture
        );
        // Only where there IS an engine message. A NOT-RUN row's message is
        // the probe's own reason for refusing, and for these two fixtures that
        // reason is precisely "the id derived from your info.toml name has a
        // slash in it" -- quoting the offending id is the explanation, not a
        // leak of it.
        if row.engine_was_measured() {
            assert!(
                !row.engine_message.contains("lsp-issue-repro-"),
                "{}: the engine message still carries the probe's staging \
                 folder id, which is an artefact of the probe and not of the \
                 fixture: {}",
                row.fixture,
                row.engine_message
            );
        }
        assert!(
            row.engine_version == "UNKNOWN" || row.engine_version.starts_with("1.29."),
            "{}: engine_version {:?} is not a recognised Openplanet \
             generation; this repo already holds claims from three of them \
             and an unstamped one cannot be diffed against a later build",
            row.fixture,
            row.engine_version
        );
        assert!(
            row.probe_date.len() == 10 && row.probe_date.starts_with("20"),
            "{}: probe_date {:?} is not a date",
            row.fixture,
            row.probe_date
        );
        checked += 1;
    }
    assert_eq!(checked, rows.len());
}

#[test]
fn a_verdict_the_probe_could_not_take_is_not_recorded_as_evidence() {
    let rows = parse_corpus();
    for row in &rows {
        if row.engine_was_measured() {
            continue;
        }
        // NOT-RUN and BUILD-ERR are the probe saying it could not look. They
        // are allowed in the artifact -- an artifact that cannot be produced
        // is a finding and belongs on the record -- but they must be VISIBLE
        // as such, never dressed as an answer.
        assert_eq!(
            row.verdict, "UNKNOWN",
            "{}: build_status is {} (the probe could not measure it) but the \
             verdict column claims {:?}. A silence must not be recorded as an \
             answer -- that is how 64 fixtures came to carry no evidence while \
             looking fine",
            row.fixture, row.build_status, row.verdict
        );
        assert!(
            row.declares_divergence(),
            "{}: the probe could not measure this fixture ({}), and no reason \
             is recorded. Say why it cannot be probed -- an artifact that \
             cannot be regenerated is a finding, and B129's manifest reports \
             one on every run rather than omitting it",
            row.fixture,
            row.build_status
        );
    }
}

#[test]
fn engine_and_lsp_agree_or_the_disagreement_is_written_down() {
    let rows = parse_corpus();
    let mut problems: Vec<String> = Vec::new();
    let mut compared = 0;
    for row in &rows {
        if !row.engine_was_measured() {
            continue;
        }
        let Some(lsp_spoke) = row.lsp_spoke() else {
            problems.push(format!(
                "{}: lsp_today is UNMEASURED, so this row cannot say whether \
                 the two sides agree",
                row.fixture
            ));
            continue;
        };
        compared += 1;
        let agrees = row.engine_spoke() == lsp_spoke;
        // A declaration-time rejection REQUIRES a note even though it agrees
        // on polarity — see `a_declaration_time_rejection_says_so_rather_than
        // _passing_as_agreement`. Without this the two rules contradict each
        // other and those rows are unsatisfiable in both directions, which is
        // the shape that makes a gate get switched off.
        let needs_note =
            agrees && row.build_status == "REJECTED" && row.reached_no_body() && lsp_spoke;
        if agrees && !needs_note && row.declares_divergence() {
            problems.push(format!(
                "{}: a divergence is declared ({:?}) but the engine ({}) and \
                 this LSP ({}) now AGREE. A stale note is how a FIXED gap \
                 keeps being described as an open one -- delete it",
                row.fixture, row.divergence, row.verdict, row.lsp_today
            ));
        } else if !agrees && !row.declares_divergence() {
            problems.push(format!(
                "{}: the engine says {} ({:?}) and this LSP says {}, with no \
                 reason recorded",
                row.fixture, row.verdict, row.engine_message, row.lsp_today
            ));
        }
    }
    assert!(
        compared >= 50,
        "only {compared} rows were actually compared; a check that skips its \
         way to green is the defect, not the gate"
    );
    assert!(
        problems.is_empty(),
        "{} row(s) where the engine evidence and this LSP do not line up:\n  {}",
        problems.len(),
        problems.join("\n  ")
    );
}

#[test]
fn the_recorded_lsp_answer_still_matches_what_the_cli_says_today() {
    let rows = parse_corpus();
    let mut drifted: Vec<String> = Vec::new();
    let mut measured = 0;
    for row in &rows {
        let now = render(run_lsp(&row.fixture));
        if now != "UNMEASURED" {
            measured += 1;
        }
        if now != row.lsp_today {
            drifted.push(format!(
                "{}: corpus records {}, the CLI says {} today",
                row.fixture, row.lsp_today, now
            ));
        }
    }
    assert!(
        measured >= 50,
        "only {measured} fixtures produced a readable diagnostic count; the \
         summary-line parser has probably drifted from the CLI's output, \
         which is exactly the defect that recorded three measured fixtures as \
         unmeasured"
    );
    assert!(
        drifted.is_empty(),
        "the checker's answer moved on {} fixture(s) since the corpus was \
         recorded. That is not automatically a bug -- but the ENGINE evidence \
         beside it was measured against the old answer, so re-run the \
         generator named in docs/probes/MANIFEST.tsv rather than editing the \
         numbers by hand:\n  {}",
        drifted.len(),
        drifted.join("\n  ")
    );
}

/// B157/B168: a JUSTIFIED row whose free-text reason IS the false claim makes
/// an audit defend the defect. Prose cannot be verified. It CAN be made to
/// expire when the thing it describes moves.
///
/// This artifact needed that within one regeneration.
/// `b102-array-sort-correct-callbacks` carried the note *"the engine rejects
/// all three 'correct' comparators and the LSP is silent on all three"*; after
/// B152 the same row measured engine ACCEPT and LSP DIAGNOSES(3) — both halves
/// inverted — and the note carried across unchanged with nothing firing, since
/// the stale-note rule only triggers when the two sides come to AGREE.
#[test]
fn every_divergence_note_is_stamped_with_the_measurement_it_describes() {
    let rows = parse_corpus();
    let mut stale = Vec::new();
    let mut stamped = 0;
    for row in &rows {
        if !row.declares_divergence() {
            assert_eq!(
                row.divergence_for.trim(),
                "-",
                "{}: carries a stamp with no note behind it",
                row.fixture
            );
            continue;
        }
        stamped += 1;
        if row.divergence_for.trim() != row.state() {
            stale.push(format!(
                "{}: note stamped for {:?}, row now measures {:?}",
                row.fixture,
                row.divergence_for,
                row.state()
            ));
        }
    }
    assert!(
        stamped >= 5,
        "only {stamped} rows carry a note; this check would pass by having \
         nothing to check"
    );
    assert!(
        stale.is_empty(),
        "{} divergence note(s) describe a measurement that no longer holds. A \
         reason written against a different state is not evidence about this \
         one, however plausible it still reads — rewrite it against what is \
         measured now, or delete it:\n  {}",
        stale.len(),
        stale.join("\n  ")
    );
}

/// Agreeing on polarity is not answering the same question. When the engine
/// rejected a unit at DECLARATION time it compiled no function bodies, so its
/// verdict covers the declarations and nothing else — while the LSP's
/// diagnostics may be entirely about call sites the engine never reached.
///
/// Measured on two fixtures: `b102-array-sort-mismatched-callbacks` (fixed by
/// B152) and `b060-property-getter-value-arg`, whose funcdefs collide with
/// Openplanet's own types so the engine never adjudicates the property-read
/// question the fixture exists to ask.
#[test]
fn a_declaration_time_rejection_says_so_rather_than_passing_as_agreement() {
    let rows = parse_corpus();
    let mut unexplained = Vec::new();
    let mut checked = 0;
    for row in &rows {
        if row.build_status != "REJECTED" || !row.reached_no_body() {
            continue;
        }
        let Some(true) = row.lsp_spoke() else {
            continue;
        };
        checked += 1;
        if !row.declares_divergence() {
            unexplained.push(format!(
                "{}: engine rejected at declaration time (0 bodies compiled), \
                 LSP reports {}",
                row.fixture, row.lsp_today
            ));
        }
    }
    assert!(
        checked > 0,
        "no row is in this state, so this check measured nothing — if that is \
         genuinely true the corpus has changed shape and this test needs \
         rewriting rather than deleting"
    );
    assert!(
        unexplained.is_empty(),
        "{} row(s) where the engine never reached a call site and the LSP \
         reports diagnostics about one, with no reason recorded. They agree on \
         polarity, which is not evidence they answered the same question:\n  {}",
        unexplained.len(),
        unexplained.join("\n  ")
    );
}
