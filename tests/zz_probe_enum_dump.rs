//! Probe support: dump the loader's enum keys for `scripts/probe_enum_corpus.py`.
//!
//! `#[ignore]`d — it writes a file and is not a regression test. It exists so
//! the probe corpora in `docs/probes/` are built from the crate's OWN key
//! generation rather than a Python re-derivation of it. Re-deriving produced
//! two confidently wrong counts in this repo's history (B046's enum count,
//! B053's ambiguity estimate), which is why this is a Rust test and not a
//! parser in the script.
//!
//! ```text
//! PROBE_ENUM_DUMP=/tmp/enums.jsonl \
//!   cargo test -j 2 --test zz_probe_enum_dump -- --ignored
//! ```

use std::path::PathBuf;

use openplanet_lsp::typedb::TypeIndex;

#[test]
#[ignore = "probe support, not a regression test; writes PROBE_ENUM_DUMP"]
fn dump_enum_keys() {
    let dest = PathBuf::from(
        std::env::var("PROBE_ENUM_DUMP").expect("set PROBE_ENUM_DUMP to the output path"),
    );
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
    let index = TypeIndex::load(
        &fixtures.join("OpenplanetCore.json"),
        &fixtures.join("OpenplanetNext.json"),
    )
    .expect("typedb fixtures must load");

    let mut out = String::new();
    let mut keys: Vec<_> = index.enums_iter().collect();
    keys.sort_by(|a, b| a.0.cmp(b.0));
    for (qname, info) in keys {
        out.push_str(&format!(
            "{}\n",
            serde_json::json!({
                "key": qname,
                "name": info.name,
                "namespace": info.namespace,
                "anonymous": info.anonymous,
                "values": info
                    .values
                    .iter()
                    .map(|(v, i)| serde_json::json!([v, i]))
                    .collect::<Vec<_>>(),
            })
        ));
    }
    std::fs::write(&dest, out).expect("dump must be writable");
    eprintln!(
        "wrote {} enum keys to {}",
        index.enum_count(),
        dest.display()
    );
}
