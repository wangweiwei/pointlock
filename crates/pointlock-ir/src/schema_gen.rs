//! FlowIR JSON Schema generation — the first leg of the R12 codegen
//! pipeline (02 §1.1):
//!
//! ```text
//! pointlock-ir Rust DTOs ──→ JSON Schema ──→ (@pointlock/ir-types, fixtures)
//! ```
//!
//! Lives in the library (not only in the `pointlock-ir-schema-gen` bin) so
//! the behavioral-equivalence tests exercise the exact generation logic
//! in-process instead of depending on a previously written artifact; the
//! bin is a thin file-writing wrapper around [`flow_ir_schema`].
//!
//! ## Post-processing
//!
//! Two mechanical passes run over the schemars output:
//!
//! 1. **Option-null stripping.** serde's `Option<T>` accepts JSON `null`,
//!    so schemars adds a null branch for optional fields. The IR forbids
//!    null everywhere (absence-by-omission, 02 §2.4), so `type: [T, "null"]`
//!    collapses to `type: T` and `anyOf: [S, {type: null}]` collapses to `S`.
//! 2. **Root metadata.** `$id` is pinned to the baseline's URN and the title
//!    to `FlowIR`.

use schemars::generate::SchemaSettings;
use serde_json::{Map, Value};

use crate::FlowIR;

/// The canonical `$id` of the FlowIR schema, identical to the acceptance
/// baseline `schema/flow-ir.v0.1.schema.json`.
pub const FLOW_IR_SCHEMA_ID: &str = "urn:pointlock:schema:ir:v0.1:flow-ir";

/// Generates the FlowIR JSON Schema (Draft 2020-12) from the Rust DTOs,
/// post-processed per the module docs. The result must be *behaviorally
/// equivalent* to the acceptance baseline over the golden fixture corpus
/// (02 §1.1); `tests/behavioral_equivalence.rs` enforces that judgment.
pub fn flow_ir_schema() -> Value {
    let settings = SchemaSettings::draft2020_12();
    let mut generator = settings.into_generator();
    let schema = generator.root_schema_for::<FlowIR>();

    let mut doc = serde_json::to_value(&schema).expect("schema serializes to JSON");
    strip_option_null(&mut doc);

    if let Value::Object(root) = &mut doc {
        root.insert(
            "$id".to_owned(),
            Value::String(FLOW_IR_SCHEMA_ID.to_owned()),
        );
        root.insert("title".to_owned(), Value::String("FlowIR".to_owned()));
    }
    doc
}

/// Recursively removes the null-acceptance that schemars adds for
/// `Option<T>` fields (see module docs).
fn strip_option_null(value: &mut Value) {
    match value {
        Value::Object(obj) => {
            strip_null_from_type(obj);
            collapse_nullable_any_of(obj);
            for (_, v) in obj.iter_mut() {
                strip_option_null(v);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_option_null(item);
            }
        }
        _ => {}
    }
}

/// `"type": ["X", "null"]` → `"type": "X"`; longer arrays just drop "null".
fn strip_null_from_type(obj: &mut Map<String, Value>) {
    let Some(Value::Array(types)) = obj.get_mut("type") else {
        return;
    };
    types.retain(|t| t != "null");
    if types.len() == 1 {
        let only = types[0].clone();
        obj.insert("type".to_owned(), only);
    }
}

/// `{"anyOf": [S, {"type": "null"}], ...siblings}` → `S` merged over the
/// siblings (sibling keys such as `description` are preserved; `S` wins on
/// conflicts).
fn collapse_nullable_any_of(obj: &mut Map<String, Value>) {
    let is_null_schema = |v: &Value| matches!(v, Value::Object(o) if o.get("type") == Some(&Value::String("null".to_owned())) && o.len() == 1);

    let Some(Value::Array(branches)) = obj.get("anyOf") else {
        return;
    };
    if branches.len() != 2 || !branches.iter().any(is_null_schema) {
        return;
    }
    let non_null = branches
        .iter()
        .find(|b| !is_null_schema(b))
        .cloned()
        .unwrap_or(Value::Bool(true));

    obj.remove("anyOf");
    if let Value::Object(inner) = non_null {
        for (k, v) in inner {
            obj.insert(k, v);
        }
    }
}
