//! `AtomDefinition` type.
//!
//! An "atom" is the unit of composition for the deterministic composer
//! (plan §3.7 architecture overview). Each atom is one YAML file under
//! `config/stage-atoms/` declaring exactly one `(operation × input-type
//! × output-type)` triple. Atoms map to stages in the existing builder
//! (`crates/core/src/builder.rs`) via a thin adapter introduced in
//! S6.12 — until then, the type is a parallel-shipping schema that
//! existing taxonomy YAMLs can reference (S4.5) without breaking the
//! current code path.
//!
//! # Architectural lineage
//!
//! Atoms are conceptually closest to **Bazel subrules** — composable
//! building blocks with explicit interface contracts. The rest of the
//! lineage is documented in CLAUDE.md and `prompt_role.txt`:
//!
//! - **Composer + harness ≈ WINGS + Pegasus** (Gil et al., USC/ISI 2007 —
//!   workflow-template specialization + execution-planning split).
//! - **`discover_*` ≈ BETSY introspection modules** (Bioinformatics 2017 —
//!   runtime method selection from typed-attribute key-value model).
//! - **Provenance ≈ Workflow Run RO-Crate Tier-3** (Provenance Run Crate;
//!   W3C PROV-O alignment via SKOS).
//!
//! # Strict-superset relationship to `StageSpec`
//!
//! `AtomDefinition` is intentionally a strict superset of the existing
//! `StageSpec` shape. The type carries its full field set and
//! `builder.rs::emit_stage` accepts either via a thin adapter. Future
//! schema changes are additive.
//!
//! # Determinism
//!
//! Every collection field uses `BTreeMap` / `Vec` (not `HashMap` /
//! `HashSet`) so YAML round-trips are byte-identical. The atom_registry
//! loader (`crates/core/src/atom_registry.rs`) sorts atom files by id
//! before yielding, preserving ordering across runs.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// One atom — the unit the composer reasons over. Loaded from
/// `config/stage-atoms/<id>.yaml` files; validated against
/// `_atom.schema.json` at load time per plan §S4.3.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct AtomDefinition {
    /// Unique atom id. Stable across reorganisations of the atom
    /// library — the composer uses this as the cache key. Convention
    /// is `<verb>_<noun>` (e.g. `align_reads`, `quantify_features`,
    /// `discover_normalisation_method`).
    pub id: String,

    /// Semver version. Stored as a free-form string; not currently
    /// validated at load time. Future work will gate duplicate ids
    /// with different versions in `validate_consistency`.
    pub version: String,

    /// What kind of step this atom represents. Drives builder-side
    /// expansion: `Operation` becomes a single execute task,
    /// `Discovery` becomes a `discover_*` task that the agent
    /// resolves at runtime, `Validation` becomes a `validate_*`
    /// wrapper around the upstream operation, `Aggregator` becomes a
    /// fan-in barrier.
    pub role: AtomRole,

    /// Required when `role == Discovery`. Names the kind of decision
    /// the agent makes (e.g. `method`, `threshold`, `panel`). Schema
    /// enforces presence; the builder reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub discovery_kind: Option<String>,

    /// Free-form English description. Surfaces in CONTEXT.md +
    /// agent prompts; never machine-parsed.
    pub description: String,

    /// EDAM operation IRI (e.g. `operation:0292` for Alignment). Use
    /// `swfc:<slug>` for in-house extensions per ADR 0004 (e.g.
    /// `swfc:scrnaseq_doublet_detection`). The schema regex enforces
    /// the pattern; the curated subtype edges in
    /// `crates/core/src/edam.rs` (S4.8) ground "is-a" lookups.
    pub edam_operation: String,

    /// EDAM data class IRI for the primary input shape (e.g.
    /// `data:1383` for "Sequence assembly"). Required for
    /// `Operation` atoms; `None` for `Aggregator` (input shapes are
    /// resolved at fan-in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub edam_data: Option<String>,

    /// EDAM format IRI for the primary committed artifact (e.g.
    /// `format:2572` for BAM, `format:3475` for tabular text). Used
    /// by the cross-version diff to align "same kind" outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub edam_format: Option<String>,

    /// Who runs this atom: an agent invocation or a human (SME)
    /// decision. The schema constrains to `agent` | `sme`.
    pub assignee: AtomAssignee,

    /// Atoms this atom requires output from. Ids are other atom ids
    /// — the composer flattens the dependency graph to a DAG. Empty
    /// for terminal "leaf" atoms whose inputs come from intake.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Atoms that are mutually-exclusive with this one. The composer
    /// rejects compositions that include conflicting atoms (e.g. a
    /// pair of competing alignment methods when only one should win).
    /// Accepts literal id lists today.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excludes: Vec<String>,

    /// Free-form attribute map the composer reads to pick between
    /// alternatives. Examples: `{"speed": "fast", "reference_free":
    /// true, "memory_gb": 16}`. The discover_* runtime selector
    /// reads compatible attribute subsets.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(type = "Record<string, unknown>")]
    pub attributes: BTreeMap<String, serde_json::Value>,

    /// Multi-modal joint-source constraints. Each
    /// entry declares that two of this atom's upstream dependencies
    /// must share the same `attributes.source_atom` value (i.e.
    /// originate from the same upstream sample / batch / dataset).
    /// Composer post-validates at compose time and rejects when
    /// the producers' `source_atom` attributes diverge.
    ///
    /// Example: a multi-modal integration atom that consumes both
    /// scRNA-seq counts and CITE-seq protein expression must
    /// declare `joint_with: [{lhs: "scrnaseq_counts", rhs:
    /// "citeseq_protein"}]` so the composer rejects a DAG that
    /// pairs counts from sample A with protein from sample B.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joint_with: Vec<JointlyWithConstraint>,

    /// Explicit rich
    /// input ports. Optional and additive: when absent, the
    /// `compatibility::engine` synthesizes coarse ports from
    /// `edam_data` / `edam_format` so legacy atoms keep working.
    /// A future migration retires the legacy path once every atom
    /// declares its inputs/outputs. The high-risk atom families
    /// (FASTQ, BAM/CRAM, VCF/BCF, count matrix, single-cell object)
    /// migrate first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<crate::workflow_contracts::port::PortContract>,

    /// Explicit rich
    /// output ports. See `inputs` for the migration story.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<crate::workflow_contracts::port::PortContract>,

    /// Pointer for runtime method selection. When set, the named
    /// `discovery_*` atom (or stage in legacy taxonomies) carries
    /// the method choice — the composer threads `method_choice` from
    /// the discovery's runtime output into this atom's invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub method_choice: Option<MethodChoiceRef>,

    /// Resource hint for sizing. Composer aggregates these into
    /// `ResourceEstimate` for the SME cost-preview surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resource_profile: Option<ResourceProfile>,

    /// Per-atom container pin (plan §S15.1 / S15.23). Composer
    /// threads this through to `Task::container` (S15.2) so
    /// `WORKFLOW.json` carries per-task pinning. The schema records
    /// the field; the harness resolves image-digest at emit time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub preferred_container: Option<ContainerSpec>,

    /// `claim_boundary` directive the LLM must restate during
    /// confirmation. Carried verbatim into the `claimBoundary`
    /// section of the emitted package's interpretation policy when
    /// the atom is part of an interpretation slice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub claim_boundary: Option<String>,

    /// Runtime-expanded iteration block. Required when
    /// the atom's compiled stage carries `StageCardinality::IterateUntil`;
    /// the builder emits a 4-template-task scaffold per S10.3 and
    /// the agent fans out a linear chain of `<id>_iter_N` tasks until
    /// the convergence metric drops below
    /// `convergence.threshold` for `convergence.consecutive_iterations`
    /// consecutive passes. `None` on every non-iterating atom — the
    /// schema validator (S10.2) gates `Some` on `cardinality ==
    /// iterate_until` so authoring slips fail at registry-load time
    /// rather than at compose time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub iterate: Option<IterateSpec>,

    /// Atom-level CEL gate. The
    /// composer adapter (`composed_atom_to_stage_spec`) threads
    /// this onto `StageSpec::condition`, which the builder's
    /// `propagate_readiness` evaluates at runtime against the
    /// upstream task results. When the expression returns
    /// `false`, the task is short-circuited to
    /// `TaskState::Skipped` with no agent dispatch.
    ///
    /// Example: `condition:
    /// "discover_batch_correction.result.batch_correction_required == true"`
    /// gates a `batch_correction` atom on the upstream
    /// discovery's verdict. Composer doesn't compile the
    /// expression; the builder's CEL interpreter
    /// (`crate::expression::ExpressionEvaluator`) does, at
    /// runtime.
    ///
    /// `None` (default) means the atom always runs when its
    /// dependencies complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub condition: Option<String>,

    /// Figure ids the atom must produce under
    /// `runtime/outputs/<stage_id>/figures/` when it is emitted as a
    /// compute task. This mirrors `StageSpec::required_figures` so the
    /// composer/archetype path preserves the same plotting contract as
    /// legacy taxonomy YAMLs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_figures: Vec<String>,

    /// Optional plotting module id when the emitted stage id is an alias
    /// or uses different spelling than the shared plotting module. For
    /// example, the `normalisation` atom renders via the
    /// `runtime.plotting.stages.normalization` module, and aliased
    /// cross-omics stages render via their underlying atom module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plot_stage_id: Option<String>,

    /// When set, this atom is exempt from the figure-obligation lint.
    /// Use only for adapters or atoms whose output is structurally
    /// non-plottable (e.g. sort/index adapters, intermediate data
    /// products that are consumed by a downstream compute stage, or
    /// pipeline-control atoms that never produce user-facing artifacts).
    /// The reason is surfaced in provenance; category may be one of
    /// `adapter`, `intermediate`, `non_plottable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub figure_exempt: Option<FigureExempt>,

    /// Headline output filenames the agent should produce under the task
    /// output directory. Mirrors `StageSpec::expected_artifacts`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_artifacts: Vec<String>,

    /// Required artifacts the harness verifies before accepting a
    /// completed task. Mirrors `StageSpec::required_artifacts`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(type = "Array<{ path: string; min_size_bytes?: number; schema_ref?: string }>")]
    pub required_artifacts: Vec<crate::taxonomy::RequiredArtifactSpec>,

    /// Validation-obligation ids that the harness
    /// runs against this atom's artifacts after task completion.
    /// Each id resolves against the runner registry
    /// (`ValidatorRegistry`); typical values are
    /// `p_value_in_unit_interval`, `gene_id_in_annotation`,
    /// `coordinate_in_contig`,
    /// `barcode_matrix_dim_consistency`,
    /// `no_train_test_leakage`, `deterministic_or_bounded_variance`.
    /// Empty default keeps legacy atoms unchanged. Populated to
    /// `RequiredArtifact.validation_obligations` by both the v4
    /// lowering pass (via `TaskNode.validators`) and the legacy
    /// `build_dag_from_composition` path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validators: Vec<String>,

    /// Derived-image warm-up: system + language packages
    /// this atom depends on at runtime. The emitter aggregates the
    /// union of every reachable atom's declarations into
    /// `policies/runtime-prereqs.json`; the harness pre-flight uses
    /// that manifest to derive a content-addressed image before the
    /// first iteration. At the atom level, only `system_packages`,
    /// `language_packages`, and `system_check` are meaningful — the
    /// `base_image` and `modality` fields belong on the
    /// archetype/taxonomy and are ignored here. Empty default keeps
    /// every legacy atom unaffected.
    #[serde(default, skip_serializing_if = "is_empty_runtime_prereqs")]
    pub runtime_packages: crate::runtime_prereqs::RuntimePrereqs,

    /// Unifying safety classification. Composes with the
    /// fine-grained `crate::sandbox_policy::SandboxPolicy` when
    /// `sandbox != None`.
    #[serde(default, skip_serializing_if = "SafetyPolicy::is_default")]
    pub safety: SafetyPolicy,
}

impl AtomDefinition {
    /// Construct a minimal valid `AtomDefinition` for tests and Tier F
    /// property generators. All optional fields default to their
    /// empty/`None`/default form; `role` is `Operation`, `assignee` is
    /// `Agent`, EDAM ids point at the "Alignment" + "Sequence
    /// assembly" placeholders. Used by
    /// `crates/eval-adapters/src/property/atom.rs`; production paths
    /// hand-author the full struct via the YAML registry.
    pub fn test_default(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            description: format!("Atom {id}"),
            id,
            version: "1.0.0".into(),
            role: AtomRole::Operation,
            discovery_kind: None,
            edam_operation: "operation:0292".into(),
            edam_data: Some("data:1383".into()),
            edam_format: Some("format:2572".into()),
            assignee: AtomAssignee::Agent,
            depends_on: Vec::new(),
            excludes: Vec::new(),
            attributes: BTreeMap::new(),
            joint_with: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            method_choice: None,
            resource_profile: None,
            preferred_container: None,
            claim_boundary: None,
            iterate: None,
            condition: None,
            required_figures: Vec::new(),
            plot_stage_id: None,
            figure_exempt: None,
            expected_artifacts: Vec::new(),
            required_artifacts: Vec::new(),
            validators: Vec::new(),
            runtime_packages: crate::runtime_prereqs::RuntimePrereqs::default(),
            safety: SafetyPolicy::default(),
        }
    }
}

/// `skip_serializing_if` predicate for the atom-level
/// `runtime_packages` field. We treat a default-constructed manifest
/// (schema_version=1, all collections empty) as "absent" so atom
/// YAMLs that don't declare anything serialize identically across
/// versions.
fn is_empty_runtime_prereqs(p: &crate::runtime_prereqs::RuntimePrereqs) -> bool {
    p.system_packages.is_empty()
        && p.language_packages.is_empty()
        && p.system_check.is_empty()
        && p.base_image.is_none()
        && p.modality.is_none()
}

/// Convergence + cap config for `StageCardinality::IterateUntil`.
///
/// Mirrors the K8s reconciliation-loop pattern (Q5.1 Part 2): a
/// max-iterations ceiling, a min-iterations floor, and a convergence
/// rule that the agent evaluates each pass against a per-atom metric.
/// Composer treats iterate atoms as single nodes when ordering;
/// runtime expansion happens in the agent (S10.4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct IterateSpec {
    /// Hard ceiling on iterations. The agent flips to
    /// `BlockerKind::IterationDidNotConverge` when reached without
    /// convergence. Schema enforces `> 0`.
    pub max_iterations: u32,

    /// Lower bound — useful when the convergence metric is noisy at
    /// startup and a single early-pass match would underfit. Default
    /// 1 (no floor beyond the implicit one); the schema rejects
    /// `min_iterations > max_iterations`.
    #[serde(default = "default_min_iterations")]
    pub min_iterations: u32,

    /// Convergence rule the agent re-evaluates each pass.
    pub convergence: IterateConvergence,

    /// What to do when `max_iterations` is reached without
    /// convergence. Default `Block` surfaces
    /// `BlockerKind::IterationDidNotConverge` so the SME picks
    /// (raise threshold / accept best so far / abort).
    #[serde(default)]
    pub on_max_iterations: IterateMaxAction,

    /// Optional CEL expression naming the field in
    /// `runtime/outputs/<task>/result.json` that ranks iterations
    /// when the SME picks "accept best so far" — without it, the
    /// last iteration is the default. Composer doesn't compile
    /// this; the agent does, at iteration time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub best_selector: Option<String>,
}

/// Convergence rule. The agent reads the metric value
/// from the iteration's result.json, compares against `threshold`
/// using `operator`, and tracks consecutive passes that satisfy the
/// rule. Convergence fires when the consecutive count crosses
/// `consecutive_iterations`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct IterateConvergence {
    /// CEL expression naming the metric field. Examples:
    /// `result.silhouette`, `result.gradient_norm`,
    /// `result.adjusted_rand`. The agent compiles this with
    /// `cel-interpreter` v0.13.0 (S7.1) and evaluates against the
    /// per-iteration result.json.
    pub metric_source: String,

    /// Comparison operator. `Lt` is the typical "loss decreasing"
    /// rule; `Gt` for metrics that should rise (silhouette);
    /// `LtEq` / `GtEq` mirror.
    pub operator: IterateConvergenceOp,

    /// Numeric threshold the metric is compared against.
    pub threshold: f64,

    /// How many *consecutive* iterations must satisfy the rule
    /// before declaring convergence. Default 2 (one stable pass
    /// rules out a stochastic dip). Schema enforces `>= 1`.
    #[serde(default = "default_consecutive_iterations")]
    pub consecutive_iterations: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// IterateConvergenceOp discriminant.
pub enum IterateConvergenceOp {
    /// Lt variant.
    Lt,
    /// LtEq variant.
    LtEq,
    /// Gt variant.
    Gt,
    /// GtEq variant.
    GtEq,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// IterateMaxAction discriminant.
pub enum IterateMaxAction {
    /// Surface `BlockerKind::IterationDidNotConverge` and let the
    /// SME decide. Default — the safe call when convergence is the
    /// quality gate (clustering stability, gradient descent).
    #[default]
    Block,
    /// Accept the best iteration per `best_selector` (or the last
    /// one if unset). The validator that follows the iterate atom
    /// (`validate_<id>`) still runs — convergence isn't validation,
    /// they're orthogonal.
    AcceptBest,
    /// Mark the iterate atom failed and abort the downstream slice.
    /// Useful when downstream contracts are too tight to tolerate
    /// non-converged iteration output.
    Abort,
}

fn default_min_iterations() -> u32 {
    1
}

fn default_consecutive_iterations() -> u32 {
    2
}

/// Extended attribute-type discriminator the composer
/// uses when validating atom-attribute compatibility along an edge.
/// Today's `attributes: BTreeMap<String, serde_json::Value>` carries
/// raw JSON values; the composer's attribute-resolution pass
/// (S7.4 formal validation) classifies each value into one of the
/// four `AttributeType` shapes below to decide whether downstream
/// atoms can consume it. The classifier is `AttributeType::classify`;
/// it stays in `crates/core` because the composer is sync.
///
/// JSON Schema subset (intentionally narrow — the composer is not a
/// schema validator):
///
/// - `Record { properties }` — JSON object with named typed fields.
///   Maps to `{"type": "object", "properties": {...}}` in JSON
///   Schema. The composer matches subset-of-fields downstream.
/// - `Array { items }` — JSON array with homogeneous element type.
///   Maps to `{"type": "array", "items": <T>}`. Lists with mixed
///   element types fall back to `Unknown` rather than `union` over
///   the element types — the composer treats `Array<Unknown>` as
///   opaque so the cost of re-resolving an element-type union
///   stays out of the per-edge fast path.
/// - `Union { variants }` — discriminated by JSON Schema `oneOf`/
///   `anyOf`. Order-stable so the composer's tie-break is
///   deterministic.
/// - `Unknown` — fallback for any value the classifier can't
///   resolve cleanly (mixed-type arrays, free-form text, raw
///   binary blobs). Composer emits a warning but does not block
///   the composition — the runtime agent inspects the value at
///   dispatch time. This bucket is intentionally wide so
///   introducing a new attribute shape doesn't require a composer
///   rev; instead, the new shape lands as a richer classifier
///   case in a follow-up that splits `Unknown` further.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeType {
    /// A JSON object with named typed fields. Carries the field
    /// names + their classified types so the composer can do
    /// subset-of-fields matching across an edge.
    Record {
        /// Properties.
        properties: BTreeMap<String, AttributeType>,
    },
    /// A homogeneous JSON array. Element type carried so the
    /// composer can match `Array<Record<...>>` shapes cleanly.
    Array { items: Box<AttributeType> },
    /// `oneOf`/`anyOf` union. Order is stable for determinism.
    Union { variants: Vec<AttributeType> },
    /// Fallback for raw scalars (string/number/bool/null), mixed-
    /// type arrays, or any shape the classifier can't resolve.
    Unknown,
}

impl AttributeType {
    /// Classify a `serde_json::Value` into one of the four typed
    /// shapes. Recursive on `Record` + `Array` + `Union`; flat for
    /// scalars. Determinism: object keys are walked in
    /// `serde_json::Value::Object` insertion order, which is
    /// `BTreeMap`-shaped under the hood for our serde config. If a
    /// future serde feature introduces non-deterministic walk
    /// order we'd switch to a manual sort here.
    pub fn classify(value: &serde_json::Value) -> Self {
        match value {
            serde_json::Value::Object(obj) => {
                let properties = obj
                    .iter()
                    .map(|(k, v)| (k.clone(), Self::classify(v)))
                    .collect();
                Self::Record { properties }
            }
            serde_json::Value::Array(arr) => {
                if arr.is_empty() {
                    // Empty array — element type undecidable; bucket
                    // as Unknown rather than guess.
                    return Self::Unknown;
                }
                // Classify the first element; if every element
                // classifies the same we keep the type, otherwise
                // bucket the array as Unknown (mixed-type arrays
                // fall back per the YAGNI choice in the doc above).
                let first = Self::classify(&arr[0]);
                if arr.iter().skip(1).all(|v| Self::classify(v) == first) {
                    Self::Array {
                        items: Box::new(first),
                    }
                } else {
                    Self::Unknown
                }
            }
            // Scalars (string/number/bool/null) are intentionally
            // bucketed as Unknown until the composer needs to
            // distinguish them. Today's edges match by `edam_data`
            // + `edam_format` IRIs, not raw scalar types — that
            // discrimination layer is the right place for the
            // expansion when it lands.
            _ => Self::Unknown,
        }
    }

    /// Does `producer` satisfy `consumer`? The composer
    /// uses this on every `depends_on` edge during the attribute-
    /// resolution pass. Subset-of-fields semantics: a `Record { a, b
    /// }` producer satisfies a `Record { a }` consumer; the
    /// inverse is rejected. `Array<T>` requires element types to
    /// satisfy. `Union` requires the producer to satisfy at least
    /// one variant.
    pub fn satisfies(producer: &Self, consumer: &Self) -> bool {
        match (producer, consumer) {
            (_, Self::Unknown) => true,
            (Self::Record { properties: p }, Self::Record { properties: c }) => c
                .iter()
                .all(|(k, ct)| p.get(k).map(|pt| Self::satisfies(pt, ct)).unwrap_or(false)),
            (Self::Array { items: p }, Self::Array { items: c }) => Self::satisfies(p, c),
            (Self::Union { variants: pv }, c) => pv.iter().any(|p| Self::satisfies(p, c)),
            (p, Self::Union { variants: cv }) => cv.iter().any(|c| Self::satisfies(p, c)),
            _ => false,
        }
    }
}

/// What kind of step an atom represents. Drives builder-side
/// expansion semantics.
///
/// Extended with `Selection` (atoms whose old filename
/// prefix was `select_`) and four speculative variants
/// (`Calibration`, `Pilot`, `Adversarial`, `Monitor`). New variants
/// default to `Operation`-equivalent behavior at consumer sites
/// until specialized — the [`default_behavior_class`] helper
/// returns one of `{Operation, Discovery, Validation, Aggregator,
/// Selection}` so consumers branch on a 5-arm match instead of a
/// 9-arm match.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum AtomRole {
    /// Single execute task (e.g. align reads, quantify features).
    #[default]
    Operation,
    /// `discover_*` task — the agent picks a method at runtime from
    /// a typed key-value attribute set. `discovery_kind` must be
    /// `Some(_)` when this variant is used.
    Discovery,
    /// `validate_*` wrapper around an upstream operation (e.g.
    /// `validate_alignment` checks mapping rates).
    Validation,
    /// Fan-in barrier that aggregates multiple upstream outputs into
    /// a single typed shape (e.g. concatenate per-sample count
    /// matrices into a study-level matrix).
    Aggregator,
    /// Pilot sizing atom — runs a tiny representative sample to
    /// project resources for the full run. Future-direction
    /// proposal per plan §3.3 (`AtomRole::Sizing`); v1 atoms can
    /// already use it but the builder treats it as `Operation` until
    /// S15 wires the pilot lifecycle to it explicitly.
    Sizing,
    /// `select_*` selection atoms (was a filename
    /// prefix). The agent picks one option from a closed candidate
    /// set (e.g. `select_sensitivity_winner`). Distinct from
    /// `Discovery` (open method selection) in that the candidate set
    /// is enumerated upstream, not discovered.
    Selection,
    /// Calibration atoms that fit
    /// per-modality / per-experiment constants from upstream data
    /// (e.g. negative-control batch effect models). Default behavior
    /// is `Operation`-equivalent; specialized consumers may treat
    /// the output as a typed parameter source.
    Calibration,
    /// Pilot-run atoms that execute a
    /// truncated version of a downstream operation to project
    /// resources. Distinct from `Sizing` in that pilots may run on
    /// real (subset) data rather than synthetic. Default behavior
    /// is `Operation`-equivalent.
    Pilot,
    /// Adversarial-test atoms that probe
    /// pipeline robustness with deliberately-perturbed inputs. Default
    /// behavior is `Operation`-equivalent; specialized consumers may
    /// route their output into the validation contract.
    Adversarial,
    /// Monitor atoms that emit metrics or
    /// alerts as a side-effect of pipeline execution rather than
    /// producing a typed artifact. Default behavior is
    /// `Operation`-equivalent.
    Monitor,
}

impl AtomRole {
    /// Collapses the 9-variant `AtomRole` into the
    /// 5-variant behavior class consumers actually branch on. New
    /// speculative variants (`Calibration`, `Pilot`, `Adversarial`,
    /// `Monitor`) and `Sizing` default to `Operation` here so
    /// existing consumers don't need to add new arms until a real
    /// specialization appears.
    ///
    /// Return values are restricted to the five "load-bearing"
    /// roles: `Operation`, `Discovery`, `Validation`, `Aggregator`,
    /// `Selection`. Consumers should branch on the result of this
    /// helper rather than the raw enum unless they have a real
    /// reason to specialize on the speculative variants.
    pub fn default_behavior_class(self) -> AtomRole {
        match self {
            AtomRole::Operation => AtomRole::Operation,
            AtomRole::Discovery => AtomRole::Discovery,
            AtomRole::Validation => AtomRole::Validation,
            AtomRole::Aggregator => AtomRole::Aggregator,
            AtomRole::Selection => AtomRole::Selection,
            // Speculative variants + Sizing fall through to
            // `Operation`-equivalent behavior.
            AtomRole::Sizing
            | AtomRole::Calibration
            | AtomRole::Pilot
            | AtomRole::Adversarial
            | AtomRole::Monitor => AtomRole::Operation,
        }
    }

    /// Convenience predicates so consumers don't have to import the
    /// `AtomRole` enum just to ask "is this a discovery stage?".
    pub fn is_discovery(self) -> bool {
        matches!(self.default_behavior_class(), AtomRole::Discovery)
    }
    /// Is validation.
    pub fn is_validation(self) -> bool {
        matches!(self.default_behavior_class(), AtomRole::Validation)
    }
    /// Is aggregator.
    pub fn is_aggregator(self) -> bool {
        matches!(self.default_behavior_class(), AtomRole::Aggregator)
    }
    /// Is selection.
    pub fn is_selection(self) -> bool {
        matches!(self.default_behavior_class(), AtomRole::Selection)
    }
    /// Is operation.
    pub fn is_operation(self) -> bool {
        matches!(self.default_behavior_class(), AtomRole::Operation)
    }
}

/// Who runs an atom. The closed enum is the schema-allowlist for the
/// `assignee` field per plan §S4.3.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum AtomAssignee {
    /// Agent variant.
    Agent,
    /// Sme variant.
    Sme,
}

/// Pointer to a discovery atom whose runtime output supplies the
/// method-choice for this atom. Replaces the today's id-prefix
/// convention (`discover_<x>` → `<x>`) per plan §3.3.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct MethodChoiceRef {
    /// Atom id of the discovery step that produces the choice.
    pub deferred_to: String,
}

/// Multi-modal joint-source constraint between two
/// upstream dependencies of an atom. The composer's post-validation
/// asserts both `lhs` and `rhs` producers carry an
/// `attributes.source_atom` field with the same value. Used by
/// integration atoms (scRNA-seq + CITE-seq from the same sample,
/// bulk RNA-seq + ATAC-seq from the same arm) to prevent pairing
/// outputs from different upstream samples / batches.
///
/// Both `lhs` and `rhs` are atom ids that appear in the atom's
/// `depends_on` list. When the composer cannot identify the
/// producer of either (atom not in registry or absent from the
/// composition), it surfaces a typed `CompositionError` rather
/// than silently dropping the constraint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct JointlyWithConstraint {
    /// First atom id that must share `attributes.source_atom`.
    pub lhs: String,
    /// Second atom id that must share `attributes.source_atom`.
    pub rhs: String,
}

/// Resource hint for the composer's `ResourceEstimate` aggregator
/// (plan §3.1 row "ResourceEstimate computed from atom
/// resource_profile"). Coarse buckets — exact sizing happens via the
/// existing `compute-profiles/` machinery + pilot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ResourceProfile {
    /// Coarse CPU bucket (e.g. `light`, `moderate`, `heavy`,
    /// `very_heavy`). Maps to per-stage profiles in
    /// `config/compute-profiles/profiles.yaml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cpu: Option<String>,
    /// Coarse memory bucket (`small`, `medium`, `large`, `xl`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub memory: Option<String>,
    /// True when the atom needs a GPU.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gpu: bool,
    /// Coarse runtime bucket in human-readable form (e.g. `seconds`,
    /// `minutes`, `hours`). Pilot replaces these with concrete
    /// projections at emit time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub runtime_class: Option<String>,
}

/// Per-atom container pin. Composer threads
/// this through to `Task::container` so `WORKFLOW.json` is the
/// reproducibility-bearing source of truth for which image ran each
/// task. Image-digest resolution happens at emit time per ADR 0025.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ContainerSpec {
    /// Image reference (e.g. `ghcr.io/scripps/scripps-bio-base`).
    pub image: String,
    /// Tag (semver-like). Pinned to a specific release; composer
    /// resolves `:latest` style refs to a digest at emit time.
    pub tag: String,
    /// SHA-256 digest, populated at emit time per ADR 0025. v1
    /// schema accepts an empty string here when the atom YAML
    /// declares only image + tag; the emitter rewrites it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub digest: String,
    /// Architecture allowlist. Default `["amd64"]` per plan §3.6
    /// non-goal #22 ("Not building cross-arch images by default").
    #[serde(default = "default_arch", skip_serializing_if = "is_default_arch")]
    pub arch: Vec<String>,
    /// True when the image needs GPU passthrough (`--nv` /
    /// `--gpus=all`). Cross-checks with `resource_profile.gpu`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gpu_required: bool,
    /// Typed network policy. `Bridge` = inherit harness default +
    /// harness's own bridge interface; `None { allowlist }` =
    /// `--network=none` plus iptables / apptainer network-args
    /// resolving the allowlist hostnames at task launch (DNS rotation
    /// safety). Default `None` = inherit (atom YAML left the field
    /// unset).
    ///
    /// Deprecated: the canonical location is
    /// `AtomDefinition.safety.network`. The field is retained on
    /// `ContainerSpec` for back-compat YAML deserialization + the
    /// `ContainerNetworkOverride` lint; new atoms must declare
    /// network policy under `safety.network` only.
    #[deprecated(
        since = "0.1.0",
        note = "Plan §A.S6: ContainerSpec.network is moved to \
                AtomDefinition.safety.network."
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub network: Option<NetworkPolicy>,
    /// Image source. `Image` = pull from registry directly using
    /// `image:tag`; `Conda` = on-demand build via Wave/Seqera
    /// resolver materializing per-tool images from conda-lock;
    /// `Host` = run on the bare host (no container). Default
    /// `Image` for back-compat with atoms whose YAML only declared
    /// `image` + `tag`.
    #[serde(default, skip_serializing_if = "ContainerSource::is_default")]
    pub source: ContainerSource,
}

// `NetworkPolicy` lives in `crates/ecaa-types/src/atom.rs` so the
// canonical `BlockerKind` binding can stand alone without pulling
// the rest of this file. Re-exported below from the v0.1 spec crate.
pub use ecaa_workflow_types::atom::NetworkPolicy;

/// Typed `ContainerSpec.source` discriminator. `Image` is the historical
/// default and matches the pre-S15.1 wire shape (atom YAMLs that only
/// declared `image:` + `tag:` still deserialize as
/// `ContainerSource::Image`). `Host` opts back to bare-host execution
/// (no container).
#[derive(
    Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContainerSource {
    /// Pull `image:tag` from a registry directly. Default.
    #[default]
    Image,
    /// On-demand build via Wave/Seqera (S15.13). `conda_packages`
    /// keys are conda channels (e.g. `conda-forge`, `bioconda`)
    /// and values are package specs (e.g. `r-seurat=5.x`).
    Conda {
        #[serde(default)]
        /// Conda packages.
        conda_packages: BTreeMap<String, Vec<String>>,
    },
    /// Run on the bare host. No container, no SBOM emission. Used
    /// only when an atom's deps are already validated to live on
    /// the host and we want the smaller blast radius.
    Host,
}

impl ContainerSource {
    fn is_default(&self) -> bool {
        matches!(self, ContainerSource::Image)
    }
}

/// Explicit figure-obligation lint exemption. When present on an
/// `AtomDefinition`, the lint skips the atom rather than flagging it
/// as a violation. Use only for atoms that genuinely cannot or should
/// not produce user-facing figures:
///
/// - `category: "adapter"` — sort/index/compression adapters that
///   transform a format without producing a new data product.
/// - `category: "intermediate"` — atoms whose output is immediately
///   consumed by a downstream compute stage; the downstream stage
///   carries the figure obligation.
/// - `category: "non_plottable"` — atoms that produce metadata,
///   manifests, or control signals (e.g. sample-sheet generators,
///   schema converters) with no meaningful visual representation.
///
/// The `reason` field is surfaced in RO-Crate provenance and in
/// the figure-obligation lint report so catalog reviewers can audit
/// exemptions at a glance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct FigureExempt {
    /// Human-readable justification for the exemption. Required.
    pub reason: String,
    /// Optional category — `adapter`, `intermediate`, `non_plottable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub category: Option<String>,
}

fn default_arch() -> Vec<String> {
    vec!["amd64".into()]
}

fn is_default_arch(v: &[String]) -> bool {
    v == ["amd64"]
}

/// Headline safety classification per AtomDefinition.
/// Inspired by open-rosalind's SkillMetadata.safety_level.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum SafetyLevel {
    /// No IO, no network, no code execution. Read-only / pure compute.
    Safe,
    /// External network egress to an explicit allowlist or via Bridge.
    Network,
    /// Heavy computation, vetted container, no network, no untrusted
    /// code. The dominant case in scripps-workflow. Default.
    #[default]
    Compute,
    /// Executes generated/dynamic code at runtime. Requires sandbox.
    Exec,
}

/// What kind of code runs inside the atom's container at runtime.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum CodeExecution {
    /// Container entrypoint only.
    #[default]
    None,
    /// Vendored scripts (deterministic) from config/ or compile-time
    /// generation.
    Vetted,
    /// Scripts the agent generates at runtime. Sandbox required.
    GeneratedByAgent,
}

// `SandboxRequirement` lives in `crates/ecaa-types/src/atom.rs` so
// the canonical `BlockerKind` binding can stand alone without pulling
// the rest of this file. Re-exported below from the v0.1 spec crate.
pub use ecaa_workflow_types::atom::SandboxRequirement;

/// Runtime package-install policy.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ProvisioningPolicy {
    /// Image is sealed. Agent's install commands fail at the proxy.
    Sealed,
    /// Agent may install only packages declared in atom.runtime_packages
    /// (or aggregated up the archetype). Default.
    #[default]
    DeclaredOnly,
    /// Agent may install from allowlisted registries. Requires Exec.
    Allowlisted,
}

/// Unifying safety policy on AtomDefinition. Composes with the
/// fine-grained `crate::sandbox_policy::SandboxPolicy` — SafetyPolicy
/// is the coarse atom-level declaration; SandboxPolicy is the
/// implementation-level enforcement that kicks in when sandbox != None.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct SafetyPolicy {
    #[serde(default)]
    /// Level.
    pub level: SafetyLevel,
    /// Moved from ContainerSpec.network. Default is the "deny all
    /// egress" form (None with empty allowlist) — required for
    /// Compute / Safe level lint consistency. See design §4.2.
    #[serde(default = "default_safety_network")]
    pub network: NetworkPolicy,
    #[serde(default)]
    /// Code execution.
    pub code_execution: CodeExecution,
    #[serde(default)]
    /// Sandbox.
    pub sandbox: SandboxRequirement,
    #[serde(default)]
    /// Provisioning.
    pub provisioning: ProvisioningPolicy,
    /// When true this task operates on controlled-access data (e.g.
    /// dbGaP-restricted genotypes) that must not be forwarded to a
    /// third-party LLM inference endpoint. The harness dispatch gate
    /// checks this flag before launching the Claude agent wrapper and
    /// emits `BlockerKind::ControlledAccessViolation` if the executor
    /// would route the data through an external LLM. Default `false`
    /// keeps existing WORKFLOW.json files backward-compatible.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub controlled_access: bool,
}

impl Default for SafetyPolicy {
    fn default() -> Self {
        Self {
            level: SafetyLevel::Compute,
            // Compute level requires no egress; override the
            // NetworkPolicy::Bridge default here so the default policy
            // lint-passes (see Task 2.1 cross-check rules).
            network: NetworkPolicy::None { allowlist: vec![] },
            code_execution: CodeExecution::None,
            sandbox: SandboxRequirement::None,
            provisioning: ProvisioningPolicy::DeclaredOnly,
            controlled_access: false,
        }
    }
}

fn default_safety_network() -> NetworkPolicy {
    NetworkPolicy::None { allowlist: vec![] }
}

impl SafetyPolicy {
    /// Suppress serialization when default — keeps YAML / JSON minimal
    /// for the common case (Compute / no-egress / None / None /
    /// DeclaredOnly).
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Extended attribute-type classification + edge
    // satisfaction tests. The composer's attribute-resolution pass
    // (S7.4) exercises these on every depends_on edge; coverage
    // here pins the algebra so a future change to the classifier
    // (e.g., promoting scalars out of Unknown) doesn't silently
    // regress edge-matching semantics.

    #[test]
    fn attribute_type_classifies_record() {
        let v = serde_json::json!({"a": "x", "b": 5});
        match AttributeType::classify(&v) {
            AttributeType::Record { properties } => {
                assert_eq!(properties.len(), 2);
                assert!(properties.contains_key("a"));
                assert!(properties.contains_key("b"));
            }
            other => panic!("expected Record, got {other:?}"),
        }
    }

    #[test]
    fn attribute_type_classifies_homogeneous_array() {
        let v = serde_json::json!([1, 2, 3]);
        match AttributeType::classify(&v) {
            AttributeType::Array { items } => {
                assert_eq!(*items, AttributeType::Unknown);
            }
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn attribute_type_classifies_array_of_records() {
        let v = serde_json::json!([{"a": 1}, {"a": 2}]);
        match AttributeType::classify(&v) {
            AttributeType::Array { items } => match *items {
                AttributeType::Record { properties } => {
                    assert!(properties.contains_key("a"));
                }
                other => panic!("expected element Record, got {other:?}"),
            },
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn attribute_type_falls_back_to_unknown_on_mixed_array() {
        let v = serde_json::json!([1, "x", true]);
        // All scalars classify as Unknown so the array elements
        // technically share the type Unknown — meaning the array
        // resolves to Array<Unknown>, not the bare Unknown.
        match AttributeType::classify(&v) {
            AttributeType::Array { items } => {
                assert_eq!(*items, AttributeType::Unknown);
            }
            other => panic!("expected Array<Unknown>, got {other:?}"),
        }
    }

    #[test]
    fn attribute_type_classifies_empty_array_as_unknown() {
        let v = serde_json::json!([]);
        assert_eq!(AttributeType::classify(&v), AttributeType::Unknown);
    }

    #[test]
    fn attribute_type_satisfies_unknown_consumer() {
        // Consumer that doesn't care about producer shape always satisfies.
        let p = AttributeType::Record {
            properties: BTreeMap::new(),
        };
        assert!(AttributeType::satisfies(&p, &AttributeType::Unknown));
    }

    #[test]
    fn attribute_type_record_subset_satisfies() {
        // Producer with a + b satisfies consumer asking for a only.
        let mut props_p = BTreeMap::new();
        props_p.insert("a".into(), AttributeType::Unknown);
        props_p.insert("b".into(), AttributeType::Unknown);
        let producer = AttributeType::Record {
            properties: props_p,
        };
        let mut props_c = BTreeMap::new();
        props_c.insert("a".into(), AttributeType::Unknown);
        let consumer = AttributeType::Record {
            properties: props_c,
        };
        assert!(AttributeType::satisfies(&producer, &consumer));
        // Inverse fails (producer missing field consumer wants).
        let mut props_c2 = BTreeMap::new();
        props_c2.insert("c".into(), AttributeType::Unknown);
        let consumer2 = AttributeType::Record {
            properties: props_c2,
        };
        assert!(!AttributeType::satisfies(&producer, &consumer2));
    }

    #[test]
    fn attribute_type_union_satisfies_when_any_variant_matches() {
        let producer = AttributeType::Union {
            variants: vec![
                AttributeType::Record {
                    properties: BTreeMap::new(),
                },
                AttributeType::Array {
                    items: Box::new(AttributeType::Unknown),
                },
            ],
        };
        // Consumer wanting a record matches the first variant.
        let consumer = AttributeType::Record {
            properties: BTreeMap::new(),
        };
        assert!(AttributeType::satisfies(&producer, &consumer));
    }

    #[test]
    fn minimal_operation_atom_roundtrips() {
        let atom = AtomDefinition {
            id: "align_reads".into(),
            version: "1.0.0".into(),
            role: AtomRole::Operation,
            discovery_kind: None,
            description: "Align short reads to a reference genome.".into(),
            edam_operation: "operation:0292".into(),
            edam_data: Some("data:2978".into()),
            edam_format: Some("format:2572".into()),
            assignee: AtomAssignee::Agent,
            depends_on: vec!["data_acquisition".into()],
            excludes: vec![],
            attributes: BTreeMap::new(),
            joint_with: vec![],
            inputs: vec![],
            outputs: vec![],
            method_choice: Some(MethodChoiceRef {
                deferred_to: "discover_aligner".into(),
            }),
            resource_profile: Some(ResourceProfile {
                cpu: Some("heavy".into()),
                memory: Some("large".into()),
                gpu: false,
                runtime_class: Some("hours".into()),
            }),
            preferred_container: None,
            claim_boundary: None,
            iterate: None,
            condition: None,
            required_figures: vec![],
            plot_stage_id: None,
            figure_exempt: None,
            expected_artifacts: vec![],
            required_artifacts: vec![],
            validators: vec![],
            runtime_packages: Default::default(),
            safety: Default::default(),
        };
        let yaml = serde_yml::to_string(&atom).expect("serialize");
        let back: AtomDefinition = serde_yml::from_str(&yaml).expect("roundtrip");
        assert_eq!(atom, back);
    }

    #[test]
    fn discovery_atom_carries_kind() {
        let atom = AtomDefinition {
            id: "discover_aligner".into(),
            version: "1.0.0".into(),
            role: AtomRole::Discovery,
            discovery_kind: Some("method".into()),
            description: "Pick a short-read aligner appropriate to organism + read shape.".into(),
            edam_operation: "swfc:aligner_choice".into(),
            edam_data: None,
            edam_format: None,
            assignee: AtomAssignee::Agent,
            depends_on: vec![],
            excludes: vec![],
            attributes: {
                let mut m = BTreeMap::new();
                m.insert("speed".into(), serde_json::json!("fast"));
                m.insert("reference_free".into(), serde_json::json!(false));
                m
            },
            joint_with: vec![],
            inputs: vec![],
            outputs: vec![],
            method_choice: None,
            resource_profile: None,
            preferred_container: None,
            claim_boundary: None,
            iterate: None,
            condition: None,
            required_figures: vec![],
            plot_stage_id: None,
            figure_exempt: None,
            expected_artifacts: vec![],
            required_artifacts: vec![],
            validators: vec![],
            runtime_packages: Default::default(),
            safety: Default::default(),
        };
        let yaml = serde_yml::to_string(&atom).unwrap();
        assert!(yaml.contains("discovery_kind: method"));
        assert!(yaml.contains("speed: fast"));
    }

    #[test]
    #[allow(deprecated)]
    fn container_spec_default_arch_is_amd64() {
        let atom = AtomDefinition {
            id: "x".into(),
            version: "1.0.0".into(),
            role: AtomRole::Operation,
            discovery_kind: None,
            description: "x".into(),
            edam_operation: "operation:0004".into(),
            edam_data: None,
            edam_format: None,
            assignee: AtomAssignee::Agent,
            depends_on: vec![],
            excludes: vec![],
            attributes: BTreeMap::new(),
            joint_with: vec![],
            inputs: vec![],
            outputs: vec![],
            method_choice: None,
            resource_profile: None,
            preferred_container: Some(ContainerSpec {
                image: "ghcr.io/scripps/scripps-bio-base".into(),
                tag: "0.1.0".into(),
                digest: String::new(),
                arch: default_arch(),
                gpu_required: false,
                network: None,
                source: ContainerSource::default(),
            }),
            claim_boundary: None,
            iterate: None,
            condition: None,
            required_figures: vec![],
            plot_stage_id: None,
            figure_exempt: None,
            expected_artifacts: vec![],
            required_artifacts: vec![],
            validators: vec![],
            runtime_packages: Default::default(),
            safety: Default::default(),
        };
        let yaml = serde_yml::to_string(&atom).unwrap();
        // Default arch is suppressed by skip_serializing_if so the
        // YAML stays minimal in the typical case.
        assert!(!yaml.contains("arch:"), "default arch leaked into YAML");
        // `source: image` is the default; suppressed
        // from YAML so pre-S15.1 atom files stay byte-identical.
        assert!(
            !yaml.contains("source:"),
            "default ContainerSource::Image leaked into YAML"
        );
    }

    /// Typed `NetworkPolicy` round-trips through
    /// serde with the `kind`-tagged shape. `Bridge` produces
    /// `{kind: bridge}`; `None { allowlist }` produces
    /// `{kind: none, allowlist: [...]}`. Empty allowlist on
    /// `None` round-trips identically (Vec::is_empty NOT
    /// skipped — explicit empty allowlist means "no egress").
    #[test]
    fn network_policy_serde_roundtrip() {
        let bridge = NetworkPolicy::Bridge;
        let yaml = serde_yml::to_string(&bridge).unwrap();
        assert!(yaml.contains("kind: bridge"), "got: {yaml}");
        let back: NetworkPolicy = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(bridge, back);

        let none_with_list = NetworkPolicy::None {
            allowlist: vec!["github.com".into(), "ghcr.io".into()],
        };
        let yaml = serde_yml::to_string(&none_with_list).unwrap();
        assert!(yaml.contains("kind: none"), "got: {yaml}");
        assert!(yaml.contains("github.com"), "allowlist serialized");
        let back: NetworkPolicy = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(none_with_list, back);

        let none_empty = NetworkPolicy::None { allowlist: vec![] };
        let yaml = serde_yml::to_string(&none_empty).unwrap();
        let back: NetworkPolicy = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(none_empty, back);
    }

    /// Typed `ContainerSource` round-trips through
    /// serde. `Image` (default) suppresses from YAML; `Conda`
    /// carries channel→packages; `Host` is a unit variant.
    #[test]
    fn container_source_serde_roundtrip() {
        let image = ContainerSource::Image;
        let yaml = serde_yml::to_string(&image).unwrap();
        assert!(yaml.contains("kind: image"), "got: {yaml}");

        let mut packages: BTreeMap<String, Vec<String>> = BTreeMap::new();
        packages.insert("conda-forge".into(), vec!["r-seurat=5.x".into()]);
        packages.insert("bioconda".into(), vec!["scanpy=1.12".into()]);
        let conda = ContainerSource::Conda {
            conda_packages: packages.clone(),
        };
        let yaml = serde_yml::to_string(&conda).unwrap();
        assert!(yaml.contains("kind: conda"), "got: {yaml}");
        assert!(yaml.contains("r-seurat"), "package serialized");
        let back: ContainerSource = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(conda, back);

        let host = ContainerSource::Host;
        let yaml = serde_yml::to_string(&host).unwrap();
        let back: ContainerSource = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(host, back);
    }

    /// Back-compat: pre-S15.1 ContainerSpec YAML
    /// (just image/tag, no `source:` block) deserializes to
    /// `ContainerSource::Image` via `#[serde(default)]`. Wire-shape
    /// preservation guarantees existing atom YAMLs and emitted
    /// `WORKFLOW.json` files keep loading.
    #[test]
    #[allow(deprecated)]
    fn container_spec_back_compat_no_source_field() {
        let yaml = "image: ghcr.io/scripps/scripps-bio-base\n\
                    tag: 0.1.0\n";
        let spec: ContainerSpec = serde_yml::from_str(yaml).unwrap();
        assert_eq!(spec.source, ContainerSource::Image);
        assert_eq!(spec.network, None);
        assert_eq!(spec.image, "ghcr.io/scripps/scripps-bio-base");
        assert_eq!(spec.tag, "0.1.0");
    }

    #[test]
    fn exclusions_and_dependencies_are_ordered() {
        // Determinism: BTreeMap/Vec preserve insertion order; YAML
        // round-trips byte-identically. Lock this in so a refactor
        // to HashMap/HashSet would fail.
        let atom = AtomDefinition {
            id: "x".into(),
            version: "1.0.0".into(),
            role: AtomRole::Operation,
            discovery_kind: None,
            description: "x".into(),
            edam_operation: "operation:0004".into(),
            edam_data: None,
            edam_format: None,
            assignee: AtomAssignee::Agent,
            depends_on: vec!["a".into(), "b".into(), "c".into()],
            excludes: vec!["x_alt_1".into(), "x_alt_2".into()],
            attributes: BTreeMap::new(),
            joint_with: vec![],
            inputs: vec![],
            outputs: vec![],
            method_choice: None,
            resource_profile: None,
            preferred_container: None,
            claim_boundary: None,
            iterate: None,
            condition: None,
            required_figures: vec![],
            plot_stage_id: None,
            figure_exempt: None,
            expected_artifacts: vec![],
            required_artifacts: vec![],
            validators: vec![],
            runtime_packages: Default::default(),
            safety: Default::default(),
        };
        let yaml = serde_yml::to_string(&atom).unwrap();
        let a_pos = yaml.find("- a").unwrap();
        let b_pos = yaml.find("- b").unwrap();
        let c_pos = yaml.find("- c").unwrap();
        assert!(a_pos < b_pos && b_pos < c_pos);
    }

    #[test]
    fn safety_policy_default_roundtrips() {
        let p = SafetyPolicy::default();
        let yaml = serde_yml::to_string(&p).unwrap();
        let back: SafetyPolicy = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(p, back);
        // Defaults: Compute level, no-network, no code, no sandbox,
        // DeclaredOnly provisioning.
        assert_eq!(p.level, SafetyLevel::Compute);
        assert_eq!(p.code_execution, CodeExecution::None);
        assert_eq!(p.sandbox, SandboxRequirement::None);
        assert_eq!(p.provisioning, ProvisioningPolicy::DeclaredOnly);
    }

    #[test]
    fn safety_policy_exec_roundtrips() {
        let p = SafetyPolicy {
            level: SafetyLevel::Exec,
            network: NetworkPolicy::None {
                allowlist: vec!["api.example.com".into()],
            },
            code_execution: CodeExecution::GeneratedByAgent,
            sandbox: SandboxRequirement::ProcessIsolation,
            provisioning: ProvisioningPolicy::Allowlisted,
            controlled_access: false,
        };
        let yaml = serde_yml::to_string(&p).unwrap();
        let back: SafetyPolicy = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(p, back);
        // Serialized YAML uses snake_case discriminants.
        assert!(yaml.contains("level: exec"));
        assert!(yaml.contains("code_execution: generated_by_agent"));
        assert!(yaml.contains("sandbox: process_isolation"));
        assert!(yaml.contains("provisioning: allowlisted"));
    }

    #[test]
    fn atom_definition_carries_safety_policy() {
        let mut atom = AtomDefinition::test_default("exec_atom");
        atom.safety = SafetyPolicy {
            level: SafetyLevel::Exec,
            network: NetworkPolicy::Bridge,
            code_execution: CodeExecution::GeneratedByAgent,
            sandbox: SandboxRequirement::ProcessIsolation,
            provisioning: ProvisioningPolicy::Allowlisted,
            controlled_access: false,
        };
        let yaml = serde_yml::to_string(&atom).unwrap();
        let back: AtomDefinition = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(atom, back);
        assert!(yaml.contains("safety:"));
        assert!(yaml.contains("level: exec"));
    }

    #[test]
    fn atom_definition_default_safety_suppressed_from_yaml() {
        let atom = AtomDefinition::test_default("plain_atom");
        let yaml = serde_yml::to_string(&atom).unwrap();
        // Default SafetyPolicy must be skipped — keeps existing atom YAMLs
        // byte-identical after the field is added.
        assert!(!yaml.contains("safety:"), "default safety leaked: {yaml}");
    }
}
