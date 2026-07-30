/* Generated from schema/generated/flow-ir.schema.json (pointlock-ir Rust DTOs, R12).
 * Do not edit by hand — run `pnpm generate` after re-emitting the schema. */

/**
 * The closed step union (7 kinds, spine A.4), discriminated by `kind` on
 * the wire. See the module docs for why this is `untagged` in serde while
 * remaining a `kind`-tagged union on the wire.
 */
export type StepIR = ActionStepIR | AssertStepIR | CallStepIR | HumanStepIR1 | IfStepIR | ForeachStepIR | LetStepIR;
/**
 * A single assertion with its explicit verify-chain.
 *
 * The three baseline `allOf` conditionals are reproduced on the generated
 * schema via `#[schemars(extend)]`:
 * 1. `expr` predicates consume no observation channel (`verifyVia: []`);
 *    all other predicates need at least one channel.
 * 2. `visual` predicates are vision-only (`verifyVia == ["vision"]`).
 * 3. For `elementState`/`elementText`, `visionPrompt` is required iff the
 *    chain contains `vision`, forbidden otherwise (03 §1.4 rule 5; the
 *    compiler never synthesizes vision prompts, principle 6).
 *
 * "vision only at the chain tail" is order-sensitive and remains a
 * bind-phase check (not expressible in JSON Schema).
 */
export type AssertionIR = {
  [k: string]: unknown;
} & {
  [k: string]: unknown;
} & {
  [k: string]: unknown;
} & {
  /**
   * Stable assertion id (unique within the step).
   */
  assertId: string;
  /**
   * Const `"unknown"` (principle 4): a channel that cannot complete
   * evaluation yields unknown for that channel and the chain advances; an
   * exhausted chain yields unknown. A completed negative is final
   * (spine R5).
   */
  onMissingInput: "unknown";
  /**
   * The predicate to evaluate.
   */
  predicate:
    | {
        selector: ElementSelectorIR;
        /**
         * The expected state.
         */
        state: "present" | "visible" | "enabled" | "absent";
        type: "elementState";
      }
    | {
        match: TextMatchIR1;
        selector: ElementSelectorIR1;
        type: "elementText";
      }
    | {
        /**
         * The boolean expression to evaluate.
         */
        expr: LitExpr | RefExpr | FnExpr;
        type: "expr";
      }
    | {
        /**
         * Author-written vision prompt.
         */
        prompt: string;
        region?: RectIR;
        type: "visual";
      };
  /**
   * Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
   * vision]`. Order carries semantics (degradation order) and participates
   * verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
   * the schema (`uniqueItems`), not by this type.
   */
  verifyVia: VerifyChannel[];
  /**
   * Author-written vision prompt, handed verbatim to the VisionVerifier
   * when `vision` is the declared degraded tail of an
   * `elementState`/`elementText` verify-chain (YAML surface key `visual`).
   * Required iff such a chain contains `vision`, forbidden otherwise.
   * Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
   */
  visionPrompt?: string;
} & {
  /**
   * Stable assertion id (unique within the step).
   */
  assertId: string;
  /**
   * Const `"unknown"` (principle 4): a channel that cannot complete
   * evaluation yields unknown for that channel and the chain advances; an
   * exhausted chain yields unknown. A completed negative is final
   * (spine R5).
   */
  onMissingInput: "unknown";
  /**
   * The predicate to evaluate.
   */
  predicate:
    | {
        selector: ElementSelectorIR;
        /**
         * The expected state.
         */
        state: "present" | "visible" | "enabled" | "absent";
        type: "elementState";
      }
    | {
        match: TextMatchIR1;
        selector: ElementSelectorIR1;
        type: "elementText";
      }
    | {
        /**
         * The boolean expression to evaluate.
         */
        expr: LitExpr | RefExpr | FnExpr;
        type: "expr";
      }
    | {
        /**
         * Author-written vision prompt.
         */
        prompt: string;
        region?: RectIR;
        type: "visual";
      };
  /**
   * Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
   * vision]`. Order carries semantics (degradation order) and participates
   * verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
   * the schema (`uniqueItems`), not by this type.
   */
  verifyVia: VerifyChannel[];
  /**
   * Author-written vision prompt, handed verbatim to the VisionVerifier
   * when `vision` is the declared degraded tail of an
   * `elementState`/`elementText` verify-chain (YAML surface key `visual`).
   * Required iff such a chain contains `vision`, forbidden otherwise.
   * Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
   */
  visionPrompt?: string;
} & {
  /**
   * Stable assertion id (unique within the step).
   */
  assertId: string;
  /**
   * Const `"unknown"` (principle 4): a channel that cannot complete
   * evaluation yields unknown for that channel and the chain advances; an
   * exhausted chain yields unknown. A completed negative is final
   * (spine R5).
   */
  onMissingInput: "unknown";
  /**
   * The predicate to evaluate.
   */
  predicate:
    | {
        selector: ElementSelectorIR;
        /**
         * The expected state.
         */
        state: "present" | "visible" | "enabled" | "absent";
        type: "elementState";
      }
    | {
        match: TextMatchIR1;
        selector: ElementSelectorIR1;
        type: "elementText";
      }
    | {
        /**
         * The boolean expression to evaluate.
         */
        expr: LitExpr | RefExpr | FnExpr;
        type: "expr";
      }
    | {
        /**
         * Author-written vision prompt.
         */
        prompt: string;
        region?: RectIR;
        type: "visual";
      };
  /**
   * Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
   * vision]`. Order carries semantics (degradation order) and participates
   * verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
   * the schema (`uniqueItems`), not by this type.
   */
  verifyVia: VerifyChannel[];
  /**
   * Author-written vision prompt, handed verbatim to the VisionVerifier
   * when `vision` is the declared degraded tail of an
   * `elementState`/`elementText` verify-chain (YAML surface key `visual`).
   * Required iff such a chain contains `vision`, forbidden otherwise.
   * Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
   */
  visionPrompt?: string;
} & {
  /**
   * Stable assertion id (unique within the step).
   */
  assertId: string;
  /**
   * Const `"unknown"` (principle 4): a channel that cannot complete
   * evaluation yields unknown for that channel and the chain advances; an
   * exhausted chain yields unknown. A completed negative is final
   * (spine R5).
   */
  onMissingInput: "unknown";
  /**
   * The predicate to evaluate.
   */
  predicate:
    | {
        selector: ElementSelectorIR;
        /**
         * The expected state.
         */
        state: "present" | "visible" | "enabled" | "absent";
        type: "elementState";
      }
    | {
        match: TextMatchIR1;
        selector: ElementSelectorIR1;
        type: "elementText";
      }
    | {
        /**
         * The boolean expression to evaluate.
         */
        expr: LitExpr | RefExpr | FnExpr;
        type: "expr";
      }
    | {
        /**
         * Author-written vision prompt.
         */
        prompt: string;
        region?: RectIR;
        type: "visual";
      };
  /**
   * Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
   * vision]`. Order carries semantics (degradation order) and participates
   * verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
   * the schema (`uniqueItems`), not by this type.
   */
  verifyVia: VerifyChannel[];
  /**
   * Author-written vision prompt, handed verbatim to the VisionVerifier
   * when `vision` is the declared degraded tail of an
   * `elementState`/`elementText` verify-chain (YAML surface key `visual`).
   * Required iff such a chain contains `vision`, forbidden otherwise.
   * Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
   */
  visionPrompt?: string;
} & {
  /**
   * Stable assertion id (unique within the step).
   */
  assertId: string;
  /**
   * Const `"unknown"` (principle 4): a channel that cannot complete
   * evaluation yields unknown for that channel and the chain advances; an
   * exhausted chain yields unknown. A completed negative is final
   * (spine R5).
   */
  onMissingInput: "unknown";
  /**
   * The predicate to evaluate.
   */
  predicate:
    | {
        selector: ElementSelectorIR;
        /**
         * The expected state.
         */
        state: "present" | "visible" | "enabled" | "absent";
        type: "elementState";
      }
    | {
        match: TextMatchIR1;
        selector: ElementSelectorIR1;
        type: "elementText";
      }
    | {
        /**
         * The boolean expression to evaluate.
         */
        expr: LitExpr | RefExpr | FnExpr;
        type: "expr";
      }
    | {
        /**
         * Author-written vision prompt.
         */
        prompt: string;
        region?: RectIR;
        type: "visual";
      };
  /**
   * Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
   * vision]`. Order carries semantics (degradation order) and participates
   * verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
   * the schema (`uniqueItems`), not by this type.
   */
  verifyVia: VerifyChannel[];
  /**
   * Author-written vision prompt, handed verbatim to the VisionVerifier
   * when `vision` is the declared degraded tail of an
   * `elementState`/`elementText` verify-chain (YAML surface key `visual`).
   * Required iff such a chain contains `vision`, forbidden otherwise.
   * Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
   */
  visionPrompt?: string;
} & {
  /**
   * Stable assertion id (unique within the step).
   */
  assertId: string;
  /**
   * Const `"unknown"` (principle 4): a channel that cannot complete
   * evaluation yields unknown for that channel and the chain advances; an
   * exhausted chain yields unknown. A completed negative is final
   * (spine R5).
   */
  onMissingInput: "unknown";
  /**
   * The predicate to evaluate.
   */
  predicate:
    | {
        selector: ElementSelectorIR;
        /**
         * The expected state.
         */
        state: "present" | "visible" | "enabled" | "absent";
        type: "elementState";
      }
    | {
        match: TextMatchIR1;
        selector: ElementSelectorIR1;
        type: "elementText";
      }
    | {
        /**
         * The boolean expression to evaluate.
         */
        expr: LitExpr | RefExpr | FnExpr;
        type: "expr";
      }
    | {
        /**
         * Author-written vision prompt.
         */
        prompt: string;
        region?: RectIR;
        type: "visual";
      };
  /**
   * Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
   * vision]`. Order carries semantics (degradation order) and participates
   * verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
   * the schema (`uniqueItems`), not by this type.
   */
  verifyVia: VerifyChannel[];
  /**
   * Author-written vision prompt, handed verbatim to the VisionVerifier
   * when `vision` is the declared degraded tail of an
   * `elementState`/`elementText` verify-chain (YAML surface key `visual`).
   * Required iff such a chain contains `vision`, forbidden otherwise.
   * Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
   */
  visionPrompt?: string;
} & {
  /**
   * Stable assertion id (unique within the step).
   */
  assertId: string;
  /**
   * Const `"unknown"` (principle 4): a channel that cannot complete
   * evaluation yields unknown for that channel and the chain advances; an
   * exhausted chain yields unknown. A completed negative is final
   * (spine R5).
   */
  onMissingInput: "unknown";
  /**
   * The predicate to evaluate.
   */
  predicate:
    | {
        selector: ElementSelectorIR;
        /**
         * The expected state.
         */
        state: "present" | "visible" | "enabled" | "absent";
        type: "elementState";
      }
    | {
        match: TextMatchIR1;
        selector: ElementSelectorIR1;
        type: "elementText";
      }
    | {
        /**
         * The boolean expression to evaluate.
         */
        expr: LitExpr | RefExpr | FnExpr;
        type: "expr";
      }
    | {
        /**
         * Author-written vision prompt.
         */
        prompt: string;
        region?: RectIR;
        type: "visual";
      };
  /**
   * Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
   * vision]`. Order carries semantics (degradation order) and participates
   * verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
   * the schema (`uniqueItems`), not by this type.
   */
  verifyVia: VerifyChannel[];
  /**
   * Author-written vision prompt, handed verbatim to the VisionVerifier
   * when `vision` is the declared degraded tail of an
   * `elementState`/`elementText` verify-chain (YAML surface key `visual`).
   * Required iff such a chain contains `vision`, forbidden otherwise.
   * Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
   */
  visionPrompt?: string;
} & {
  /**
   * Stable assertion id (unique within the step).
   */
  assertId: string;
  /**
   * Const `"unknown"` (principle 4): a channel that cannot complete
   * evaluation yields unknown for that channel and the chain advances; an
   * exhausted chain yields unknown. A completed negative is final
   * (spine R5).
   */
  onMissingInput: "unknown";
  /**
   * The predicate to evaluate.
   */
  predicate:
    | {
        selector: ElementSelectorIR;
        /**
         * The expected state.
         */
        state: "present" | "visible" | "enabled" | "absent";
        type: "elementState";
      }
    | {
        match: TextMatchIR1;
        selector: ElementSelectorIR1;
        type: "elementText";
      }
    | {
        /**
         * The boolean expression to evaluate.
         */
        expr: LitExpr | RefExpr | FnExpr;
        type: "expr";
      }
    | {
        /**
         * Author-written vision prompt.
         */
        prompt: string;
        region?: RectIR;
        type: "visual";
      };
  /**
   * Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
   * vision]`. Order carries semantics (degradation order) and participates
   * verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
   * the schema (`uniqueItems`), not by this type.
   */
  verifyVia: VerifyChannel[];
  /**
   * Author-written vision prompt, handed verbatim to the VisionVerifier
   * when `vision` is the declared degraded tail of an
   * `elementState`/`elementText` verify-chain (YAML surface key `visual`).
   * Required iff such a chain contains `vision`, forbidden otherwise.
   * Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
   */
  visionPrompt?: string;
} & {
  /**
   * Stable assertion id (unique within the step).
   */
  assertId: string;
  /**
   * Const `"unknown"` (principle 4): a channel that cannot complete
   * evaluation yields unknown for that channel and the chain advances; an
   * exhausted chain yields unknown. A completed negative is final
   * (spine R5).
   */
  onMissingInput: "unknown";
  /**
   * The predicate to evaluate.
   */
  predicate:
    | {
        selector: ElementSelectorIR;
        /**
         * The expected state.
         */
        state: "present" | "visible" | "enabled" | "absent";
        type: "elementState";
      }
    | {
        match: TextMatchIR1;
        selector: ElementSelectorIR1;
        type: "elementText";
      }
    | {
        /**
         * The boolean expression to evaluate.
         */
        expr: LitExpr | RefExpr | FnExpr;
        type: "expr";
      }
    | {
        /**
         * Author-written vision prompt.
         */
        prompt: string;
        region?: RectIR;
        type: "visual";
      };
  /**
   * Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
   * vision]`. Order carries semantics (degradation order) and participates
   * verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
   * the schema (`uniqueItems`), not by this type.
   */
  verifyVia: VerifyChannel[];
  /**
   * Author-written vision prompt, handed verbatim to the VisionVerifier
   * when `vision` is the declared degraded tail of an
   * `elementState`/`elementText` verify-chain (YAML surface key `visual`).
   * Required iff such a chain contains `vision`, forbidden otherwise.
   * Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
   */
  visionPrompt?: string;
} & {
  /**
   * Stable assertion id (unique within the step).
   */
  assertId: string;
  /**
   * Const `"unknown"` (principle 4): a channel that cannot complete
   * evaluation yields unknown for that channel and the chain advances; an
   * exhausted chain yields unknown. A completed negative is final
   * (spine R5).
   */
  onMissingInput: "unknown";
  /**
   * The predicate to evaluate.
   */
  predicate:
    | {
        selector: ElementSelectorIR;
        /**
         * The expected state.
         */
        state: "present" | "visible" | "enabled" | "absent";
        type: "elementState";
      }
    | {
        match: TextMatchIR1;
        selector: ElementSelectorIR1;
        type: "elementText";
      }
    | {
        /**
         * The boolean expression to evaluate.
         */
        expr: LitExpr | RefExpr | FnExpr;
        type: "expr";
      }
    | {
        /**
         * Author-written vision prompt.
         */
        prompt: string;
        region?: RectIR;
        type: "visual";
      };
  /**
   * Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
   * vision]`. Order carries semantics (degradation order) and participates
   * verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
   * the schema (`uniqueItems`), not by this type.
   */
  verifyVia: VerifyChannel[];
  /**
   * Author-written vision prompt, handed verbatim to the VisionVerifier
   * when `vision` is the declared degraded tail of an
   * `elementState`/`elementText` verify-chain (YAML surface key `visual`).
   * Required iff such a chain contains `vision`, forbidden otherwise.
   * Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
   */
  visionPrompt?: string;
} & {
  /**
   * Stable assertion id (unique within the step).
   */
  assertId: string;
  /**
   * Const `"unknown"` (principle 4): a channel that cannot complete
   * evaluation yields unknown for that channel and the chain advances; an
   * exhausted chain yields unknown. A completed negative is final
   * (spine R5).
   */
  onMissingInput: "unknown";
  /**
   * The predicate to evaluate.
   */
  predicate:
    | {
        selector: ElementSelectorIR;
        /**
         * The expected state.
         */
        state: "present" | "visible" | "enabled" | "absent";
        type: "elementState";
      }
    | {
        match: TextMatchIR1;
        selector: ElementSelectorIR1;
        type: "elementText";
      }
    | {
        /**
         * The boolean expression to evaluate.
         */
        expr: LitExpr | RefExpr | FnExpr;
        type: "expr";
      }
    | {
        /**
         * Author-written vision prompt.
         */
        prompt: string;
        region?: RectIR;
        type: "visual";
      };
  /**
   * Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
   * vision]`. Order carries semantics (degradation order) and participates
   * verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
   * the schema (`uniqueItems`), not by this type.
   */
  verifyVia: VerifyChannel[];
  /**
   * Author-written vision prompt, handed verbatim to the VisionVerifier
   * when `vision` is the declared degraded tail of an
   * `elementState`/`elementText` verify-chain (YAML surface key `visual`).
   * Required iff such a chain contains `vision`, forbidden otherwise.
   * Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
   */
  visionPrompt?: string;
} & {
  /**
   * Stable assertion id (unique within the step).
   */
  assertId: string;
  /**
   * Const `"unknown"` (principle 4): a channel that cannot complete
   * evaluation yields unknown for that channel and the chain advances; an
   * exhausted chain yields unknown. A completed negative is final
   * (spine R5).
   */
  onMissingInput: "unknown";
  /**
   * The predicate to evaluate.
   */
  predicate:
    | {
        selector: ElementSelectorIR;
        /**
         * The expected state.
         */
        state: "present" | "visible" | "enabled" | "absent";
        type: "elementState";
      }
    | {
        match: TextMatchIR1;
        selector: ElementSelectorIR1;
        type: "elementText";
      }
    | {
        /**
         * The boolean expression to evaluate.
         */
        expr: LitExpr | RefExpr | FnExpr;
        type: "expr";
      }
    | {
        /**
         * Author-written vision prompt.
         */
        prompt: string;
        region?: RectIR;
        type: "visual";
      };
  /**
   * Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
   * vision]`. Order carries semantics (degradation order) and participates
   * verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
   * the schema (`uniqueItems`), not by this type.
   */
  verifyVia: VerifyChannel[];
  /**
   * Author-written vision prompt, handed verbatim to the VisionVerifier
   * when `vision` is the declared degraded tail of an
   * `elementState`/`elementText` verify-chain (YAML surface key `visual`).
   * Required iff such a chain contains `vision`, forbidden otherwise.
   * Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
   */
  visionPrompt?: string;
} & {
  /**
   * Stable assertion id (unique within the step).
   */
  assertId: string;
  /**
   * Const `"unknown"` (principle 4): a channel that cannot complete
   * evaluation yields unknown for that channel and the chain advances; an
   * exhausted chain yields unknown. A completed negative is final
   * (spine R5).
   */
  onMissingInput: "unknown";
  /**
   * The predicate to evaluate.
   */
  predicate:
    | {
        selector: ElementSelectorIR;
        /**
         * The expected state.
         */
        state: "present" | "visible" | "enabled" | "absent";
        type: "elementState";
      }
    | {
        match: TextMatchIR1;
        selector: ElementSelectorIR1;
        type: "elementText";
      }
    | {
        /**
         * The boolean expression to evaluate.
         */
        expr: LitExpr | RefExpr | FnExpr;
        type: "expr";
      }
    | {
        /**
         * Author-written vision prompt.
         */
        prompt: string;
        region?: RectIR;
        type: "visual";
      };
  /**
   * Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
   * vision]`. Order carries semantics (degradation order) and participates
   * verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
   * the schema (`uniqueItems`), not by this type.
   */
  verifyVia: VerifyChannel[];
  /**
   * Author-written vision prompt, handed verbatim to the VisionVerifier
   * when `vision` is the declared degraded tail of an
   * `elementState`/`elementText` verify-chain (YAML surface key `visual`).
   * Required iff such a chain contains `vision`, forbidden otherwise.
   * Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
   */
  visionPrompt?: string;
} & {
  /**
   * Stable assertion id (unique within the step).
   */
  assertId: string;
  /**
   * Const `"unknown"` (principle 4): a channel that cannot complete
   * evaluation yields unknown for that channel and the chain advances; an
   * exhausted chain yields unknown. A completed negative is final
   * (spine R5).
   */
  onMissingInput: "unknown";
  /**
   * The predicate to evaluate.
   */
  predicate:
    | {
        selector: ElementSelectorIR;
        /**
         * The expected state.
         */
        state: "present" | "visible" | "enabled" | "absent";
        type: "elementState";
      }
    | {
        match: TextMatchIR1;
        selector: ElementSelectorIR1;
        type: "elementText";
      }
    | {
        /**
         * The boolean expression to evaluate.
         */
        expr: LitExpr | RefExpr | FnExpr;
        type: "expr";
      }
    | {
        /**
         * Author-written vision prompt.
         */
        prompt: string;
        region?: RectIR;
        type: "visual";
      };
  /**
   * Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
   * vision]`. Order carries semantics (degradation order) and participates
   * verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
   * the schema (`uniqueItems`), not by this type.
   */
  verifyVia: VerifyChannel[];
  /**
   * Author-written vision prompt, handed verbatim to the VisionVerifier
   * when `vision` is the declared degraded tail of an
   * `elementState`/`elementText` verify-chain (YAML surface key `visual`).
   * Required iff such a chain contains `vision`, forbidden otherwise.
   * Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
   */
  visionPrompt?: string;
} & {
  /**
   * Stable assertion id (unique within the step).
   */
  assertId: string;
  /**
   * Const `"unknown"` (principle 4): a channel that cannot complete
   * evaluation yields unknown for that channel and the chain advances; an
   * exhausted chain yields unknown. A completed negative is final
   * (spine R5).
   */
  onMissingInput: "unknown";
  /**
   * The predicate to evaluate.
   */
  predicate:
    | {
        selector: ElementSelectorIR;
        /**
         * The expected state.
         */
        state: "present" | "visible" | "enabled" | "absent";
        type: "elementState";
      }
    | {
        match: TextMatchIR1;
        selector: ElementSelectorIR1;
        type: "elementText";
      }
    | {
        /**
         * The boolean expression to evaluate.
         */
        expr: LitExpr | RefExpr | FnExpr;
        type: "expr";
      }
    | {
        /**
         * Author-written vision prompt.
         */
        prompt: string;
        region?: RectIR;
        type: "visual";
      };
  /**
   * Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
   * vision]`. Order carries semantics (degradation order) and participates
   * verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
   * the schema (`uniqueItems`), not by this type.
   */
  verifyVia: VerifyChannel[];
  /**
   * Author-written vision prompt, handed verbatim to the VisionVerifier
   * when `vision` is the declared degraded tail of an
   * `elementState`/`elementText` verify-chain (YAML surface key `visual`).
   * Required iff such a chain contains `vision`, forbidden otherwise.
   * Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
   */
  visionPrompt?: string;
} & {
  /**
   * Stable assertion id (unique within the step).
   */
  assertId: string;
  /**
   * Const `"unknown"` (principle 4): a channel that cannot complete
   * evaluation yields unknown for that channel and the chain advances; an
   * exhausted chain yields unknown. A completed negative is final
   * (spine R5).
   */
  onMissingInput: "unknown";
  /**
   * The predicate to evaluate.
   */
  predicate:
    | {
        selector: ElementSelectorIR;
        /**
         * The expected state.
         */
        state: "present" | "visible" | "enabled" | "absent";
        type: "elementState";
      }
    | {
        match: TextMatchIR1;
        selector: ElementSelectorIR1;
        type: "elementText";
      }
    | {
        /**
         * The boolean expression to evaluate.
         */
        expr: LitExpr | RefExpr | FnExpr;
        type: "expr";
      }
    | {
        /**
         * Author-written vision prompt.
         */
        prompt: string;
        region?: RectIR;
        type: "visual";
      };
  /**
   * Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
   * vision]`. Order carries semantics (degradation order) and participates
   * verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
   * the schema (`uniqueItems`), not by this type.
   */
  verifyVia: VerifyChannel[];
  /**
   * Author-written vision prompt, handed verbatim to the VisionVerifier
   * when `vision` is the declared degraded tail of an
   * `elementState`/`elementText` verify-chain (YAML surface key `visual`).
   * Required iff such a chain contains `vision`, forbidden otherwise.
   * Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
   */
  visionPrompt?: string;
} & {
  /**
   * Stable assertion id (unique within the step).
   */
  assertId: string;
  /**
   * Const `"unknown"` (principle 4): a channel that cannot complete
   * evaluation yields unknown for that channel and the chain advances; an
   * exhausted chain yields unknown. A completed negative is final
   * (spine R5).
   */
  onMissingInput: "unknown";
  /**
   * The predicate to evaluate.
   */
  predicate:
    | {
        selector: ElementSelectorIR;
        /**
         * The expected state.
         */
        state: "present" | "visible" | "enabled" | "absent";
        type: "elementState";
      }
    | {
        match: TextMatchIR1;
        selector: ElementSelectorIR1;
        type: "elementText";
      }
    | {
        /**
         * The boolean expression to evaluate.
         */
        expr: LitExpr | RefExpr | FnExpr;
        type: "expr";
      }
    | {
        /**
         * Author-written vision prompt.
         */
        prompt: string;
        region?: RectIR;
        type: "visual";
      };
  /**
   * Explicit, ordered verify-chain — a subsequence of `[dom, uiTree,
   * vision]`. Order carries semantics (degradation order) and participates
   * verbatim in `judgeHash` (02 §12.1 rule 5). Uniqueness is enforced by
   * the schema (`uniqueItems`), not by this type.
   */
  verifyVia: VerifyChannel[];
  /**
   * Author-written vision prompt, handed verbatim to the VisionVerifier
   * when `vision` is the declared degraded tail of an
   * `elementState`/`elementText` verify-chain (YAML surface key `visual`).
   * Required iff such a chain contains `vision`, forbidden otherwise.
   * Part of the assertion, hence inside the `judgeHash` domain (02 §12.3).
   */
  visionPrompt?: string;
};
/**
 * Whitelisted pure-function application.
 */
export type FnExpr = {
  [k: string]: unknown;
} & {
  /**
   * Ordered argument expressions.
   */
  args: Expr[];
  /**
   * The pure function to apply.
   */
  fn: "eq" | "ne" | "not" | "and" | "or" | "concat" | "len" | "coalesce" | "jsonPath" | "regexMatch";
};
/**
 * Expression node: exactly one of `lit` / `ref` / `fn` (02 §8.1).
 *
 * Wire shape is the baseline schema's `oneOf` of three closed single-key
 * objects; the variants are mutually exclusive by their required keys, so
 * serde's untagged representation is deterministic.
 */
export type Expr = LitExpr | RefExpr | FnExpr;
/**
 * Channel subset legal on the verify-chain. `coordinate` is structurally
 * excluded (a coordinate cannot verify anything).
 */
export type VerifyChannel = "dom" | "uiTree" | "vision";
/**
 * Actual execution mode reported by the DeviceRail daemon; the whitelist
 * semantics live on [`crate::BoundAttempt::accept_execution_modes`]
 * (spine §6.4 R-degrade).
 */
export type ExecutionMode = "nativeSemantic" | "webSemantic" | "coordinateFallback";
/**
 * Pointlock-layer closed error taxonomy (spine §5). DeviceRail
 * `ErrorInfo.code` is an open string set mapped onto this enum.
 *
 * snake_case on the wire, aligned with DeviceRail error-code style.
 */
export type ErrorClass =
  | "capability_drift"
  | "bind_arguments_invalid"
  | "action_failed_retryable"
  | "action_failed_final"
  | "action_timed_out"
  | "action_cancelled"
  | "target_stale"
  | "transport_lost"
  | "session_degraded";
/**
 * DeviceRail feature id passthrough (e.g. 'device.semanticActions.v1'); future Pointlock-owned features use the 'pointlock.' prefix.
 */
export type FeatureId = string;

/**
 * Pointlock Typed IR v0.1 — the sole input accepted by `pointlock-runner` and
 * the sole output of the `pointlock-compiler` seal phase.
 *
 * Closed vocabulary per spine Appendix A. All objects are closed except the
 * three documented exemption classes (02 §2.2): embedded JSON Schema
 * documents, identifier-keyed maps, and `StepBase` (composed into variants).
 */
export interface FlowIR {
  /**
   * The step body (≥ 1 step).
   *
   * @minItems 1
   */
  body: [StepIR, ...StepIR[]];
  /**
   * The flow's name-identity.
   */
  flowId: string;
  /**
   * Flow-level handler hooks.
   *
   * @minItems 1
   */
  handlers?: [HandlerBinding, ...HandlerBinding[]];
  /**
   * Canonical whole-tree hash (excluding `irHash` itself and `sourceMap`;
   * covers callee irHashes via `subflows` — the link-closure property,
   * 02 §12.2).
   */
  irHash: string;
  /**
   * IR semantic-generation number, const `1` in v0.1.
   */
  irVersion: 1;
  /**
   * Digest of the `CapabilityLockfile` used at bind time; attestation
   * mismatch at runtime is `capability_drift`, refuse to run.
   */
  lockfileDigest: string;
  /**
   * Output contract.
   */
  outputs: OutputDecl[];
  /**
   * Input contract.
   */
  params: ParamDecl[];
  /**
   * The provider this flow was compiled against.
   */
  provider: {
    /**
     * Const `"devicerail"` — the only provider of v0.1.
     */
    name: "devicerail";
    /**
     * Provider package version the manifest came from.
     */
    version: string;
  };
  /**
   * Union of features required by the whole flow; fed into
   * `FeatureOffer.required` at session open (free enforcement).
   * Set semantics — serialized in lexicographic order.
   */
  requiredFeatures: FeatureId[];
  /**
   * IR path → YAML span mapping, plus macro origin traces. Pure
   * diagnostics: excluded from `irHash` (02 §12.2).
   */
  sourceMap: SourceMapEntry[];
  /**
   * Subflow registry: reference, not inline — callees are independent
   * artifacts pinned by `irHash` (02 §6).
   */
  subflows: {
    [k: string]: FlowRef2;
  };
  /**
   * Verdict folding policy (`strict` folds degraded pass to unknown).
   */
  verdictPolicy: "standard" | "strict";
}
/**
 * `kind: "action"` — fixed pipeline `preflight? → act → observe → assert`.
 */
export interface ActionStepIR {
  /**
   * Post-hoc assertions. Empty array ⇒ this step yields no verdict.
   */
  assertions: AssertionIR[];
  binding: ActionBinding;
  /**
   * Whether to materialize a checkpoint at this step boundary. Required
   * because sealed IR materializes all defaulted fields
   * (single-representation rule; default true, false inside macro
   * expansions).
   */
  checkpoint: boolean;
  /**
   * `mutating | readonly` (`pure` is excluded — it belongs to `let`).
   */
  effect: "mutating" | "readonly";
  /**
   * Canonical hash of "what this step does to the world" (02 §12.3).
   */
  effectHash: string;
  /**
   * Step-level handler hooks; override flow-level ones.
   *
   * @minItems 1
   */
  handlers?: [HandlerBinding, ...HandlerBinding[]];
  /**
   * Author-declared idempotence (materialized default: false). Governs
   * timed-out auto-retry and reconcile-uncertain replay permission.
   */
  idempotent: boolean;
  /**
   * Canonical hash of "how this step is judged" (02 §12.3).
   */
  judgeHash: string;
  /**
   * Const `"action"`.
   */
  kind: "action";
  /**
   * Data contract of the projected output, for downstream static checks.
   */
  outputSchema?: {} | boolean;
  outputs?: ExprMap1;
  /**
   * Pre-entry world probes; double as resume drift detection
   * (spine §6.7-C). Distinct from post-hoc `assertions` (`expect`).
   *
   * @minItems 1
   */
  preflight?: [AssertionIR, ...AssertionIR[]];
  retry?: RetryPolicy2;
  /**
   * Author-provided, flow-unique, stable step identity.
   */
  stepId: string;
  /**
   * Step budget in milliseconds.
   */
  timeoutMs?: number;
  /**
   * Canonical verb — pure metadata for reports; the runner has no verb
   * switch (spine R7).
   */
  verb?: "tap" | "set_value" | "clear" | "wait_for" | "find" | "observe" | "screenshot" | "invoke";
}
/**
 * The element to check.
 */
export interface ElementSelectorIR {
  /**
   * UI context scoping (native/web, optional context id).
   */
  context?: {
    /**
     * Optional concrete context id.
     */
    contextId?: string;
    /**
     * Context kind (`native` | `web`).
     */
    contextKind: "native" | "web";
  };
  /**
   * CSS selector (web contexts).
   */
  css?: string;
  /**
   * Stable identifier (resource id / test id).
   */
  identifier?: string;
  /**
   * Accessible name.
   */
  name?: string;
  /**
   * Accessibility role.
   */
  role?: string;
  text?: TextMatchIR;
  /**
   * Current value match.
   */
  value?: string;
}
/**
 * Text content match.
 */
export interface TextMatchIR {
  /**
   * Case sensitivity (materialized default: `false`).
   */
  caseSensitive: boolean;
  /**
   * Match mode (materialized default: `exact`).
   */
  mode: "exact" | "contains";
  /**
   * The text to match.
   */
  value: string;
}
/**
 * The text matcher.
 */
export interface TextMatchIR1 {
  /**
   * Case sensitivity (materialized default: `false`).
   */
  caseSensitive: boolean;
  /**
   * Match mode (materialized default: `exact`).
   */
  mode: "exact" | "contains";
  /**
   * The text to match.
   */
  value: string;
}
/**
 * The element to check.
 */
export interface ElementSelectorIR1 {
  /**
   * UI context scoping (native/web, optional context id).
   */
  context?: {
    /**
     * Optional concrete context id.
     */
    contextId?: string;
    /**
     * Context kind (`native` | `web`).
     */
    contextKind: "native" | "web";
  };
  /**
   * CSS selector (web contexts).
   */
  css?: string;
  /**
   * Stable identifier (resource id / test id).
   */
  identifier?: string;
  /**
   * Accessible name.
   */
  name?: string;
  /**
   * Accessibility role.
   */
  role?: string;
  text?: TextMatchIR;
  /**
   * Current value match.
   */
  value?: string;
}
/**
 * Literal JSON value.
 */
export interface LitExpr {
  /**
   * The literal value, verbatim.
   */
  lit: {
    [k: string]: unknown;
  };
}
/**
 * Reference into the closed scope grammar.
 */
export interface RefExpr {
  /**
   * The dotted reference path.
   */
  ref: string;
}
/**
 * Optional region of interest.
 */
export interface RectIR {
  /**
   * Height (≥ 0).
   */
  height: number;
  /**
   * Width (≥ 0).
   */
  width: number;
  /**
   * X origin.
   */
  x: number;
  /**
   * Y origin.
   */
  y: number;
}
/**
 * The compile-time fully bound act-chain.
 */
export interface ActionBinding {
  /**
   * The attempts, in declared order. A subsequent attempt is tried only
   * after `action_failed_final` of the previous one (spine §6.2).
   *
   * @minItems 1
   */
  attempts: [BoundAttempt, ...BoundAttempt[]];
}
/**
 * One fully bound attempt of the act-chain (02 §5.1).
 *
 * `protection` is const `"standard"` in v0.1: bind rejects protected
 * actions (spine R6). `coordinate` attempts must carry literal static
 * coordinates in `args` (bind-phase check).
 */
export interface BoundAttempt {
  /**
   * Whitelist of daemon-internal execution modes (spine §6.4 R-degrade).
   * Derived per attempt from its own `channel` only; semantic attempts
   * never include `coordinateFallback`. Set semantics — serialized in
   * declaration order of [`ExecutionMode`], deduplicated on load.
   *
   * @minItems 1
   */
  acceptExecutionModes: [ExecutionMode, ...ExecutionMode[]];
  /**
   * Provider-native action name (per lockfile.device.actions).
   */
  actionName: string;
  args: ExprMap;
  /**
   * Locating channel — [`ActChannel`], so `vision` is structurally
   * impossible here (principle 7).
   */
  channel: "dom" | "uiTree" | "coordinate";
  /**
   * Const `"standard"` in v0.1 (spine R6).
   */
  protection: "standard";
  /**
   * Feature this attempt depends on (e.g. `device.semanticActions.v1`).
   */
  requiresFeature?: string;
}
/**
 * Arguments as expressions; shape-checked against the action's
 * `inputSchema` at bind time and re-checked after evaluation at runtime.
 */
export interface ExprMap {
  [k: string]: Expr;
}
/**
 * A handler mounted on a hook.
 *
 * The `errorClasses` filter is legal only on `onError` — the baseline
 * `if`/`else` conditional is reproduced via `#[schemars(extend)]`.
 */
export interface HandlerBinding {
  /**
   * What to do when the hook fires.
   */
  action:
    | {
        kind: "retry";
        policy: RetryPolicy;
      }
    | {
        kind: "continue";
      }
    | {
        human: HumanStepIR;
        kind: "escalate";
      }
    | {
        kind: "abort";
      }
    | {
        flowRef: FlowRef;
        kind: "repair";
      };
  /**
   * Error-class filter — only meaningful (and only legal) on `onError`.
   *
   * @minItems 1
   */
  errorClasses?: [ErrorClass, ...ErrorClass[]];
  /**
   * The hook this handler fires on.
   */
  hook: "onFail" | "onUnknown" | "onError" | "onResumeDrift";
  /**
   * Trigger budget (loop guard).
   */
  maxTriggers: number;
}
/**
 * The retry policy for the re-entry.
 */
export interface RetryPolicy {
  /**
   * Backoff: fixed milliseconds or an exponential schedule.
   */
  backoffMs:
    | number
    | {
        /**
         * Multiplication factor (≥ 1).
         */
        factor: number;
        /**
         * Initial delay in milliseconds (≥ 0).
         */
        initial: number;
        /**
         * Delay ceiling in milliseconds (≥ 0).
         */
        max: number;
      };
  /**
   * Attempt budget (≥ 1).
   */
  maxAttempts: number;
  /**
   * Which error classes are retryable here. Semantically meaningful:
   * `action_failed_retryable`, `target_stale` (forces re-observe), and —
   * for idempotent steps — `action_timed_out`; `check` warns on the rest.
   * Set semantics — serialized in [`ErrorClass`] declaration order.
   *
   * @minItems 1
   */
  retryOn: [ErrorClass, ...ErrorClass[]];
}
/**
 * The embedded human step (boxed: much larger than sibling variants).
 */
export interface HumanStepIR {
  /**
   * Whether to materialize a checkpoint at this step boundary. Required
   * because sealed IR materializes all defaulted fields
   * (single-representation rule; default true, false inside macro
   * expansions).
   */
  checkpoint: boolean;
  /**
   * Enumerated options for judge/confirm modes.
   *
   * @minItems 1
   */
  decisions?: [string, ...string[]];
  /**
   * Canonical hash of "what this step does to the world" (02 §12.3).
   */
  effectHash: string;
  /**
   * Step-level handler hooks; override flow-level ones.
   *
   * @minItems 1
   */
  handlers?: [HandlerBinding, ...HandlerBinding[]];
  /**
   * Canonical hash of "how this step is judged" (02 §12.3).
   */
  judgeHash: string;
  /**
   * Const `"human"`.
   */
  kind: "human";
  /**
   * Interaction mode.
   */
  mode: "confirm" | "judge" | "provideInput" | "repairWorld";
  /**
   * Const `"unknown"` (principle 4).
   */
  onTimeout: "unknown";
  /**
   * Input contract for `provideInput` mode (required there, schema-enforced).
   */
  outputSchema?: {} | boolean;
  /**
   * Pre-entry world probes; double as resume drift detection
   * (spine §6.7-C). Distinct from post-hoc `assertions` (`expect`).
   *
   * @minItems 1
   */
  preflight?: [AssertionIR, ...AssertionIR[]];
  /**
   * Evidence/values presented to the human (expressions).
   */
  presents: Expr[];
  /**
   * The question posed to the human.
   */
  prompt: string;
  retry?: RetryPolicy1;
  /**
   * Author-provided, flow-unique, stable step identity.
   */
  stepId: string;
  /**
   * Required budget; expiry yields verdict `unknown`.
   */
  timeoutMs: number;
}
/**
 * Retry policy — applies to the act phase only (spine §6.5 mount 1).
 */
export interface RetryPolicy1 {
  /**
   * Backoff: fixed milliseconds or an exponential schedule.
   */
  backoffMs:
    | number
    | {
        /**
         * Multiplication factor (≥ 1).
         */
        factor: number;
        /**
         * Initial delay in milliseconds (≥ 0).
         */
        initial: number;
        /**
         * Delay ceiling in milliseconds (≥ 0).
         */
        max: number;
      };
  /**
   * Attempt budget (≥ 1).
   */
  maxAttempts: number;
  /**
   * Which error classes are retryable here. Semantically meaningful:
   * `action_failed_retryable`, `target_stale` (forces re-observe), and —
   * for idempotent steps — `action_timed_out`; `check` warns on the rest.
   * Set semantics — serialized in [`ErrorClass`] declaration order.
   *
   * @minItems 1
   */
  retryOn: [ErrorClass, ...ErrorClass[]];
}
/**
 * The repair subflow, pinned like a `call`.
 */
export interface FlowRef {
  /**
   * The callee's flow id.
   */
  flowId: string;
  /**
   * The callee's content hash (integrity pin; runner verifies on load).
   */
  irHash: string;
}
/**
 * Output projection: `Record<name, Expr>` over `ActionResult.output` /
 * observation metadata. Self-refs inside refer to the *raw* output
 * (02 §4.1.1). Absent ⇒ identity projection.
 */
export interface ExprMap1 {
  [k: string]: Expr;
}
/**
 * Retry policy — applies to the act phase only (spine §6.5 mount 1).
 */
export interface RetryPolicy2 {
  /**
   * Backoff: fixed milliseconds or an exponential schedule.
   */
  backoffMs:
    | number
    | {
        /**
         * Multiplication factor (≥ 1).
         */
        factor: number;
        /**
         * Initial delay in milliseconds (≥ 0).
         */
        initial: number;
        /**
         * Delay ceiling in milliseconds (≥ 0).
         */
        max: number;
      };
  /**
   * Attempt budget (≥ 1).
   */
  maxAttempts: number;
  /**
   * Which error classes are retryable here. Semantically meaningful:
   * `action_failed_retryable`, `target_stale` (forces re-observe), and —
   * for idempotent steps — `action_timed_out`; `check` warns on the rest.
   * Set semantics — serialized in [`ErrorClass`] declaration order.
   *
   * @minItems 1
   */
  retryOn: [ErrorClass, ...ErrorClass[]];
}
/**
 * `kind: "assert"` — side-effect-free observation and judgment.
 */
export interface AssertStepIR {
  /**
   * At least one assertion (an assertion-free assert step is meaningless).
   *
   * @minItems 1
   */
  assertions: [AssertionIR, ...AssertionIR[]];
  /**
   * Whether to materialize a checkpoint at this step boundary. Required
   * because sealed IR materializes all defaulted fields
   * (single-representation rule; default true, false inside macro
   * expansions).
   */
  checkpoint: boolean;
  /**
   * Canonical hash of "what this step does to the world" (02 §12.3).
   */
  effectHash: string;
  /**
   * Step-level handler hooks; override flow-level ones.
   *
   * @minItems 1
   */
  handlers?: [HandlerBinding, ...HandlerBinding[]];
  /**
   * Canonical hash of "how this step is judged" (02 §12.3).
   */
  judgeHash: string;
  /**
   * Const `"assert"`.
   */
  kind: "assert";
  /**
   * Observation source: fresh capture or reuse of an action step's
   * before/after observation (offline re-judgeable).
   */
  observe:
    | "fresh"
    | {
        /**
         * The action step whose observation is reused.
         */
        fromStep: string;
        /**
         * Which observation (`after` | `before`).
         */
        which: "after" | "before";
      };
  /**
   * Pre-entry world probes; double as resume drift detection
   * (spine §6.7-C). Distinct from post-hoc `assertions` (`expect`).
   *
   * @minItems 1
   */
  preflight?: [AssertionIR, ...AssertionIR[]];
  retry?: RetryPolicy3;
  /**
   * Author-provided, flow-unique, stable step identity.
   */
  stepId: string;
  /**
   * Step budget in milliseconds.
   */
  timeoutMs?: number;
}
/**
 * Retry policy — applies to the act phase only (spine §6.5 mount 1).
 */
export interface RetryPolicy3 {
  /**
   * Backoff: fixed milliseconds or an exponential schedule.
   */
  backoffMs:
    | number
    | {
        /**
         * Multiplication factor (≥ 1).
         */
        factor: number;
        /**
         * Initial delay in milliseconds (≥ 0).
         */
        initial: number;
        /**
         * Delay ceiling in milliseconds (≥ 0).
         */
        max: number;
      };
  /**
   * Attempt budget (≥ 1).
   */
  maxAttempts: number;
  /**
   * Which error classes are retryable here. Semantically meaningful:
   * `action_failed_retryable`, `target_stale` (forces re-observe), and —
   * for idempotent steps — `action_timed_out`; `check` warns on the rest.
   * Set semantics — serialized in [`ErrorClass`] declaration order.
   *
   * @minItems 1
   */
  retryOn: [ErrorClass, ...ErrorClass[]];
}
/**
 * `kind: "call"` — subflow invocation, pinned by content hash.
 */
export interface CallStepIR {
  /**
   * Whether to materialize a checkpoint at this step boundary. Required
   * because sealed IR materializes all defaulted fields
   * (single-representation rule; default true, false inside macro
   * expansions).
   */
  checkpoint: boolean;
  /**
   * Canonical hash of "what this step does to the world" (02 §12.3).
   */
  effectHash: string;
  flowRef: FlowRef1;
  /**
   * Step-level handler hooks; override flow-level ones.
   *
   * @minItems 1
   */
  handlers?: [HandlerBinding, ...HandlerBinding[]];
  inputs: ExprMap2;
  /**
   * Canonical hash of "how this step is judged" (02 §12.3).
   */
  judgeHash: string;
  /**
   * Const `"call"`.
   */
  kind: "call";
  /**
   * Pre-entry world probes; double as resume drift detection
   * (spine §6.7-C). Distinct from post-hoc `assertions` (`expect`).
   *
   * @minItems 1
   */
  preflight?: [AssertionIR, ...AssertionIR[]];
  retry?: RetryPolicy4;
  /**
   * Author-provided, flow-unique, stable step identity.
   */
  stepId: string;
  /**
   * Step budget in milliseconds.
   */
  timeoutMs?: number;
}
/**
 * The callee, pinned by `flowId` + `irHash`.
 */
export interface FlowRef1 {
  /**
   * The callee's flow id.
   */
  flowId: string;
  /**
   * The callee's content hash (integrity pin; runner verifies on load).
   */
  irHash: string;
}
/**
 * Caller-scope input expressions (call-by-value snapshot).
 */
export interface ExprMap2 {
  [k: string]: Expr;
}
/**
 * Retry policy — applies to the act phase only (spine §6.5 mount 1).
 */
export interface RetryPolicy4 {
  /**
   * Backoff: fixed milliseconds or an exponential schedule.
   */
  backoffMs:
    | number
    | {
        /**
         * Multiplication factor (≥ 1).
         */
        factor: number;
        /**
         * Initial delay in milliseconds (≥ 0).
         */
        initial: number;
        /**
         * Delay ceiling in milliseconds (≥ 0).
         */
        max: number;
      };
  /**
   * Attempt budget (≥ 1).
   */
  maxAttempts: number;
  /**
   * Which error classes are retryable here. Semantically meaningful:
   * `action_failed_retryable`, `target_stale` (forces re-observe), and —
   * for idempotent steps — `action_timed_out`; `check` warns on the rest.
   * Set semantics — serialized in [`ErrorClass`] declaration order.
   *
   * @minItems 1
   */
  retryOn: [ErrorClass, ...ErrorClass[]];
}
/**
 * `kind: "human"` — human collaboration node (principle 8).
 */
export interface HumanStepIR1 {
  /**
   * Whether to materialize a checkpoint at this step boundary. Required
   * because sealed IR materializes all defaulted fields
   * (single-representation rule; default true, false inside macro
   * expansions).
   */
  checkpoint: boolean;
  /**
   * Enumerated options for judge/confirm modes.
   *
   * @minItems 1
   */
  decisions?: [string, ...string[]];
  /**
   * Canonical hash of "what this step does to the world" (02 §12.3).
   */
  effectHash: string;
  /**
   * Step-level handler hooks; override flow-level ones.
   *
   * @minItems 1
   */
  handlers?: [HandlerBinding, ...HandlerBinding[]];
  /**
   * Canonical hash of "how this step is judged" (02 §12.3).
   */
  judgeHash: string;
  /**
   * Const `"human"`.
   */
  kind: "human";
  /**
   * Interaction mode.
   */
  mode: "confirm" | "judge" | "provideInput" | "repairWorld";
  /**
   * Const `"unknown"` (principle 4).
   */
  onTimeout: "unknown";
  /**
   * Input contract for `provideInput` mode (required there, schema-enforced).
   */
  outputSchema?: {} | boolean;
  /**
   * Pre-entry world probes; double as resume drift detection
   * (spine §6.7-C). Distinct from post-hoc `assertions` (`expect`).
   *
   * @minItems 1
   */
  preflight?: [AssertionIR, ...AssertionIR[]];
  /**
   * Evidence/values presented to the human (expressions).
   */
  presents: Expr[];
  /**
   * The question posed to the human.
   */
  prompt: string;
  retry?: RetryPolicy1;
  /**
   * Author-provided, flow-unique, stable step identity.
   */
  stepId: string;
  /**
   * Required budget; expiry yields verdict `unknown`.
   */
  timeoutMs: number;
}
/**
 * `kind: "if"` — conditional container.
 */
export interface IfStepIR {
  /**
   * Whether to materialize a checkpoint at this step boundary. Required
   * because sealed IR materializes all defaulted fields
   * (single-representation rule; default true, false inside macro
   * expansions).
   */
  checkpoint: boolean;
  /**
   * Expression node: exactly one of `lit` / `ref` / `fn` (02 §8.1).
   *
   * Wire shape is the baseline schema's `oneOf` of three closed single-key
   * objects; the variants are mutually exclusive by their required keys, so
   * serde's untagged representation is deterministic.
   */
  cond: LitExpr | RefExpr | FnExpr;
  /**
   * Canonical hash of "what this step does to the world" (02 §12.3).
   */
  effectHash: string;
  /**
   * Steps executed otherwise (≥ 1 when present). Unselected branch steps
   * are `skipped`.
   *
   * @minItems 1
   */
  else?: [StepIR, ...StepIR[]];
  /**
   * Step-level handler hooks; override flow-level ones.
   *
   * @minItems 1
   */
  handlers?: [HandlerBinding, ...HandlerBinding[]];
  /**
   * Canonical hash of "how this step is judged" (02 §12.3).
   */
  judgeHash: string;
  /**
   * Const `"if"`.
   */
  kind: "if";
  /**
   * Pre-entry world probes; double as resume drift detection
   * (spine §6.7-C). Distinct from post-hoc `assertions` (`expect`).
   *
   * @minItems 1
   */
  preflight?: [AssertionIR, ...AssertionIR[]];
  retry?: RetryPolicy5;
  /**
   * Author-provided, flow-unique, stable step identity.
   */
  stepId: string;
  /**
   * Steps executed when the condition holds (≥ 1).
   *
   * @minItems 1
   */
  then: [StepIR, ...StepIR[]];
  /**
   * Step budget in milliseconds.
   */
  timeoutMs?: number;
}
/**
 * Retry policy — applies to the act phase only (spine §6.5 mount 1).
 */
export interface RetryPolicy5 {
  /**
   * Backoff: fixed milliseconds or an exponential schedule.
   */
  backoffMs:
    | number
    | {
        /**
         * Multiplication factor (≥ 1).
         */
        factor: number;
        /**
         * Initial delay in milliseconds (≥ 0).
         */
        initial: number;
        /**
         * Delay ceiling in milliseconds (≥ 0).
         */
        max: number;
      };
  /**
   * Attempt budget (≥ 1).
   */
  maxAttempts: number;
  /**
   * Which error classes are retryable here. Semantically meaningful:
   * `action_failed_retryable`, `target_stale` (forces re-observe), and —
   * for idempotent steps — `action_timed_out`; `check` warns on the rest.
   * Set semantics — serialized in [`ErrorClass`] declaration order.
   *
   * @minItems 1
   */
  retryOn: [ErrorClass, ...ErrorClass[]];
}
/**
 * `kind: "foreach"` — iteration container.
 */
export interface ForeachStepIR {
  /**
   * Iteration variable name (scoped as `iter.<as>`).
   */
  as: string;
  /**
   * Loop body (≥ 1 step).
   *
   * @minItems 1
   */
  body: [StepIR, ...StepIR[]];
  /**
   * Whether to materialize a checkpoint at this step boundary. Required
   * because sealed IR materializes all defaulted fields
   * (single-representation rule; default true, false inside macro
   * expansions).
   */
  checkpoint: boolean;
  /**
   * Canonical hash of "what this step does to the world" (02 §12.3).
   */
  effectHash: string;
  /**
   * Step-level handler hooks; override flow-level ones.
   *
   * @minItems 1
   */
  handlers?: [HandlerBinding, ...HandlerBinding[]];
  /**
   * Expression node: exactly one of `lit` / `ref` / `fn` (02 §8.1).
   *
   * Wire shape is the baseline schema's `oneOf` of three closed single-key
   * objects; the variants are mutually exclusive by their required keys, so
   * serde's untagged representation is deterministic.
   */
  items: LitExpr | RefExpr | FnExpr;
  /**
   * Canonical hash of "how this step is judged" (02 §12.3).
   */
  judgeHash: string;
  /**
   * Const `"foreach"`.
   */
  kind: "foreach";
  /**
   * Pre-entry world probes; double as resume drift detection
   * (spine §6.7-C). Distinct from post-hoc `assertions` (`expect`).
   *
   * @minItems 1
   */
  preflight?: [AssertionIR, ...AssertionIR[]];
  retry?: RetryPolicy6;
  /**
   * Author-provided, flow-unique, stable step identity.
   */
  stepId: string;
  /**
   * Step budget in milliseconds.
   */
  timeoutMs?: number;
}
/**
 * Retry policy — applies to the act phase only (spine §6.5 mount 1).
 */
export interface RetryPolicy6 {
  /**
   * Backoff: fixed milliseconds or an exponential schedule.
   */
  backoffMs:
    | number
    | {
        /**
         * Multiplication factor (≥ 1).
         */
        factor: number;
        /**
         * Initial delay in milliseconds (≥ 0).
         */
        initial: number;
        /**
         * Delay ceiling in milliseconds (≥ 0).
         */
        max: number;
      };
  /**
   * Attempt budget (≥ 1).
   */
  maxAttempts: number;
  /**
   * Which error classes are retryable here. Semantically meaningful:
   * `action_failed_retryable`, `target_stale` (forces re-observe), and —
   * for idempotent steps — `action_timed_out`; `check` warns on the rest.
   * Set semantics — serialized in [`ErrorClass`] declaration order.
   *
   * @minItems 1
   */
  retryOn: [ErrorClass, ...ErrorClass[]];
}
/**
 * `kind: "let"` — pure bindings into `vars.*` (SSA).
 */
export interface LetStepIR {
  /**
   * The bindings (≥ 1 entry).
   */
  bindings: ExprMap3;
  /**
   * Whether to materialize a checkpoint at this step boundary. Required
   * because sealed IR materializes all defaulted fields
   * (single-representation rule; default true, false inside macro
   * expansions).
   */
  checkpoint: boolean;
  /**
   * Canonical hash of "what this step does to the world" (02 §12.3).
   */
  effectHash: string;
  /**
   * Step-level handler hooks; override flow-level ones.
   *
   * @minItems 1
   */
  handlers?: [HandlerBinding, ...HandlerBinding[]];
  /**
   * Canonical hash of "how this step is judged" (02 §12.3).
   */
  judgeHash: string;
  /**
   * Const `"let"`.
   */
  kind: "let";
  /**
   * Pre-entry world probes; double as resume drift detection
   * (spine §6.7-C). Distinct from post-hoc `assertions` (`expect`).
   *
   * @minItems 1
   */
  preflight?: [AssertionIR, ...AssertionIR[]];
  retry?: RetryPolicy7;
  /**
   * Author-provided, flow-unique, stable step identity.
   */
  stepId: string;
  /**
   * Step budget in milliseconds.
   */
  timeoutMs?: number;
}
/**
 * Identifier-keyed map of expressions (exemption class 2: keys are data, constrained by propertyNames).
 */
export interface ExprMap3 {
  [k: string]: Expr;
}
/**
 * Retry policy — applies to the act phase only (spine §6.5 mount 1).
 */
export interface RetryPolicy7 {
  /**
   * Backoff: fixed milliseconds or an exponential schedule.
   */
  backoffMs:
    | number
    | {
        /**
         * Multiplication factor (≥ 1).
         */
        factor: number;
        /**
         * Initial delay in milliseconds (≥ 0).
         */
        initial: number;
        /**
         * Delay ceiling in milliseconds (≥ 0).
         */
        max: number;
      };
  /**
   * Attempt budget (≥ 1).
   */
  maxAttempts: number;
  /**
   * Which error classes are retryable here. Semantically meaningful:
   * `action_failed_retryable`, `target_stale` (forces re-observe), and —
   * for idempotent steps — `action_timed_out`; `check` warns on the rest.
   * Set semantics — serialized in [`ErrorClass`] declaration order.
   *
   * @minItems 1
   */
  retryOn: [ErrorClass, ...ErrorClass[]];
}
/**
 * One declared flow output.
 */
export interface OutputDecl {
  /**
   * Expression node: exactly one of `lit` / `ref` / `fn` (02 §8.1).
   *
   * Wire shape is the baseline schema's `oneOf` of three closed single-key
   * objects; the variants are mutually exclusive by their required keys, so
   * serde's untagged representation is deterministic.
   */
  from: LitExpr | RefExpr | FnExpr;
  /**
   * Output name.
   */
  name: string;
  /**
   * JSON Schema contract of the value.
   */
  schema: {} | boolean;
}
/**
 * One declared flow parameter.
 */
export interface ParamDecl {
  /**
   * Default value (any JSON). Note: an explicit JSON `null` default does
   * not survive a serde round-trip (absence-by-omission rule, 02 §2.4);
   * the compiler never emits one.
   */
  default?: {
    [k: string]: unknown;
  };
  /**
   * Parameter name.
   */
  name: string;
  /**
   * Whether the run must supply this parameter.
   */
  required: boolean;
  /**
   * JSON Schema contract of the value.
   */
  schema: {} | boolean;
}
/**
 * One source-map entry.
 */
export interface SourceMapEntry {
  /**
   * The YAML source file.
   */
  file: string;
  /**
   * RFC 6901 JSON Pointer into this FlowIR document.
   */
  irPath: string;
  /**
   * Macro expansion chain, innermost first. Present iff the IR node was
   * produced by macro expansion — the only structural residue macros
   * leave in the IR (02 §7).
   *
   * @minItems 1
   */
  origin?: [MacroOriginFrame, ...MacroOriginFrame[]];
  span: SourceSpan1;
}
/**
 * One frame of a macro expansion chain.
 */
export interface MacroOriginFrame {
  /**
   * The file containing the expansion site.
   */
  file: string;
  /**
   * The macro's name.
   */
  macro: string;
  span: SourceSpan;
}
/**
 * The span of the expansion site.
 */
export interface SourceSpan {
  /**
   * End column (1-based).
   */
  endCol: number;
  /**
   * End line (1-based).
   */
  endLine: number;
  /**
   * Start column (1-based).
   */
  startCol: number;
  /**
   * Start line (1-based).
   */
  startLine: number;
}
/**
 * The source span.
 */
export interface SourceSpan1 {
  /**
   * End column (1-based).
   */
  endCol: number;
  /**
   * End line (1-based).
   */
  endLine: number;
  /**
   * Start column (1-based).
   */
  startCol: number;
  /**
   * Start line (1-based).
   */
  startLine: number;
}
/**
 * Content-pinned reference to a compiled flow artifact.
 */
export interface FlowRef2 {
  /**
   * The callee's flow id.
   */
  flowId: string;
  /**
   * The callee's content hash (integrity pin; runner verifies on load).
   */
  irHash: string;
}
