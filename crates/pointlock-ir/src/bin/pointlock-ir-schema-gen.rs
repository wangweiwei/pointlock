//! Writes the generated FlowIR JSON Schema (Draft 2020-12) to
//! `schema/generated/flow-ir.schema.json`.
//!
//! Thin wrapper around [`pointlock_ir::schema_gen::flow_ir_schema`], where
//! the actual generation and post-processing live (so the
//! behavioral-equivalence tests can call the same logic in-process).
//!
//! The output must be diffed against the acceptance baseline
//! `schema/flow-ir.v0.1.schema.json`; the equivalence judge is behavioral
//! (golden fixture accept/reject parity), not textual.

use std::fs;
use std::path::Path;

fn main() {
    let doc = pointlock_ir::schema_gen::flow_ir_schema();

    let out_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/generated/flow-ir.schema.json");
    let out_dir = out_path.parent().expect("output path has a parent");
    fs::create_dir_all(out_dir).expect("create schema/generated/");

    let mut rendered = serde_json::to_string_pretty(&doc).expect("schema renders");
    rendered.push('\n');
    fs::write(&out_path, rendered).expect("write generated schema");

    // Canonicalize the path for the log line if possible.
    let display = out_path.canonicalize().unwrap_or(out_path);
    println!("wrote {}", display.display());
}
