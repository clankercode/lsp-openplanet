//! B129: the gold manifest is enforced against the filesystem, not trusted.
//!
//! `docs/probes/MANIFEST.tsv` names every engine-derived artifact in this
//! repo. A manifest is only worth the enforcement behind it: a hand-copied
//! list silently stops covering what it does not mention, which is how B131's
//! struct audit came to cover 7 of 11 structs while passing. So these tests
//! PARSE `docs/probes/` and compare both directions —
//!
//! * a `.tsv` on disk that the manifest does not name FAILS, so a new corpus
//!   cannot dodge the manifest by not being added to it;
//! * a manifest row whose artifact is missing from disk FAILS, so a deleted
//!   corpus cannot leave a row claiming it is still regenerable.
//!
//! # Every test here asserts PRESENCE before it asserts a verdict
//!
//! B139: a gate written as "no failures" cannot distinguish "everything
//! passed" from "nothing ran". Each test below therefore asserts that it
//! actually found the artifacts it was asked to check before it reports on
//! them — a glob that matches nothing, a manifest that fails to parse, or a
//! wrong repo root would otherwise pass every one of these silently.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Data rows currently expected. Not a target to edit freely: raising it is
/// how a new corpus gets registered, and LOWERING it is how one gets deleted,
/// so both directions are a reviewed change.
const EXPECTED_MANIFEST_ROWS: usize = 17;

/// `.tsv` corpora expected in `docs/probes/`. Separate from the manifest row
/// count because the manifest also carries TYPEDB and PRIMARY rows that are
/// not files in that directory.
const EXPECTED_TSV_CORPORA: usize = 11;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn manifest_path() -> PathBuf {
    repo_root().join("docs/probes/MANIFEST.tsv")
}

#[derive(Debug, Clone)]
struct Row {
    artifact: String,
    kind: String,
    question: String,
    generator: String,
    engine_version: String,
    #[allow(dead_code)]
    probe_date: String,
    rows: String,
    diff: String,
    #[allow(dead_code)]
    dependents: String,
    volatility: String,
}

/// Parse the manifest, or panic with the reason. A silent empty parse is the
/// failure mode every test here is guarding against, so this never returns
/// an empty vector without saying so.
fn parse_manifest() -> Vec<Row> {
    let path = manifest_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the manifest at {}: {e}", path.display()));

    let mut lines = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty());

    let header = lines.next().expect("manifest has no header line");
    let header_cols: Vec<&str> = header.split('\t').collect();
    assert_eq!(
        header_cols,
        vec![
            "artifact",
            "kind",
            "question",
            "generator",
            "engine_version",
            "probe_date",
            "rows",
            "diff",
            "dependents",
            "volatility",
        ],
        "manifest column set changed; the runner and these tests read it positionally",
    );

    let parsed: Vec<Row> = lines
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            assert_eq!(
                c.len(),
                10,
                "manifest row has {} columns, expected 10: {l}",
                c.len()
            );
            Row {
                artifact: c[0].to_string(),
                kind: c[1].to_string(),
                question: c[2].to_string(),
                generator: c[3].to_string(),
                engine_version: c[4].to_string(),
                probe_date: c[5].to_string(),
                rows: c[6].to_string(),
                diff: c[7].to_string(),
                dependents: c[8].to_string(),
                volatility: c[9].to_string(),
            }
        })
        .collect();

    assert!(
        !parsed.is_empty(),
        "manifest parsed to ZERO rows — every check in this file would pass vacuously",
    );
    parsed
}

/// Every `.tsv` in `docs/probes/` except the manifest itself.
fn tsv_corpora_on_disk() -> BTreeSet<String> {
    let dir = repo_root().join("docs/probes");
    let entries =
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));

    let found: BTreeSet<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".tsv") && n != "MANIFEST.tsv")
        .map(|n| format!("docs/probes/{n}"))
        .collect();

    assert!(
        !found.is_empty(),
        "found ZERO .tsv corpora in {} — the directory scan is broken, not the corpora",
        dir.display(),
    );
    found
}

/// Data rows in a corpus: excludes the `#` preamble AND the column header.
fn data_rows(rel: &str) -> usize {
    let path = repo_root().join(rel);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read corpus {}: {e}", path.display()));
    let n = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .count();
    assert!(n > 0, "corpus {rel} has no non-comment lines at all");
    n - 1
}

#[test]
fn the_manifest_parses_and_holds_the_expected_number_of_rows() {
    let rows = parse_manifest();
    assert_eq!(
        rows.len(),
        EXPECTED_MANIFEST_ROWS,
        "manifest row count changed; update EXPECTED_MANIFEST_ROWS deliberately",
    );
}

#[test]
fn every_corpus_on_disk_is_registered_in_the_manifest() {
    let on_disk = tsv_corpora_on_disk();
    assert_eq!(
        on_disk.len(),
        EXPECTED_TSV_CORPORA,
        "corpus count on disk changed; a new corpus must also be added to the manifest",
    );

    let registered: BTreeSet<String> = parse_manifest().into_iter().map(|r| r.artifact).collect();

    let unregistered: Vec<&String> = on_disk.difference(&registered).collect();
    assert!(
        unregistered.is_empty(),
        "these corpora exist on disk but are NOT in docs/probes/MANIFEST.tsv, \
         so `just regen-golds` would never touch them: {unregistered:?}",
    );
}

#[test]
fn every_manifest_artifact_exists_on_disk() {
    let rows = parse_manifest();
    let mut checked = 0usize;
    let mut missing = Vec::new();
    for r in &rows {
        let p = repo_root().join(&r.artifact);
        checked += 1;
        if !p.exists() {
            missing.push(r.artifact.clone());
        }
    }
    assert_eq!(
        checked, EXPECTED_MANIFEST_ROWS,
        "checked {checked} artifacts, expected {EXPECTED_MANIFEST_ROWS}",
    );
    assert!(
        missing.is_empty(),
        "manifest names artifacts that do not exist: {missing:?}",
    );
}

#[test]
fn every_corpus_row_count_matches_the_file_on_disk() {
    let rows = parse_manifest();
    let mut compared = 0usize;
    let mut wrong = Vec::new();
    for r in &rows {
        if r.rows == "-" {
            continue;
        }
        let claimed: usize = r
            .rows
            .parse()
            .unwrap_or_else(|_| panic!("row count for {} is not a number: {}", r.artifact, r.rows));
        let actual = data_rows(&r.artifact);
        compared += 1;
        if claimed != actual {
            wrong.push(format!("{}: manifest {claimed}, disk {actual}", r.artifact));
        }
    }
    assert_eq!(
        compared, EXPECTED_TSV_CORPORA,
        "compared {compared} row counts, expected {EXPECTED_TSV_CORPORA} — \
         a corpus whose `rows` cell became `-` would silently stop being checked",
    );
    assert!(wrong.is_empty(), "row counts drifted: {wrong:?}");
}

#[test]
fn every_row_answers_a_question() {
    // The third column is the point of the manifest, not decoration: B118 and
    // B123 each produced a right output for a wrong stated reason, and a
    // manifest carrying only artifact+generator would regenerate both
    // correctly while preserving the false explanation forever.
    let rows = parse_manifest();
    let mut checked = 0usize;
    for r in &rows {
        checked += 1;
        assert!(
            r.question.len() > 30,
            "{}: the question column must state the question the artifact ANSWERS, got {:?}",
            r.artifact,
            r.question,
        );
        assert!(
            r.question.contains('?'),
            "{}: the question column is a QUESTION and must be phrased as one, got {:?}",
            r.artifact,
            r.question,
        );
    }
    assert_eq!(checked, EXPECTED_MANIFEST_ROWS);
}

#[test]
fn every_row_declares_a_known_kind_and_diff_mode() {
    let kinds: BTreeSet<&str> = ["CORPUS", "SIDECAR", "TYPEDB", "PRIMARY", "VERSIONS"]
        .into_iter()
        .collect();
    let diffs: BTreeSet<&str> = ["STRING", "STRUCTURED", "MANUAL"].into_iter().collect();
    let rows = parse_manifest();
    let mut checked = 0usize;
    for r in &rows {
        checked += 1;
        assert!(
            kinds.contains(r.kind.as_str()),
            "{}: unknown kind {:?}",
            r.artifact,
            r.kind
        );
        assert!(
            diffs.contains(r.diff.as_str()),
            "{}: unknown diff mode {:?}",
            r.artifact,
            r.diff
        );
    }
    assert_eq!(checked, EXPECTED_MANIFEST_ROWS);
}

#[test]
fn a_primary_row_is_manual_in_both_senses() {
    // PRIMARY is hand-transcribed engine ground truth in a source comment.
    // There is no output file to overwrite, so it can be neither generated
    // nor machine-diffed, and a row claiming otherwise would make the runner
    // report a verdict it never earned.
    let rows = parse_manifest();
    let primary: Vec<&Row> = rows.iter().filter(|r| r.kind == "PRIMARY").collect();
    assert!(
        !primary.is_empty(),
        "no PRIMARY rows — this test would pass vacuously",
    );
    for r in primary {
        assert_eq!(
            r.generator, "MANUAL",
            "{}: PRIMARY must not claim a generator",
            r.artifact
        );
        assert_eq!(
            r.diff, "MANUAL",
            "{}: PRIMARY cannot be machine-diffed",
            r.artifact
        );
    }
}

#[test]
fn artifacts_without_a_generator_are_declared_not_omitted() {
    // B148 EMPTIED THIS SET. b067 and b071 now have generators, so the
    // expected orphan list is empty — which is the deliberate shortening this
    // test's own failure message asked for.
    //
    // An empty expectation could be satisfied by an empty or unparsed
    // manifest, so the count of generator-BEARING rows is asserted alongside
    // it. Otherwise this test would keep passing after the thing it checks
    // stopped existing.
    // b067 and b071 are fully stamped engine gold with no script anywhere in
    // the repo. An artifact that cannot be regenerated is a FINDING, so the
    // manifest states it and the runner reports it on every run. This test
    // pins that they stay declared rather than quietly dropped.
    let rows = parse_manifest();
    let orphans: BTreeSet<String> = rows
        .iter()
        .filter(|r| r.generator == "NONE")
        .map(|r| r.artifact.clone())
        .collect();
    let expected: BTreeSet<String> = BTreeSet::new();
    let with_generator = rows
        .iter()
        .filter(|r| r.generator != "NONE" && r.generator != "MANUAL")
        .count();
    assert!(
        with_generator >= 10,
        "only {with_generator} artifacts declare a runnable generator; an empty \
         orphan set means nothing if the manifest lost its generators",
    );
    assert_eq!(
        orphans, expected,
        "an artifact lost its generator, or a new one landed without a script; \
         `generator=NONE` is legal but must be a deliberate, reported finding",
    );
}

#[test]
fn dependents_named_by_the_manifest_exist() {
    // The fan-out edge is only useful if it points at real files: B094's
    // "abstract does NOT exempt" is restated in four files in four phrasings,
    // and regenerating a corpus updates none of them.
    let rows = parse_manifest();
    let mut checked = 0usize;
    let mut missing = Vec::new();
    for r in &rows {
        if r.dependents == "-" {
            continue;
        }
        for dep in r.dependents.split(';') {
            checked += 1;
            if !repo_root().join(dep).exists() {
                missing.push(format!("{} -> {dep}", r.artifact));
            }
        }
    }
    assert!(
        checked >= 10,
        "only {checked} dependent edges checked — the fan-out column is the reason \
         in-source tables stay manual, so an empty column is a regression",
    );
    assert!(
        missing.is_empty(),
        "manifest points at files that do not exist: {missing:?}"
    );
}

#[test]
fn engine_versions_are_a_known_generation_or_explicitly_unknown() {
    // Three generations are cited concurrently in this repo. UNKNOWN is a
    // legitimate value and is NOT quietly treated as "current": the runner
    // refuses to diff an UNKNOWN artifact against a live run rather than
    // reporting a drift it cannot attribute.
    let known: BTreeSet<&str> = ["1.29.5", "1.29.9", "1.29.14", "UNKNOWN"]
        .into_iter()
        .collect();
    let rows = parse_manifest();
    let mut unknown_count = 0usize;
    for r in &rows {
        assert!(
            known.contains(r.engine_version.as_str()),
            "{}: unrecognised engine version {:?}",
            r.artifact,
            r.engine_version,
        );
        if r.engine_version == "UNKNOWN" {
            unknown_count += 1;
        }
    }
    assert!(
        unknown_count > 0,
        "no UNKNOWN versions — if every artifact became stamped that is real progress, \
         but it must be an observed change rather than this test silently passing",
    );
}

#[test]
fn the_manifest_covers_every_kind_it_defines() {
    // Guards against a manifest that degenerates to one kind: if TYPEDB or
    // PRIMARY rows vanished, the runner would still report "no drift".
    let rows = parse_manifest();
    let kinds: BTreeMap<String, usize> = rows.iter().fold(BTreeMap::new(), |mut m, r| {
        *m.entry(r.kind.clone()).or_insert(0) += 1;
        m
    });
    for k in ["CORPUS", "SIDECAR", "TYPEDB", "PRIMARY", "VERSIONS"] {
        assert!(
            kinds.contains_key(k),
            "manifest has no {k} row; kinds present: {kinds:?}"
        );
    }
}

/// B151: `volatility` says how to READ an artifact's drift, and a vocabulary
/// that is never validated is a free-text field.
#[test]
fn every_row_declares_a_known_volatility() {
    let known: BTreeSet<&str> = ["SURFACE", "SEMANTICS", "HYBRID"].into_iter().collect();
    let rows = parse_manifest();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for r in &rows {
        assert!(
            known.contains(r.volatility.as_str()),
            "{}: unknown volatility {:?}",
            r.artifact,
            r.volatility,
        );
        seen.insert(r.volatility.clone());
    }
    // A vocabulary with an unused value is an untested value. HYBRID exists
    // because b067 measures a language rule over 41 GAME CLASS NAMES, so one
    // label per file would have lied about it; if that row ever loses the
    // label, the distinction has quietly stopped being expressed.
    assert_eq!(
        seen.len(),
        3,
        "the manifest uses only {seen:?}; every volatility value must be exercised",
    );
}

/// B151: SURFACE means "retained per version", and a retention policy that
/// nothing checks is a comment. An artifact is retained only if a store holds a
/// copy of THAT artifact — not merely if a store exists, and not if some other
/// artifact's version happens to match.
#[test]
fn every_surface_artifact_is_archived_or_explicitly_unstamped() {
    let stores = ["tests/fixtures/typedb-versions", "docs/probes/versions"];
    let mut archived: BTreeSet<String> = BTreeSet::new();
    let mut store_versions = 0usize;
    for st in stores {
        let dir = repo_root().join(st);
        assert!(dir.is_dir(), "version store {st} is missing");
        for v in std::fs::read_dir(&dir).expect("read store").flatten() {
            if !v.path().is_dir() {
                continue;
            }
            store_versions += 1;
            for f in std::fs::read_dir(v.path()).expect("read version").flatten() {
                archived.insert(f.file_name().to_string_lossy().to_string());
            }
        }
    }
    assert!(
        store_versions >= 3,
        "only {store_versions} version directories across {stores:?}; SURFACE \
         retention would be declared and implemented by almost nothing",
    );

    let rows = parse_manifest();
    let mut checked = 0usize;
    let mut unretained = Vec::new();
    for r in &rows {
        if r.volatility != "SURFACE" || r.kind == "VERSIONS" {
            continue;
        }
        checked += 1;
        let src = repo_root().join(&r.artifact);
        let hit = if src.is_dir() {
            std::fs::read_dir(&src)
                .map(|d| {
                    d.flatten()
                        .any(|f| archived.contains(&f.file_name().to_string_lossy().to_string()))
                })
                .unwrap_or(false)
        } else {
            src.file_name()
                .map(|n| archived.contains(&n.to_string_lossy().to_string()))
                .unwrap_or(false)
        };
        // An UNSTAMPED artifact cannot be filed under a version without
        // asserting one nobody measured — the exact error B148 found in b067,
        // where a plausible stamp had been copied from a sibling corpus. It is
        // allowed to be unarchived, and `just regen-golds` reports it every run.
        if !hit && r.engine_version != "UNKNOWN" {
            unretained.push(r.artifact.clone());
        }
    }
    assert!(
        checked >= 3,
        "only {checked} SURFACE artifacts checked; this test would pass vacuously",
    );
    assert!(
        unretained.is_empty(),
        "these SURFACE artifacts are stamped but nothing archives them, so \
         regenerating one destroys the only copy: {unretained:?}",
    );
}

/// An archived copy is HISTORY, not a current artifact: registering one would
/// make `just regen-golds` try to regenerate a snapshot that is immutable by
/// design.
#[test]
fn archived_copies_are_not_registered_as_artifacts() {
    let registered: BTreeSet<String> = parse_manifest().into_iter().map(|r| r.artifact).collect();
    let mut found = 0usize;
    let store = repo_root().join("docs/probes/versions");
    for v in std::fs::read_dir(&store)
        .expect("read corpus store")
        .flatten()
    {
        if !v.path().is_dir() {
            continue;
        }
        for f in std::fs::read_dir(v.path()).expect("read version").flatten() {
            let rel = f
                .path()
                .strip_prefix(repo_root())
                .expect("under repo")
                .to_string_lossy()
                .to_string();
            found += 1;
            assert!(
                !registered.contains(&rel),
                "{rel} is an immutable archived copy and must not be a manifest row",
            );
        }
    }
    assert!(
        found > 0,
        "the corpus store holds nothing; this check is vacuous"
    );
}
