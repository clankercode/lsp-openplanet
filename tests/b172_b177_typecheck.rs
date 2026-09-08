use std::path::PathBuf;

use openplanet_lsp::analysis::DocumentAnalysis;
use openplanet_lsp::analysis_snapshot::AnalysisSnapshot;
use openplanet_lsp::config::LspConfig;
use openplanet_lsp::typecheck::{Checker, GlobalScope};
use openplanet_lsp::typedb::TypeIndex;

fn errors(source: &str) -> Vec<String> {
    let analysis = DocumentAnalysis::analyze(source, &LspConfig::default().defines);
    let mut symbols = openplanet_lsp::symbols::SymbolTable::new();
    let fid = symbols.allocate_file_id();
    symbols.set_file_symbols(
        fid,
        openplanet_lsp::symbols::SymbolTable::extract_symbols(
            fid,
            analysis.masked_source(),
            &analysis.file,
        ),
    );
    let scope = GlobalScope::new(&symbols, None);
    let mut checker = Checker::new(analysis.masked_source(), &scope);
    checker.check_file(&analysis.file);
    checker
        .diagnostics
        .into_iter()
        .filter(|d| d.severity() == openplanet_lsp::typecheck::TypeDiagnosticSeverity::Error)
        .map(|d| d.message())
        .collect()
}

fn errors_except_duplicates(source: &str) -> Vec<String> {
    errors(source)
        .into_iter()
        .filter(|message| !message.contains("same name and parameters already exists"))
        .collect()
}

fn errors_with_typedb(source: &str) -> Vec<String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/typedb");
    let typedb = TypeIndex::load(
        &root.join("OpenplanetCore.json"),
        &root.join("OpenplanetNext.json"),
    )
    .unwrap();
    let analysis = DocumentAnalysis::analyze(source, &LspConfig::default().defines);
    let mut symbols = openplanet_lsp::symbols::SymbolTable::new();
    let fid = symbols.allocate_file_id();
    symbols.set_file_symbols(
        fid,
        openplanet_lsp::symbols::SymbolTable::extract_symbols(
            fid,
            analysis.masked_source(),
            &analysis.file,
        ),
    );
    let scope = GlobalScope::new(&symbols, Some(&typedb));
    let mut checker = Checker::new(analysis.masked_source(), &scope);
    checker.check_file(&analysis.file);
    checker
        .diagnostics
        .into_iter()
        .filter(|d| d.severity() == openplanet_lsp::typecheck::TypeDiagnosticSeverity::Error)
        .map(|d| d.message())
        .collect()
}

fn workspace_errors(files: &[(PathBuf, &str)], file: usize) -> Vec<String> {
    let owned: Vec<_> = files
        .iter()
        .map(|(path, source)| (path.clone(), (*source).to_string()))
        .collect();
    let snapshot = AnalysisSnapshot::from_files(&owned, &LspConfig::default());
    let analysis = &snapshot.files()[file].analysis;
    let parsed = &analysis.file;
    let source = analysis.masked_source();
    let scope = GlobalScope::new(snapshot.symbols(), None);
    let mut checker = Checker::new(source, &scope);
    checker.check_file(parsed);
    checker
        .diagnostics
        .iter()
        .filter(|d| d.severity() == openplanet_lsp::typecheck::TypeDiagnosticSeverity::Error)
        .map(|d| d.message())
        .collect()
}

#[test]
fn b172_rejects_values_returned_from_void_functions_but_not_bare_return() {
    let messages = errors(
        "uint Helper() { return 1; }\n\
         void Bad() { return Helper(); }\n\
         void Good() { return; }\n\
         uint AlsoGood() { return Helper(); }",
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.contains("return type mismatch: function returns `void`"))
            .count(),
        1,
        "expected only the value-returning void function to fail: {messages:?}"
    );
}

#[test]
fn b173_only_dictionary_value_conversions_supported_by_the_typedb_are_accepted() {
    let messages = errors_with_typedb(
        "dictionary dict;\n\
         uint64 Bad() { return dict[\"x\"]; }\n\
         int64 GoodInt() { return dict[\"x\"]; }\n\
         double GoodDouble() { return dict[\"x\"]; }\n\
         void GoodOut() { uint64 value; dict.Get(\"x\", value); }",
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.contains("dictionaryValue") && message.contains("uint64"))
            .count(),
        1,
        "expected only the unsupported uint64 conversion to fail: {messages:?}"
    );
}

#[test]
fn b174_rejects_void_calls_only_when_a_value_is_required() {
    let messages = errors(
        "void VoidFn() {}\n\
         void Take(int value) {}\n\
         void Use() { VoidFn(); int value = VoidFn(); Take(VoidFn()); }",
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.contains("Data type can't be 'void'"))
            .count(),
        2,
        "the statement call is legal; initializer and argument are not: {messages:?}"
    );
}

#[test]
fn b175_flags_the_second_duplicate_global_function_across_files() {
    let files = [
        (
            PathBuf::from("first.as"),
            "uint16 GetOffset(const string &in a, const string &in b) { return 0; }",
        ),
        (
            PathBuf::from("second.as"),
            "uint16 GetOffset(const string &in a, const string &in b) { return 1; }\n\
             uint16 GetOffset(const string &in a) { return 1; }\n\
             namespace Other { uint16 GetOffset(const string &in a, const string &in b) { return 1; } }",
        ),
    ];
    let messages = workspace_errors(&files, 1);
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.contains("same name and parameters already exists"))
            .count(),
        1,
        "only the cross-file exact duplicate should fail: {messages:?}"
    );
}

#[test]
fn b176_rejects_non_convertible_handles_but_accepts_upcasts_and_explicit_casts() {
    let messages = errors_except_duplicates(
        "interface ICounter {}\n\
         class RawCounter {}\n\
         class Base {}\n\
         class Derived : Base {}\n\
         uint Count(RawCounter@ value) { return 0; }\n\
         void TakeBase(Base@ value) {}\n\
         void Use(ICounter@ ic, Derived@ derived) {\n\
             Count(ic);\n\
             TakeBase(derived);\n\
             Count(cast<RawCounter>(ic));\n\
         }",
    );
    assert_eq!(
        messages
            .iter()
            .filter(
                |message| message.contains("No matching signatures") && message.contains("Count")
            )
            .count(),
        1,
        "the incompatible handle is the only Count call that should fail: {messages:?}"
    );
}

#[test]
fn b177_rejects_primitive_inout_parameters_but_accepts_object_inout() {
    let messages = errors(
        "class Object {}\n\
         void Good(Object &inout value) {}\n\
         void BadUint(uint &inout value) {}\n\
         void BadInt64(int64 &inout value) {}\
\
         void BadString(string &inout value) {}",
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| message
                .contains("Only object types that support object handles can use &inout"))
            .count(),
        3,
        "all primitive inout forms should fail and the object form should pass: {messages:?}"
    );
}
