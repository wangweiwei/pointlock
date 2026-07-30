//! Golden fixture corpus: behavioral equivalence between the acceptance
//! baseline schema (`schema/flow-ir.v0.1.schema.json`) and the schema
//! generated from the Rust DTOs, plus the serde round-trip guarantee.
//!
//! The equivalence judge (02 §1.1, 2026-07-17 ruling) is *behavioral*:
//! accept/reject parity over the full corpus — positives accepted by both
//! validators, negatives rejected by both. Any divergence fails the test
//! with a per-fixture report of which side disagreed and why; textual
//! schema diffs are for humans only and never a judgment criterion.
//!
//! The generated schema is produced in-process by
//! [`pointlock_ir::schema_gen::flow_ir_schema`] — no dependency on having
//! run the `pointlock-ir-schema-gen` bin beforehand.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use jsonschema::{Draft, Validator};
use serde_json::Value;

/// Absolute path of the repo-root `schema/` directory.
fn schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema")
}

fn load_json(path: &Path) -> Value {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn baseline_schema() -> Value {
    load_json(&schema_dir().join("flow-ir.v0.1.schema.json"))
}

fn compile(schema: &Value, name: &str) -> Validator {
    jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(schema)
        .unwrap_or_else(|e| panic!("{name} schema does not compile: {e}"))
}

/// Loads `schema/fixtures/<sub>/*.json`, sorted by name, excluding the
/// negative-side `manifest.json`.
fn fixtures(sub: &str) -> Vec<(String, Value)> {
    let dir = schema_dir().join("fixtures").join(sub);
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| {
            entry
                .expect("directory entry")
                .file_name()
                .into_string()
                .expect("utf-8 file name")
        })
        .filter(|name| name.ends_with(".json") && name != "manifest.json")
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no fixtures found in {}", dir.display());
    names
        .into_iter()
        .map(|name| {
            let doc = load_json(&dir.join(&name));
            (name, doc)
        })
        .collect()
}

/// Renders the first `limit` validation errors of `validator` against `doc`.
fn error_digest(validator: &Validator, doc: &Value, limit: usize) -> String {
    let mut out = String::new();
    for (i, error) in validator.iter_errors(doc).enumerate() {
        if i == limit {
            out.push_str("      …\n");
            break;
        }
        let _ = writeln!(out, "      at `{}`: {error}", error.instance_path);
    }
    if out.is_empty() {
        out.push_str("      (accepted)\n");
    }
    out
}

/// Lists the JSON Pointers at which two values differ (capped).
fn diff_pointers(a: &Value, b: &Value, at: &str, out: &mut Vec<String>) {
    if out.len() >= 16 {
        return;
    }
    match (a, b) {
        (Value::Object(ma), Value::Object(mb)) => {
            for (key, va) in ma {
                match mb.get(key) {
                    Some(vb) => diff_pointers(va, vb, &format!("{at}/{key}"), out),
                    None => out.push(format!("{at}/{key} (missing on the right)")),
                }
            }
            for key in mb.keys() {
                if !ma.contains_key(key) {
                    out.push(format!("{at}/{key} (missing on the left)"));
                }
            }
        }
        (Value::Array(xs), Value::Array(ys)) => {
            if xs.len() != ys.len() {
                out.push(format!("{at} (array length {} vs {})", xs.len(), ys.len()));
                return;
            }
            for (i, (x, y)) in xs.iter().zip(ys).enumerate() {
                diff_pointers(x, y, &format!("{at}/{i}"), out);
            }
        }
        _ if a != b => out.push(format!("{at} ({a} vs {b})")),
        _ => {}
    }
}

/// Both schemas must themselves be valid against the Draft 2020-12
/// meta-schema (the 02 §1.1 delivery statement, kept enforced).
#[test]
fn schemas_satisfy_the_2020_12_meta_schema() {
    let baseline = baseline_schema();
    jsonschema::meta::validate(&baseline)
        .unwrap_or_else(|e| panic!("baseline schema fails the meta-schema: {e}"));
    let generated = pointlock_ir::schema_gen::flow_ir_schema();
    jsonschema::meta::validate(&generated)
        .unwrap_or_else(|e| panic!("generated schema fails the meta-schema: {e}"));
}

/// The behavioral-equivalence acceptance test (02 §1.1): both validators
/// accept every positive fixture and reject every negative fixture. Any
/// accept/reject divergence — the only equivalence criterion — is reported
/// per fixture and fails the test.
#[test]
fn fixture_corpus_accept_reject_parity() {
    let baseline = compile(&baseline_schema(), "baseline");
    let generated = compile(&pointlock_ir::schema_gen::flow_ir_schema(), "generated");

    let mut divergences = String::new();
    let mut corpus_failures = String::new();

    for (name, doc) in fixtures("positive") {
        let b = baseline.is_valid(&doc);
        let g = generated.is_valid(&doc);
        if b != g {
            let _ = writeln!(
                divergences,
                "positive/{name}: baseline={}, generated={}",
                verdict(b),
                verdict(g)
            );
            let rejecting = if b { &generated } else { &baseline };
            let side = if b { "generated" } else { "baseline" };
            let _ = writeln!(divergences, "    {side} rejection detail:");
            divergences.push_str(&error_digest(rejecting, &doc, 5));
        } else if !b {
            let _ = writeln!(corpus_failures, "positive/{name}: rejected by BOTH schemas");
            let _ = writeln!(corpus_failures, "    baseline rejection detail:");
            corpus_failures.push_str(&error_digest(&baseline, &doc, 5));
        }
    }

    for (name, doc) in fixtures("negative") {
        let b = baseline.is_valid(&doc);
        let g = generated.is_valid(&doc);
        if b != g {
            let _ = writeln!(
                divergences,
                "negative/{name}: baseline={}, generated={}",
                verdict(b),
                verdict(g)
            );
            let rejecting = if b { &generated } else { &baseline };
            let side = if b { "generated" } else { "baseline" };
            let _ = writeln!(divergences, "    {side} rejection detail:");
            divergences.push_str(&error_digest(rejecting, &doc, 5));
        } else if b {
            let _ = writeln!(corpus_failures, "negative/{name}: accepted by BOTH schemas");
        }
    }

    let mut report = String::new();
    if !divergences.is_empty() {
        let _ = writeln!(
            report,
            "behavioral divergence between baseline and generated schema:\n{divergences}"
        );
    }
    if !corpus_failures.is_empty() {
        let _ = writeln!(
            report,
            "fixture corpus defects (schemas agree, corpus expectation violated):\n{corpus_failures}"
        );
    }
    assert!(report.is_empty(), "\n{report}");
}

fn verdict(accepted: bool) -> &'static str {
    if accepted { "accept" } else { "reject" }
}

/// Positive fixtures must survive the serde round-trip untouched:
/// JSON → `FlowIR` → JSON is a semantic identity (`serde_json::Value`
/// equality — key order is irrelevant, absence stays absence, no nulls
/// appear; 02 §2.4 single-representation rule).
#[test]
fn positive_fixtures_round_trip_through_flow_ir() {
    let mut report = String::new();
    for (name, doc) in fixtures("positive") {
        let flow: pointlock_ir::FlowIR = match serde_json::from_value(doc.clone()) {
            Ok(flow) => flow,
            Err(e) => {
                let _ = writeln!(report, "positive/{name}: does not deserialize: {e}");
                continue;
            }
        };
        let reserialized = serde_json::to_value(&flow).expect("FlowIR reserializes");
        if reserialized != doc {
            let mut pointers = Vec::new();
            diff_pointers(&doc, &reserialized, "", &mut pointers);
            let _ = writeln!(
                report,
                "positive/{name}: round-trip changed the document at:\n      {}",
                pointers.join("\n      ")
            );
        }
    }
    assert!(report.is_empty(), "round-trip failures:\n{report}");
}

/// Every negative fixture must have an expected-rejection entry in
/// `negative/manifest.json`, and every entry must point at an existing
/// fixture (the manifest replaces in-instance annotations, which the
/// closed baseline schema does not tolerate).
#[test]
fn negative_manifest_is_complete() {
    let manifest = load_json(&schema_dir().join("fixtures/negative/manifest.json"));
    let cases = manifest["cases"]
        .as_object()
        .expect("manifest.json has a `cases` object");

    let fixture_names: Vec<String> = fixtures("negative").into_iter().map(|(n, _)| n).collect();

    let mut report = String::new();
    for name in &fixture_names {
        match cases.get(name) {
            Some(Value::String(reason)) if !reason.is_empty() => {}
            Some(_) => {
                let _ = writeln!(report, "{name}: manifest entry is not a non-empty string");
            }
            None => {
                let _ = writeln!(report, "{name}: missing from manifest.json");
            }
        }
    }
    for name in cases.keys() {
        if !fixture_names.iter().any(|f| f == name) {
            let _ = writeln!(report, "{name}: manifest entry has no fixture file");
        }
    }
    assert!(report.is_empty(), "negative manifest incomplete:\n{report}");
}
