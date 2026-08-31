//! B134: the tripwire under a MEASURED-AND-REJECTED decision.
//!
//! B134 asked whether Core enum-value doc strings could drive deprecation
//! support. The answer is no, and the reason is structural rather than
//! statistical: a doc string is attached to a VALUE, and a deprecated flag and
//! its replacement are ALIASES SHARING ONE NUMERIC VALUE, so both names carry
//! the identical string. `UI::ChildFlags::Borders` is the name you are told to
//! adopt, and its own documentation says it is deprecated.
//!
//! That decision is written up on `SymbolHint::Relocation` and reproduced by
//! `scripts/measure_enum_doc_migrations.py`. This file is the part that keeps
//! it honest: **a rejection resting on a claim about the data rots exactly as
//! quietly as an acceptance does.** If a future dump ever separates the alias
//! docs, these tests fail and the decision is worth revisiting — which is the
//! only outcome that should reopen it.
//!
//! Deliberately NOT a matcher, and not a count of "how many look deprecated".
//! It pins the one property the rejection depends on.

use std::collections::BTreeMap;
use std::path::PathBuf;

fn dump(version: &str) -> serde_json::Value {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/typedb-versions")
        .join(version)
        .join("OpenplanetCore.json");
    let text =
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()));
    serde_json::from_str(&text).expect("Core dump is valid JSON")
}

/// Every `(qualified name, doc, numeric value)` for enum values carrying a doc.
fn documented_values(version: &str) -> Vec<(String, String, i64, String)> {
    let j = dump(version);
    let mut out = Vec::new();
    for e in j["enums"].as_array().expect("enums is an array") {
        let ns = e["ns"].as_str().unwrap_or("");
        let ename = e["name"].as_str().unwrap_or("");
        let Some(values) = e["values"].as_object() else {
            continue;
        };
        for (name, x) in values {
            let doc = x["d"].as_str().unwrap_or("").trim().to_string();
            if doc.is_empty() {
                continue;
            }
            let v = x["v"].as_i64().unwrap_or(i64::MIN);
            out.push((
                format!("{ns}::{ename}::{name}"),
                doc,
                v,
                format!("{ns}::{ename}"),
            ));
        }
    }
    assert!(
        !out.is_empty(),
        "{version}: no documented enum values found — the reader is broken, not the dump",
    );
    out
}

/// THE CLAIM THE REJECTION RESTS ON. A deprecated flag and its replacement
/// share one numeric value and one doc string, so nothing in the data says
/// which of the two names you should stop writing.
#[test]
fn a_deprecated_enum_value_and_its_replacement_are_indistinguishable() {
    // (enum, deprecated name, replacement name) — the four migration pairs.
    let pairs = [
        ("UI::ChildFlags", "Border", "Borders"),
        ("UI::Col", "NavHighlight", "NavCursor"),
        (
            "UI::TabBarFlags",
            "FittingPolicyResizeDown",
            "FittingPolicyShrink",
        ),
        ("UI::TreeNodeFlags", "SpanTextWidth", "SpanLabelWidth"),
    ];
    let mut checked = 0usize;
    for version in ["1.29.5", "1.29.14"] {
        let vals = documented_values(version);
        let by_name: BTreeMap<&str, (&str, i64)> = vals
            .iter()
            .map(|(k, d, v, _)| (k.as_str(), (d.as_str(), *v)))
            .collect();
        for (enum_name, dep, repl) in pairs {
            let dk = format!("{enum_name}::{dep}");
            let rk = format!("{enum_name}::{repl}");
            let (ddoc, dv) = *by_name
                .get(dk.as_str())
                .unwrap_or_else(|| panic!("{version}: {dk} is gone — the population changed"));
            let (rdoc, rv) = *by_name
                .get(rk.as_str())
                .unwrap_or_else(|| panic!("{version}: {rk} is gone — the population changed"));
            checked += 1;
            assert_eq!(
                dv, rv,
                "{version}: {dep} and {repl} no longer share a numeric value — the \
                 alias collision B134 was rejected over may be gone; re-measure",
            );
            assert_eq!(
                ddoc, rdoc,
                "{version}: {dep} and {repl} now carry DIFFERENT doc strings. B134 was \
                 rejected because they were identical and a matcher could not tell the \
                 deprecated name from its replacement. That may no longer hold — \
                 re-run `just enum-doc-migrations` and revisit the decision.",
            );
            // The prose names the REPLACEMENT, which is why reading it harder
            // cannot help: it says which name is correct, never which you wrote.
            assert!(
                ddoc.to_lowercase().contains(&repl.to_lowercase()),
                "{version}: {dk}'s doc no longer names {repl} as the replacement",
            );
        }
    }
    assert_eq!(
        checked,
        pairs.len() * 2,
        "checked {checked} pairs across two versions; this test would pass vacuously",
    );
}

/// The population is small and stable enough that a change to it is a signal.
/// Not a matcher: a count, so an engine bump that adds real deprecation data
/// is noticed rather than assumed away.
#[test]
fn the_documented_enum_value_population_is_what_was_measured() {
    for (version, expected) in [("1.29.5", 206usize), ("1.29.14", 208usize)] {
        let n = documented_values(version).len();
        assert_eq!(
            n, expected,
            "{version}: {n} documented enum values, measured {expected} in B134. \
             The population moved — re-run `just enum-doc-migrations` before \
             trusting the rejection.",
        );
    }
}

/// The one thing that DID drift, pinned because it is the reason a matcher is
/// unmaintainable: ordinary prose moved INTO the migration cue set on a symbol
/// that was never deprecated.
#[test]
fn ordinary_prose_drifted_into_the_migration_cue_set_between_versions() {
    let key = "UI::InputTextFlags::EnterReturnsTrue";
    let doc_for = |version: &str| -> String {
        documented_values(version)
            .into_iter()
            .find(|(k, ..)| k == key)
            .unwrap_or_else(|| panic!("{version}: {key} not found"))
            .1
    };
    let old = doc_for("1.29.5");
    let new = doc_for("1.29.14");
    assert!(
        !old.to_lowercase().contains("instead"),
        "1.29.5 {key} was expected to carry NO migration cue, got: {old}",
    );
    assert!(
        new.to_lowercase().contains("instead"),
        "1.29.14 {key} was expected to have GAINED a migration cue, got: {new}",
    );
    // The flag is not deprecated in either version. A cue-based matcher would
    // have started warning about it on an engine bump, for a prose edit.
    assert!(
        !new.to_lowercase().contains("deprecat"),
        "{key} now says it is deprecated; it was not, and that was the point",
    );
}
