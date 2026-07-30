//! Thin file-writing wrapper around
//! [`pointlock_store::projection::projection_schemas`] (the pattern of
//! `pointlock-ir-schema-gen`): writes the five projection schemas to
//! `schema/generated/projection/` at the repo root.

use std::fs;
use std::path::PathBuf;

fn main() {
    let out_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/generated/projection");
    fs::create_dir_all(&out_dir).expect("create schema/generated/projection");
    for (family, schema) in pointlock_store::projection::projection_schemas() {
        let path = out_dir.join(format!("{}.schema.json", family.stem()));
        let mut body = serde_json::to_string_pretty(&schema).expect("schema serializes");
        body.push('\n');
        fs::write(&path, body).expect("write schema artifact");
        println!("wrote {}", path.display());
    }
}
