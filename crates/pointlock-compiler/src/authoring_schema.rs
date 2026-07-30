//! `pointlock compile --emit-authoring-schema` (03 §4.1): the authoring
//! surface vocabulary, generated from the compiler's own closed keyword
//! tables and the IR's schema'd enums — never hand-maintained prose, so
//! it cannot drift from what `normalize` actually accepts.
//!
//! The output is a CLI-owned versioned JSON document
//! (`pointlockAuthoringSchema: 1`, the `pointlockReport`/`pointlockBundle`
//! precedent) consumed by the NL drafter as prompt context
//! (`@pointlock/nl-drafter`'s `authoringSchema` input). It is a
//! vocabulary catalog, not a validating YAML schema — the compiler
//! remains the only enforcer (principle: the LLM drafts, `pointlock
//! compile` judges).

use pointlock_ir::PureFn;
use pointlock_ir::vocab::HumanMode;
use serde_json::{Value, json};

use crate::normalize::{
    ASSERTION_KEYS, CHANNEL_TOKENS, DISPOSITION_HEADS, ELEMENT_STATES, HOOK_KEYS, OBSERVE_HEADS,
    OBSERVE_WANTS, RESERVED, STEP_COMMON, STEP_NOT_M2, TOP_KEYS, VERB_HEADS_M1,
};

/// The `${{ ... }}` scope roots (03 §1.9's closed list; `macro.*` is
/// legal inside macro bodies only, `secrets.*` is v0.2-reserved). Each
/// entry is proven against the real `RefPath` grammar by the unit test
/// below — the list cannot silently diverge from the parser.
const SCOPE_ROOT_EXAMPLES: &[(&str, &str)] = &[
    ("params", "params.ssid"),
    ("env", "env.deviceId"),
    ("steps", "steps.someStep.output.value"),
    ("vars", "vars.label"),
    ("iter", "iter.item"),
];

/// Builds the authoring-surface vocabulary document.
pub fn authoring_schema() -> Value {
    json!({
        "pointlockAuthoringSchema": 1,
        "expression": {
            "delimiter": "${{ ... }}",
            "functions": enum_values::<PureFn>(),
            "scopeRoots": SCOPE_ROOT_EXAMPLES
                .iter()
                .map(|(root, _)| *root)
                .collect::<Vec<_>>(),
            "notes": [
                "expressions in YAML flow-context collections must be quoted (03 §1.9)",
                "a ${{ }} embedded in a larger string desugars to concat(...)",
                "macro.* is legal inside macro bodies only; secrets.* is v0.2-reserved",
            ],
        },
        "topLevelKeys": TOP_KEYS,
        "reservedKeywords": RESERVED,
        "step": {
            "commonKeys": STEP_COMMON,
            "notInSubset": STEP_NOT_M2,
            "verbHeads": VERB_HEADS_M1
                .iter()
                .map(|(token, _)| *token)
                .collect::<Vec<_>>(),
            "observeHeads": OBSERVE_HEADS
                .iter()
                .map(|(token, _)| *token)
                .collect::<Vec<_>>(),
            "observeWants": OBSERVE_WANTS,
            "structureHeads": ["invoke", "call", "if", "foreach", "let", "human", "expect"],
            "invokeOnlyKeys": {
                "expect_schema": "narrows the invoke output (03 §1.3); C7 refuses field \
                                  access into an un-narrowed invoke output",
            },
        },
        "assertion": {
            "keys": ASSERTION_KEYS,
            "elementStates": ELEMENT_STATES
                .iter()
                .map(|(token, _)| *token)
                .collect::<Vec<_>>(),
            "channels": CHANNEL_TOKENS
                .iter()
                .map(|(token, _)| *token)
                .collect::<Vec<_>>(),
        },
        "handlers": {
            "hooks": HOOK_KEYS.iter().map(|(token, _)| *token).collect::<Vec<_>>(),
            "dispositionHeads": DISPOSITION_HEADS,
        },
        "human": {
            "modes": enum_values::<HumanMode>(),
            "repairWorldDecisions": ["done", "cannotRepair"],
        },
    })
}

/// The wire literals of a schema'd string enum, extracted from its
/// generated JSON Schema — the same generator that seals artifacts, so
/// the vocabulary here IS the IR's.
fn enum_values<T: schemars::JsonSchema>() -> Vec<String> {
    let schema = schemars::schema_for!(T);
    let value = schema.as_value();
    value["enum"]
        .as_array()
        .or_else(|| {
            // Some enums render as oneOf-of-consts; flatten those.
            value["oneOf"].as_array()
        })
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .or_else(|| item["const"].as_str().map(str::to_owned))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_document_is_generated_from_the_real_tables() {
        let doc = authoring_schema();
        assert_eq!(doc["pointlockAuthoringSchema"], 1);
        // Enum extraction really extracted (empty = the generator's shape
        // changed under us — fail loud, never ship an empty vocabulary).
        let functions = doc["expression"]["functions"]
            .as_array()
            .expect("functions");
        assert!(
            functions.iter().any(|f| f == "concat"),
            "PureFn vocabulary must surface: {functions:?}"
        );
        let modes = doc["human"]["modes"].as_array().expect("modes");
        assert!(modes.iter().any(|m| m == "provideInput"), "{modes:?}");
        // Table coupling: the verb list is the normalize table verbatim.
        assert_eq!(
            doc["step"]["verbHeads"],
            json!(["tap", "set_value", "clear", "wait_for", "find"])
        );
        assert!(
            doc["assertion"]["keys"]
                .as_array()
                .expect("keys")
                .iter()
                .any(|k| k == "value"),
            "the delivered value: sugar must be in the vocabulary"
        );
    }

    #[test]
    fn every_scope_root_is_accepted_by_the_ref_path_grammar() {
        // The advertised roots are proven against the real parser — a
        // root the grammar rejects would be an advertised lie.
        for (root, example) in SCOPE_ROOT_EXAMPLES {
            assert!(
                pointlock_ir::RefPath::new((*example).to_owned()).is_ok(),
                "scope root `{root}` example `{example}` must parse"
            );
        }
        // Negative control: the v0.2-reserved root stays rejected.
        assert!(pointlock_ir::RefPath::new("secrets.apiKey".to_owned()).is_err());
    }
}
