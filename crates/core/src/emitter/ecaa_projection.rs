//! Spec-shaped projection layer (C1 Phase-2, approach (a)).
//!
//! # Why this module exists
//!
//! The 8 committed sidecar JSON Schemas under
//! `docs/ecaa-spec/subgraph-schemas/*.json` USED to be schemars-generated
//! FROM the internal Rust record types (`Turn`, `DecisionRecord`,
//! `ValidationReport`, `EdgeContract`, `ClaimVerificationReport`,
//! `VerifierDecision`, `Assumption`, `AuditProofReport`). That made
//! emit-time schema validation a **tautology** — the implementation types
//! were validated against schemas derived from those same types, so the
//! check could never catch a divergence from the spec's normative node /
//! edge model (`v0.2.md` §4-5, `ecaa-v0.2.ttl`).
//!
//! This module breaks the tautology. It is a thin, pure projection that
//! maps each internal record into the spec's typed node / edge JSON — the
//! `(N_G, E_G)` pair from §4.1, with the closed `type` / `predicate`
//! enums of §5 and the §4.2 prefix-tagged cross-graph ids. The
//! hand-authored spec schemas then validate THIS projection output, not
//! the raw impl-typed sidecars.
//!
//! # Shape contract (must agree with the schemas + the validator)
//!
//! A projected sub-graph is a JSON array whose every element is either a
//! **node** or an **edge**:
//!
//! - node: `{ "id": "<G>:<localid>", "type": "<NodeType>", "props": { … } }`
//! - edge: `{ "source_id": "<id>", "target_id": "<[G:]id>", "predicate": "<Predicate>" }`
//!
//! `type` is drawn from [`SpecNodeType`] (the 25-member closed set,
//! pinned to [`ecaa_workflow_types::consts::NODE_TYPES`]); `predicate` is
//! drawn from [`SpecPredicate`] (the 20-member closed set, pinned to
//! [`ecaa_workflow_types::consts::EDGE_PREDICATES`]). The hand-authored
//! `subgraph-schemas/*.json` encode exactly these closed enums, and the
//! `schemars_generation.rs` drift test asserts the schema enum members
//! equal the consts arrays — so a node typed outside the closed set
//! fails validation rather than silently passing.
//!
//! # Scope (Phase-2 = design + skeleton)
//!
//! The projection is intentionally lossy: it carries the spec-required
//! structural fields onto the typed nodes/edges and drops impl-only
//! provenance (chain-of-custody, schema_version, ctx hashes, …) that the
//! spec object model does not name. Sidecars whose on-disk row shape does
//! not deserialize cleanly into a single typed record (intake turns,
//! proofs.jsonl, claim-verification.json) are projected value-first in
//! [`project_subgraph`]; the typed [`ProjectToSpec`] impls cover the
//! records the plan names explicitly and exist primarily for
//! per-record-type unit coverage.

use crate::audit_proof::loader::LoadedPackage;
use serde_json::{json, Map, Value};

/// The 25 closed node types (`v0.2.md` §5), one variant per entry in
/// [`ecaa_workflow_types::consts::NODE_TYPES`]. The wire form is produced
/// exclusively via [`SpecNodeType::as_str`] (no serde derive) so the
/// closed-set string can never silently diverge from a serde rename; a
/// drift test pins it to the const so adding a node type touches both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecNodeType {
    // I — Intent (5)
    Question,
    Cohort,
    Contrast,
    Modality,
    ExpectedOutput,
    // D — Decision (4)
    MethodChoice,
    Justification,
    Alternative,
    Citation,
    // E — Execution (5)
    WorkflowStep,
    Container,
    InputFile,
    OutputFile,
    RuntimeEnvironment,
    // V — Evidence (4)
    Table,
    Figure,
    Statistic,
    File,
    // C — Claim (3)
    Claim,
    Quantification,
    Direction,
    // Q — Equivalence (1)
    RerunOutcome,
    // F — Failure (2)
    Blocker,
    RecoveryAction,
    // A — Audit-proof (1)
    InvariantVerdict,
}

impl SpecNodeType {
    /// Canonical wire string — MUST match the matching
    /// `consts::NODE_TYPES` entry. (PascalCase, not snake_case: node types
    /// are class names, unlike the snake_case edge predicates.)
    pub fn as_str(self) -> &'static str {
        match self {
            SpecNodeType::Question => "Question",
            SpecNodeType::Cohort => "Cohort",
            SpecNodeType::Contrast => "Contrast",
            SpecNodeType::Modality => "Modality",
            SpecNodeType::ExpectedOutput => "ExpectedOutput",
            SpecNodeType::MethodChoice => "MethodChoice",
            SpecNodeType::Justification => "Justification",
            SpecNodeType::Alternative => "Alternative",
            SpecNodeType::Citation => "Citation",
            SpecNodeType::WorkflowStep => "WorkflowStep",
            SpecNodeType::Container => "Container",
            SpecNodeType::InputFile => "InputFile",
            SpecNodeType::OutputFile => "OutputFile",
            SpecNodeType::RuntimeEnvironment => "RuntimeEnvironment",
            SpecNodeType::Table => "Table",
            SpecNodeType::Figure => "Figure",
            SpecNodeType::Statistic => "Statistic",
            SpecNodeType::File => "File",
            SpecNodeType::Claim => "Claim",
            SpecNodeType::Quantification => "Quantification",
            SpecNodeType::Direction => "Direction",
            SpecNodeType::RerunOutcome => "RerunOutcome",
            SpecNodeType::Blocker => "Blocker",
            SpecNodeType::RecoveryAction => "RecoveryAction",
            SpecNodeType::InvariantVerdict => "InvariantVerdict",
        }
    }
}

/// The 20 closed edge predicates (`v0.2.md` §5), one variant per entry in
/// [`ecaa_workflow_types::consts::EDGE_PREDICATES`]. Wire form is
/// snake_case (plus the one PROV-O import `prov:wasDerivedFrom`), produced
/// exclusively via [`SpecPredicate::as_str`] (no serde derive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecPredicate {
    // I (3)
    Refines,
    Stratifies,
    Expects,
    // D (5)
    Chooses,
    Rejects,
    Cites,
    Amends,
    WasDerivedFrom,
    // E (3)
    Produces,
    Consumes,
    RunsIn,
    // V (2)
    AppearsIn,
    ComputedFrom,
    // C (2)
    SupportedBy,
    Contradicts,
    // Q (2)
    EquivalentTo,
    DivergesFrom,
    // F (2)
    Requires,
    Unblocks,
    // A (1)
    EvaluatedAgainst,
}

impl SpecPredicate {
    /// Canonical wire string — MUST match the matching
    /// `consts::EDGE_PREDICATES` entry.
    pub fn as_str(self) -> &'static str {
        match self {
            SpecPredicate::Refines => "refines",
            SpecPredicate::Stratifies => "stratifies",
            SpecPredicate::Expects => "expects",
            SpecPredicate::Chooses => "chooses",
            SpecPredicate::Rejects => "rejects",
            SpecPredicate::Cites => "cites",
            SpecPredicate::Amends => "amends",
            SpecPredicate::WasDerivedFrom => "prov:wasDerivedFrom",
            SpecPredicate::Produces => "produces",
            SpecPredicate::Consumes => "consumes",
            SpecPredicate::RunsIn => "runs_in",
            SpecPredicate::AppearsIn => "appears_in",
            SpecPredicate::ComputedFrom => "computed_from",
            SpecPredicate::SupportedBy => "supported_by",
            SpecPredicate::Contradicts => "contradicts",
            SpecPredicate::EquivalentTo => "equivalent_to",
            SpecPredicate::DivergesFrom => "diverges_from",
            SpecPredicate::Requires => "requires",
            SpecPredicate::Unblocks => "unblocks",
            SpecPredicate::EvaluatedAgainst => "evaluated_against",
        }
    }
}

/// One spec-typed node in a sub-graph `G` (`v0.2.md` §4.1). `id` is the
/// LOCAL identifier within `G`; [`SpecNode::to_value`] prefix-tags it with
/// the sub-graph letter per §4.2.
#[derive(Debug, Clone, PartialEq)]
pub struct SpecNode {
    /// Local (un-prefixed) identifier, unique within the sub-graph.
    pub id: String,
    /// Closed node type.
    pub node_type: SpecNodeType,
    /// Spec-named structural props (text, value, status, class, …).
    pub props: Map<String, Value>,
}

impl SpecNode {
    /// Build a node with no props. Accepts `&str`, `&String`, or `String`.
    pub fn new(id: impl AsRef<str>, node_type: SpecNodeType) -> Self {
        Self {
            id: id.as_ref().to_string(),
            node_type,
            props: Map::new(),
        }
    }

    /// Attach one prop and return self (builder).
    pub fn with_prop(mut self, key: &str, value: Value) -> Self {
        self.props.insert(key.to_string(), value);
        self
    }

    /// Serialize as the on-wire node JSON, prefix-tagging `id` with the
    /// sub-graph `letter` (§4.2). The `props` object is preserved as a
    /// nested object so the hand-authored schema can validate node-type
    /// `type` independently of free-form props.
    pub fn to_value(&self, letter: char) -> Value {
        json!({
            "id": format!("{letter}:{}", self.id),
            "type": self.node_type.as_str(),
            "props": Value::Object(self.props.clone()),
        })
    }
}

/// One spec-typed edge `(source_id, target_id, predicate)` (`v0.2.md`
/// §4.1). `source_id` is always local to `G`; `target_id` is local for an
/// intra-graph edge and already prefix-tagged (`<G>:<id>`) for a
/// cross-graph reference (§4.2).
#[derive(Debug, Clone, PartialEq)]
pub struct SpecEdge {
    /// Source node id (local to the emitting sub-graph).
    pub source_id: String,
    /// Target node id (local, or prefix-tagged for cross-graph refs).
    pub target_id: String,
    /// Closed edge predicate.
    pub predicate: SpecPredicate,
}

impl SpecEdge {
    /// Build an edge. Accepts `&str`, `&String`, or `String` for both ids.
    pub fn new(
        source_id: impl AsRef<str>,
        target_id: impl AsRef<str>,
        predicate: SpecPredicate,
    ) -> Self {
        Self {
            source_id: source_id.as_ref().to_string(),
            target_id: target_id.as_ref().to_string(),
            predicate,
        }
    }

    /// Serialize as the on-wire edge JSON, prefix-tagging the LOCAL
    /// `source_id` with `letter`. `target_id` is emitted verbatim: a
    /// cross-graph target is already prefix-tagged by the projector, and a
    /// local target is prefixed here for global resolvability (§4.3).
    pub fn to_value(&self, letter: char) -> Value {
        let target = if is_prefix_tagged(&self.target_id) {
            self.target_id.clone()
        } else {
            format!("{letter}:{}", self.target_id)
        };
        json!({
            "source_id": format!("{letter}:{}", self.source_id),
            "target_id": target,
            "predicate": self.predicate.as_str(),
        })
    }
}

/// True when `id` already carries a `^(I|D|E|V|C|Q|F|A):` prefix tag
/// (§4.2). Used so the projector can mix local + cross-graph targets and
/// the serializer only prefixes the local ones.
fn is_prefix_tagged(id: &str) -> bool {
    match id.split_once(':') {
        Some((letter, rest)) => {
            !rest.is_empty() && matches!(letter, "I" | "D" | "E" | "V" | "C" | "Q" | "F" | "A")
        }
        None => false,
    }
}

/// Pure projection from an internal record to its spec nodes + edges.
///
/// Implemented for the record types the C1 plan names explicitly. The
/// value-first [`project_subgraph`] does the on-disk row → typed record →
/// `project()` plumbing; these impls are the per-record unit of the
/// mapping and exist so each record-type's projection is independently
/// testable.
pub trait ProjectToSpec {
    /// Project to `(nodes, edges)` with LOCAL ids (the serializer adds the
    /// sub-graph prefix). Cross-graph edge targets are returned
    /// already-tagged.
    fn project(&self) -> (Vec<SpecNode>, Vec<SpecEdge>);
}

// ───────────────────────── D — Decision ─────────────────────────────────

impl ProjectToSpec for crate::decision_log::DecisionRecord {
    /// `set_intake_method` / `amend_stage` → a `MethodChoice` node + a
    /// `chooses` edge to the named method (§5.2). The §5.2 justification
    /// cardinality is satisfied by the `rationale` prop (≥30 chars) carried
    /// onto the node; v0.1 emits no per-decision `Citation`, so `cites`
    /// edges are absent. Other decision kinds project to nothing (they are
    /// not part of the D object model).
    fn project(&self) -> (Vec<SpecNode>, Vec<SpecEdge>) {
        use crate::decision_log::DecisionType;
        let (stage, method_prose) = match &self.decision {
            DecisionType::SetIntakeMethod {
                stage,
                method_prose,
            }
            | DecisionType::AmendStage {
                stage,
                method_prose,
            } => (stage.clone(), method_prose.clone()),
            _ => return (Vec::new(), Vec::new()),
        };
        let rationale = self
            .rationale
            .clone()
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| method_prose.clone());
        let node_id = format!("method_{}", sanitize_id(&stage));
        // `chooses` points to the chosen method identifier. The full prose
        // is preserved in the node's `value` prop; the edge target is a
        // sanitized method token so it satisfies the §4.2 id grapheme set.
        let method_token = sanitize_id(method_token_from_prose(&method_prose));
        let node = SpecNode::new(&node_id, SpecNodeType::MethodChoice)
            .with_prop("stage", json!(stage))
            .with_prop("value", json!(method_prose))
            .with_prop("rationale", json!(rationale));
        let edge = SpecEdge::new(&node_id, method_token, SpecPredicate::Chooses);
        (vec![node], vec![edge])
    }
}

// ───────────────────────── E + V — EdgeContract ─────────────────────────

impl ProjectToSpec for crate::workflow_contracts::edge::EdgeContract {
    /// An `EdgeContract` carries a producer→consumer dependency. Project it
    /// as the consumer `WorkflowStep` (E) plus a `consumes` edge naming the
    /// producer step's dependency. `consumes` is one of E's three closed
    /// predicates (§5.3); the producer's own `WorkflowStep` node is emitted
    /// by its own EdgeContract row (or stands alone when it has no
    /// upstream). The V sub-graph carries the orthogonal `computed_from`
    /// lineage to E `OutputFile`s — see [`project_evidence_subgraph`].
    fn project(&self) -> (Vec<SpecNode>, Vec<SpecEdge>) {
        let node = SpecNode::new(&self.to_node, SpecNodeType::WorkflowStep)
            .with_prop("from_port", json!(self.from_port))
            .with_prop("to_port", json!(self.to_port));
        let edge = SpecEdge::new(
            &self.to_node,
            self.from_node.clone(),
            SpecPredicate::Consumes,
        );
        (vec![node], vec![edge])
    }
}

// ───────────────────────── F — Assumption ───────────────────────────────

impl ProjectToSpec for crate::workflow_contracts::evidence::Assumption {
    /// An `Assumption` is the F sub-graph's `Blocker` (the file retains its
    /// legacy "assumptions" name; §5.7). No recovery action is modeled in
    /// v0.1's emit-time ledger, so the §5.7 cardinality is satisfied by the
    /// `resolved_at`-equivalent `resolution` prop rather than an `unblocks`
    /// edge.
    fn project(&self) -> (Vec<SpecNode>, Vec<SpecEdge>) {
        // `AssumptionResolution` is a `serde(tag = "kind")` enum, so it
        // serializes to an object `{"kind": "..."}`; lift the tag string.
        let resolution = serde_json::to_value(&self.resolution)
            .ok()
            .and_then(|v| v.get("kind").and_then(Value::as_str).map(str::to_string));
        let mut node = SpecNode::new(&self.id, SpecNodeType::Blocker)
            .with_prop("statement", Value::String(self.statement.clone()));
        if let Some(r) = resolution {
            node = node.with_prop("resolution", Value::String(r));
        }
        (vec![node], Vec::new())
    }
}

// ───────────────────────── Q — VerifierDecision ─────────────────────────
//
// The internal `VerifierDecision` is the COMPILE-TIME compatibility-engine
// substrate, not a re-execution outcome. The spec Q sub-graph
// (`RerunOutcome` with the 5-class enum) is populated post-emit by the
// harness re-execution classifier. At emit time `verifier-decisions.jsonl`
// is empty, so the typed impl below projects a single `RerunOutcome` from
// whatever class field a harness-written row carries, and the value-first
// projector handles the empty-at-emit case by returning an empty array.
// The impl is keyed on the on-disk fields rather than the internal
// `VerifierDecision` enum because that enum models a different concept.

/// Project a single harness-written Q row (`{ id?, class?, … }`) into a
/// `RerunOutcome` node. Returns `None` for rows with no recognizable id.
fn project_rerun_outcome_row(row: &Value) -> Option<SpecNode> {
    let id = row
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| row.get("artifact_path").and_then(Value::as_str))?;
    let mut node = SpecNode::new(sanitize_id(id), SpecNodeType::RerunOutcome);
    if let Some(class) = row
        .get("class")
        .or_else(|| row.get("bucket"))
        .and_then(Value::as_str)
    {
        node = node.with_prop("class", json!(class));
    }
    Some(node)
}

// ───────────────────────── I — intake Turn ──────────────────────────────
//
// The on-disk `intake-conversation.jsonl` row shape is heterogeneous (full
// `Turn`, `ToolCallRecord`, and the emitter's flattened
// `{role, content, type}` form all coexist). The I sub-graph object model
// (§5.1) needs at least one `Question`, exactly one `Modality`, and at
// least one `ExpectedOutput`. Rather than guess those from free-text turn
// bodies, the value-first projector synthesizes the three required I nodes
// from the package's classification-derived intake row; see
// [`project_intent_subgraph`].

// ───────────────────────── value-first projectors ───────────────────────

/// Project the I (Intent) sub-graph. The §5.1 cardinality REQUIRES ≥1
/// `Question`, exactly one `Modality`, ≥1 `ExpectedOutput`. The emitter's
/// intake row carries the SME's question text and (via the package's
/// classification) the modality; we synthesize the three required nodes
/// plus an `expects` edge from the question to the expected output.
fn project_intent_subgraph(pkg: &LoadedPackage) -> Vec<SpecNode> {
    // First non-tool intake row with a `content`/`text` body is the SME
    // question. The emitter row is `{role, content, type:"Question", …}`.
    let question_text = pkg.intake.iter().find_map(|row| {
        let is_tool = row.get("tool_name").is_some();
        if is_tool {
            return None;
        }
        row.get("content")
            .or_else(|| row.get("text"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    });
    let modality = pkg.intake.iter().find_map(|row| {
        row.get("modality")
            .or_else(|| row.get("value"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    vec![
        SpecNode::new("question_001", SpecNodeType::Question)
            .with_prop("text", json!(question_text.unwrap_or_default())),
        SpecNode::new("modality_001", SpecNodeType::Modality)
            .with_prop("value", json!(modality.unwrap_or_else(|| "unknown".into()))),
        SpecNode::new("output_001", SpecNodeType::ExpectedOutput)
            .with_prop("schema", json!("analysis_results")),
    ]
}

/// Project the C (Claim) sub-graph from `claim-verification.json`. Each
/// `verdicts[]` entry becomes a `Claim` node; each `supported_by[]` string
/// becomes a cross-graph `supported_by` edge tagged into V (§5.5 anchors
/// Invariant 1). The 3-value status enum is mapped onto the closed
/// `verified|pending|contradicted` spec set.
fn project_claim_subgraph(pkg: &LoadedPackage) -> Vec<Value> {
    let Some(claims) = pkg.claims.as_ref() else {
        return Vec::new();
    };
    let Some(verdicts) = claims.get("verdicts").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (idx, v) in verdicts.iter().enumerate() {
        // §4.2 id grapheme set — sanitize the impl claim_id (e.g. `c-001`
        // is already safe; defensively collapse any out-of-set chars).
        let id = v
            .get("claim_id")
            .and_then(Value::as_str)
            .map(sanitize_id)
            .unwrap_or_else(|| format!("claim_{idx:03}"));
        let status = map_claim_status(v.get("status").and_then(Value::as_str).unwrap_or("pending"));
        let text = v
            .get("narrative_text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let node = SpecNode::new(&id, SpecNodeType::Claim)
            .with_prop("text", json!(text))
            .with_prop("status", json!(status));
        out.push(node.to_value('C'));
        if let Some(refs) = v.get("supported_by").and_then(Value::as_array) {
            for r in refs.iter().filter_map(Value::as_str) {
                // The supported_by target is a V Evidence node; tag it into V.
                let target = format!("V:{}", evidence_local_id(r));
                let edge = SpecEdge::new(&id, target, SpecPredicate::SupportedBy);
                out.push(edge.to_value('C'));
            }
        }
    }
    out
}

/// Map an impl claim-status string onto the closed §5.5 set.
fn map_claim_status(status: &str) -> &'static str {
    match status {
        "verified" => "verified",
        "mismatch" | "contradicted" => "contradicted",
        // unverifiable / pending / anything else → pending
        _ => "pending",
    }
}

/// Derive a stable V-local id from a `supported_by` reference string such
/// as `runtime/tables/de_results.csv#row_TP53`. The fragment (after `#`)
/// is the most specific evidence handle; fall back to the whole string.
fn evidence_local_id(reference: &str) -> String {
    let handle = reference.rsplit('#').next().unwrap_or(reference);
    sanitize_id(handle)
}

/// Extract a short method token from a free-text method prose. Uses the
/// first whitespace-delimited word (the chosen tool name in practice, e.g.
/// "DESeq2 chosen per protocol" → "DESeq2"); falls back to the whole prose
/// when it is a single token.
fn method_token_from_prose(prose: &str) -> &str {
    prose.split_whitespace().next().unwrap_or(prose)
}

/// Reduce an arbitrary string to the §4.2 id grapheme set
/// (`[A-Za-z0-9_\-]`). Disallowed chars collapse to `_`.
fn sanitize_id(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "x".to_string()
    } else {
        cleaned
    }
}

/// Project the V (Evidence) sub-graph from the package's ACTUAL analytical
/// outputs — the RO-Crate `@graph` output entities (figure obligations /
/// produced `runtime/outputs/` artifacts) plus any real-path proofs row — via
/// the shared [`crate::audit_proof::output_source`] derivation that Invariant 3
/// (`evidence_coverage`) also uses, so reader and writer AGREE.
///
/// This DECOUPLES V from E: the Execution (E) sub-graph stays backed by
/// `proofs.jsonl` `EdgeContract` → `WorkflowStep` rows (see the `EdgeContract`
/// [`ProjectToSpec`] impl), while V materializes from the produced/declared
/// output entities. Each output becomes a `Figure`/`Table`/`File` V node
/// (§5.4 closed set) with a unique id, plus a `computed_from` edge to the E
/// `WorkflowStep` that produced it (the producing task, derived from the
/// `runtime/outputs/<task>/…` path). §5.4 anchors Invariant 3.
fn project_evidence_subgraph(pkg: &LoadedPackage) -> Vec<Value> {
    use crate::audit_proof::output_source::analytical_outputs;
    use std::collections::BTreeMap;
    // De-dup local ids by basename, disambiguating collisions deterministically
    // (two `volcano.png` under different tasks would otherwise share an id).
    let mut seen: BTreeMap<String, u32> = BTreeMap::new();
    let mut out = Vec::new();
    for (idx, output) in analytical_outputs(&pkg.output_entities, &pkg.proofs)
        .into_iter()
        .enumerate()
    {
        let base = sanitize_id(output.path.rsplit('/').next().unwrap_or(&output.path));
        let base = if base.is_empty() {
            format!("evidence_{idx:03}")
        } else {
            base
        };
        let count = seen.entry(base.clone()).or_insert(0);
        let id = if *count == 0 {
            base.clone()
        } else {
            format!("{base}_{count}")
        };
        *count += 1;
        let node_type = match output.kind {
            crate::audit_proof::output_source::OutputKind::Figure => SpecNodeType::Figure,
            crate::audit_proof::output_source::OutputKind::Table => SpecNodeType::Table,
            crate::audit_proof::output_source::OutputKind::File => SpecNodeType::File,
        };
        let node = SpecNode::new(&id, node_type).with_prop("path", json!(output.path));
        out.push(node.to_value('V'));
        if let Some(task) = &output.producer_task {
            // `computed_from` points from the V evidence node to the producing
            // E `WorkflowStep` (the §5.4 cardinality requirement).
            let target = format!("E:{}", sanitize_id(task));
            let edge = SpecEdge::new(&id, target, SpecPredicate::ComputedFrom);
            out.push(edge.to_value('V'));
        }
    }
    out
}

/// Project the A (Audit-proof) sub-graph from `audit-proof-report.json`.
/// Each verdict becomes an `InvariantVerdict` node plus an
/// `evaluated_against` edge to the package IRI (§5.8 REQUIRES exactly 6
/// nodes, each with an `evaluated_against` edge).
fn project_audit_proof_subgraph(report: &Value) -> Vec<Value> {
    let Some(verdicts) = report.get("verdicts").and_then(Value::as_array) else {
        return Vec::new();
    };
    let pkg_iri = report
        .get("package_iri")
        .and_then(Value::as_str)
        .unwrap_or("ro-crate-metadata.json");
    let mut out = Vec::new();
    for (idx, v) in verdicts.iter().enumerate() {
        let invariant_id = v.get("id").and_then(Value::as_str).unwrap_or("");
        let id = if invariant_id.is_empty() {
            format!("verdict_{idx:03}")
        } else {
            invariant_id.to_string()
        };
        let status = v
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unverified");
        let node = SpecNode::new(&id, SpecNodeType::InvariantVerdict)
            .with_prop("invariant_id", json!(invariant_id))
            .with_prop("verdict", json!(status));
        out.push(node.to_value('A'));
        // `evaluated_against` target is an IRI, emitted verbatim (not a
        // sub-graph node) per §5.8.
        out.push(json!({
            "source_id": format!("A:{id}"),
            "target_id": pkg_iri,
            "predicate": SpecPredicate::EvaluatedAgainst.as_str(),
        }));
    }
    out
}

/// Project one sub-graph (`letter ∈ {I,D,E,V,C,Q,F,A}`) of a loaded
/// package into the spec node/edge JSON array the hand-authored schema
/// validates. Unknown letters return an empty vec.
///
/// This is the single entry point the emit-time validator calls; it owns
/// the on-disk-row → typed-record plumbing so the [`ProjectToSpec`] impls
/// stay pure and unit-testable.
pub fn project_subgraph(letter: char, pkg: &LoadedPackage) -> Vec<Value> {
    match letter {
        'I' => project_intent_subgraph(pkg)
            .iter()
            .map(|n| n.to_value('I'))
            .collect(),
        'D' => project_typed_jsonl::<crate::decision_log::DecisionRecord>(&pkg.decisions, 'D'),
        'E' => {
            project_typed_jsonl::<crate::workflow_contracts::edge::EdgeContract>(&pkg.proofs, 'E')
        }
        'V' => project_evidence_subgraph(pkg),
        'C' => project_claim_subgraph(pkg),
        'Q' => pkg
            .verifier_decisions
            .iter()
            .filter_map(project_rerun_outcome_row)
            .map(|n| n.to_value('Q'))
            .collect(),
        'F' => project_typed_jsonl::<crate::workflow_contracts::evidence::Assumption>(
            &pkg.assumptions,
            'F',
        ),
        // The audit-proof report is NOT a `LoadedPackage` field (it is the
        // report's own output, written after the other 7 sidecars). The
        // single-arg `project_subgraph` therefore returns empty for 'A';
        // the validator reads `audit-proof-report.json` itself and calls
        // [`project_audit_proof`] directly.
        'A' => Vec::new(),
        _ => Vec::new(),
    }
}

/// Deserialize each on-disk JSONL row into `T`, project it, and flatten to
/// node/edge JSON. Rows that fail to deserialize into `T` are skipped (the
/// spec object model only names the records this projection understands).
fn project_typed_jsonl<T>(rows: &[Value], letter: char) -> Vec<Value>
where
    T: serde::de::DeserializeOwned + ProjectToSpec,
{
    let mut out = Vec::new();
    for row in rows {
        let Ok(record) = serde_json::from_value::<T>(row.clone()) else {
            continue;
        };
        let (nodes, edges) = record.project();
        for n in &nodes {
            out.push(n.to_value(letter));
        }
        for e in &edges {
            out.push(e.to_value(letter));
        }
    }
    out
}

/// Project the A sub-graph directly from a loaded `audit-proof-report.json`
/// value. Exposed because the report is not a `LoadedPackage` field; the
/// validator reads the file and calls this.
pub fn project_audit_proof(report: &Value) -> Vec<Value> {
    project_audit_proof_subgraph(report)
}

/// Project the A (Audit-proof) sub-graph into RO-Crate JSON-LD `@graph`
/// node form so the audit-proof verdicts live as FIRST-CLASS typed triples
/// inside `ro-crate-metadata.json` (not merely as a file-reference to
/// `runtime/audit-proof-report.json`).
///
/// Each verdict becomes one JSON-LD node:
///
/// ```json
/// { "@id": "A:claim_completeness",
///   "@type": "InvariantVerdict",
///   "invariant_id": "claim_completeness",
///   "verdict": "<status>",
///   "evaluated_against": { "@id": "ro-crate-metadata.json" } }
/// ```
///
/// The `evaluated_against` edge is folded onto the node as a JSON-LD object
/// reference (idiomatic in an RO-Crate `@graph`, where edges are properties
/// rather than standalone triple objects). The node `@id` is derived from
/// the invariant id — deterministic, no wall-clock / uuid value — so two
/// emits of the same package produce byte-identical nodes. The report's
/// `evaluated_at` timestamp is INTENTIONALLY dropped here (it is the
/// spec-documented byte-reproducibility exclusion and must never enter the
/// determinism baseline that `ro-crate-metadata.json` belongs to).
///
/// Returns the nodes sorted by `@id` for deterministic graph order.
pub fn project_audit_proof_jsonld(report: &Value) -> Vec<Value> {
    let Some(verdicts) = report.get("verdicts").and_then(Value::as_array) else {
        return Vec::new();
    };
    let pkg_iri = report
        .get("package_iri")
        .and_then(Value::as_str)
        .unwrap_or("ro-crate-metadata.json");
    // BTreeMap keyed on the prefix-tagged @id gives deterministic ordering
    // and de-duplicates any (theoretically impossible) repeated invariant id.
    let mut nodes: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    for (idx, v) in verdicts.iter().enumerate() {
        let invariant_id = v.get("id").and_then(Value::as_str).unwrap_or("");
        let local_id = if invariant_id.is_empty() {
            format!("verdict_{idx:03}")
        } else {
            sanitize_id(invariant_id)
        };
        let at_id = format!("A:{local_id}");
        let status = v
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unverified");
        nodes.insert(
            at_id.clone(),
            json!({
                "@id": at_id,
                "@type": SpecNodeType::InvariantVerdict.as_str(),
                "invariant_id": invariant_id,
                "verdict": status,
                // `evaluated_against` (§5.8) folded onto the node as a
                // JSON-LD object reference to the package IRI.
                SpecPredicate::EvaluatedAgainst.as_str(): { "@id": pkg_iri },
            }),
        );
    }
    nodes.into_values().collect()
}

/// Project the C (Claim) sub-graph into RO-Crate JSON-LD `@graph` node form so
/// the runtime verifier's verdicts live as FIRST-CLASS typed `Claim` triples
/// inside `ro-crate-metadata.json` — not merely behind the file-reference to
/// `runtime/claim-verification.json` (which is the empty agent-writable stub
/// post-exec). The verdicts are fed from the SIGNED sink
/// (`runtime/verification-reports/claim-verification.signed.json`), passed here
/// already unioned into the `{verdicts:[…]}` `claims` shape the loader returns.
///
/// REUSES [`project_claim_subgraph`] verbatim (which owns the `claim_id`
/// sanitization, the `verified|pending|contradicted` [`map_claim_status`]
/// mapping, and the `supported_by → V:` [`evidence_local_id`] edge derivation)
/// by handing it a minimal [`LoadedPackage`] carrying only the claims; the spec
/// node/edge output is then RESHAPED into `@graph` nodes with the
/// `supported_by` edge FOLDED onto its `Claim` node as a JSON-LD object
/// reference (idiomatic in an RO-Crate `@graph`, mirroring
/// [`project_audit_proof_jsonld`]'s `evaluated_against` fold).
///
/// Each verdict becomes one node:
///
/// ```json
/// { "@id": "C:differential_expression_claim-0",
///   "@type": "Claim",
///   "status": "verified",
///   "text": "",
///   "supported_by": [ { "@id": "V:differential_expression_tsv" } ] }
/// ```
///
/// Deterministic: node `@id`s derive from the verdict `claim_id`; no wall-clock
/// value enters, so re-injection keeps `ro-crate-metadata.json` reproducible.
pub fn project_claim_jsonld(claims: &Value) -> Vec<Value> {
    let pkg = LoadedPackage {
        claims: Some(claims.clone()),
        ..LoadedPackage::default()
    };
    // Reuse the canonical projector. It returns interleaved spec node values
    // (`{id, type, props}`) and spec edge values (`{source_id, target_id,
    // predicate}`); fold the edges onto their source node.
    let spec = project_claim_subgraph(&pkg);
    let mut nodes: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    let mut edges: Vec<(String, String, String)> = Vec::new();
    for item in spec {
        if let (Some(id), Some(ty)) = (
            item.get("id").and_then(Value::as_str),
            item.get("type").and_then(Value::as_str),
        ) {
            // A node: `{id, type, props}`. Reshape to `@graph` form, lifting the
            // structural props (text, status) to top-level keys.
            let mut node = json!({ "@id": id, "@type": ty });
            if let Some(props) = item.get("props").and_then(Value::as_object) {
                for (k, v) in props {
                    node[k] = v.clone();
                }
            }
            nodes.insert(id.to_string(), node);
        } else if let (Some(src), Some(tgt), Some(pred)) = (
            item.get("source_id").and_then(Value::as_str),
            item.get("target_id").and_then(Value::as_str),
            item.get("predicate").and_then(Value::as_str),
        ) {
            edges.push((src.to_string(), tgt.to_string(), pred.to_string()));
        }
    }
    // Fold each edge onto its source node as a JSON-LD object reference under
    // the predicate key (e.g. `supported_by: [{ "@id": "V:…" }]`).
    for (src, tgt, pred) in edges {
        let Some(node) = nodes.get_mut(&src) else {
            continue;
        };
        let arr = node
            .as_object_mut()
            .expect("node is an object literal")
            .entry(pred)
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(a) = arr.as_array_mut() {
            a.push(json!({ "@id": tgt }));
        }
    }
    nodes.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_types::consts::{EDGE_PREDICATES, NODE_TYPES};

    /// Enumerate every node type so the drift check below is exhaustive.
    const ALL_NODE_TYPES: &[SpecNodeType] = &[
        SpecNodeType::Question,
        SpecNodeType::Cohort,
        SpecNodeType::Contrast,
        SpecNodeType::Modality,
        SpecNodeType::ExpectedOutput,
        SpecNodeType::MethodChoice,
        SpecNodeType::Justification,
        SpecNodeType::Alternative,
        SpecNodeType::Citation,
        SpecNodeType::WorkflowStep,
        SpecNodeType::Container,
        SpecNodeType::InputFile,
        SpecNodeType::OutputFile,
        SpecNodeType::RuntimeEnvironment,
        SpecNodeType::Table,
        SpecNodeType::Figure,
        SpecNodeType::Statistic,
        SpecNodeType::File,
        SpecNodeType::Claim,
        SpecNodeType::Quantification,
        SpecNodeType::Direction,
        SpecNodeType::RerunOutcome,
        SpecNodeType::Blocker,
        SpecNodeType::RecoveryAction,
        SpecNodeType::InvariantVerdict,
    ];

    const ALL_PREDICATES: &[SpecPredicate] = &[
        SpecPredicate::Refines,
        SpecPredicate::Stratifies,
        SpecPredicate::Expects,
        SpecPredicate::Chooses,
        SpecPredicate::Rejects,
        SpecPredicate::Cites,
        SpecPredicate::Amends,
        SpecPredicate::WasDerivedFrom,
        SpecPredicate::Produces,
        SpecPredicate::Consumes,
        SpecPredicate::RunsIn,
        SpecPredicate::AppearsIn,
        SpecPredicate::ComputedFrom,
        SpecPredicate::SupportedBy,
        SpecPredicate::Contradicts,
        SpecPredicate::EquivalentTo,
        SpecPredicate::DivergesFrom,
        SpecPredicate::Requires,
        SpecPredicate::Unblocks,
        SpecPredicate::EvaluatedAgainst,
    ];

    /// The projection enums MUST be a 1:1, same-order mirror of the
    /// canonical closed sets. This is the in-crate half of the contract;
    /// the schema half is asserted by `schemars_generation.rs`.
    #[test]
    fn node_type_enum_matches_consts() {
        let wire: Vec<&str> = ALL_NODE_TYPES.iter().map(|n| n.as_str()).collect();
        let expected: Vec<&str> = NODE_TYPES.to_vec();
        assert_eq!(
            wire, expected,
            "SpecNodeType drifted from consts::NODE_TYPES"
        );
    }

    #[test]
    fn predicate_enum_matches_consts() {
        let wire: Vec<&str> = ALL_PREDICATES.iter().map(|p| p.as_str()).collect();
        let expected: Vec<&str> = EDGE_PREDICATES.to_vec();
        assert_eq!(
            wire, expected,
            "SpecPredicate drifted from consts::EDGE_PREDICATES"
        );
    }

    #[test]
    fn node_to_value_prefix_tags_id() {
        let node = SpecNode::new("q_1", SpecNodeType::Question).with_prop("text", json!("hi"));
        let v = node.to_value('I');
        assert_eq!(v["id"], json!("I:q_1"));
        assert_eq!(v["type"], json!("Question"));
        assert_eq!(v["props"]["text"], json!("hi"));
    }

    #[test]
    fn edge_local_target_is_prefixed_crossgraph_is_not() {
        let local = SpecEdge::new("a", "b", SpecPredicate::ComputedFrom).to_value('V');
        assert_eq!(local["source_id"], json!("V:a"));
        assert_eq!(local["target_id"], json!("V:b"));
        assert_eq!(local["predicate"], json!("computed_from"));

        let cross = SpecEdge::new("c", "V:fig_1", SpecPredicate::SupportedBy).to_value('C');
        assert_eq!(cross["source_id"], json!("C:c"));
        assert_eq!(
            cross["target_id"],
            json!("V:fig_1"),
            "already-tagged target kept"
        );
    }

    #[test]
    fn is_prefix_tagged_recognizes_the_eight_letters() {
        assert!(is_prefix_tagged("V:fig_1"));
        assert!(is_prefix_tagged("A:claim_completeness"));
        assert!(!is_prefix_tagged("fig_1"));
        assert!(!is_prefix_tagged("X:foo"));
        assert!(!is_prefix_tagged("V:"));
    }

    #[test]
    fn decision_record_projects_method_choice() {
        use crate::decision_log::{DecisionActor, DecisionRecord, DecisionType};
        let rec = DecisionRecord::new(
            "s-1",
            DecisionType::SetIntakeMethod {
                stage: "differential_expression".into(),
                method_prose: "DESeq2 chosen per protocol; meets the 30-char minimum.".into(),
            },
            DecisionActor::Sme,
            None,
        );
        let (nodes, edges) = rec.project();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].node_type, SpecNodeType::MethodChoice);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].predicate, SpecPredicate::Chooses);
    }

    #[test]
    fn decision_record_ignores_non_method_kinds() {
        use crate::decision_log::{DecisionActor, DecisionRecord, DecisionType};
        let rec = DecisionRecord::new(
            "s-1",
            DecisionType::Confirm { summary_hash: None },
            DecisionActor::Sme,
            None,
        );
        let (nodes, edges) = rec.project();
        assert!(nodes.is_empty());
        assert!(edges.is_empty());
    }

    #[test]
    fn edge_contract_projects_workflow_step_and_consumes() {
        use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
        let ec = EdgeContract {
            from_node: "counts".into(),
            from_port: "output".into(),
            to_node: "differential_expression".into(),
            to_port: "input".into(),
            proof: CompatibilityProof::default(),
            kind: EdgeKind::TypedDataFlow,
            chain_of_custody: None,
        };
        let (nodes, edges) = ec.project();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].node_type, SpecNodeType::WorkflowStep);
        assert_eq!(nodes[0].id, "differential_expression");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].predicate, SpecPredicate::Consumes);
        assert_eq!(edges[0].target_id, "counts");
    }

    #[test]
    fn audit_proof_jsonld_projects_typed_verdict_nodes() {
        let report = json!({
            "package_iri": "ro-crate-metadata.json",
            "evaluated_at": "2026-06-02T00:00:00Z",
            "verdicts": [
                {"id": "claim_completeness", "status": "pass"},
                {"id": "decision_justification", "status": "fail"},
            ],
        });
        let nodes = project_audit_proof_jsonld(&report);
        assert_eq!(nodes.len(), 2);
        // Sorted by @id (claim_completeness < decision_justification).
        assert_eq!(nodes[0]["@id"], json!("A:claim_completeness"));
        assert_eq!(nodes[0]["@type"], json!("InvariantVerdict"));
        assert_eq!(nodes[0]["verdict"], json!("pass"));
        assert_eq!(nodes[0]["invariant_id"], json!("claim_completeness"));
        assert_eq!(
            nodes[0]["evaluated_against"],
            json!({"@id": "ro-crate-metadata.json"})
        );
        // The non-deterministic evaluated_at MUST NOT leak into the node body.
        assert!(nodes[0].get("evaluated_at").is_none());
        assert_eq!(nodes[1]["verdict"], json!("fail"));
    }

    #[test]
    fn audit_proof_jsonld_empty_report_yields_no_nodes() {
        assert!(project_audit_proof_jsonld(&json!({})).is_empty());
        assert!(project_audit_proof_jsonld(&json!({"verdicts": []})).is_empty());
    }

    /// Build a `LoadedPackage` carrying RO-Crate output entities + bare-EdgeContract
    /// proofs (the production conversation-path shape).
    fn pkg_with_outputs_and_proofs(
        output_entities: Vec<Value>,
        proofs: Vec<Value>,
    ) -> LoadedPackage {
        LoadedPackage {
            proofs,
            output_entities,
            ..Default::default()
        }
    }

    #[test]
    fn evidence_subgraph_materializes_from_output_entities_not_proofs() {
        // The V sub-graph must come from the RO-Crate output entities (declared
        // figure obligations / produced files), NOT from the E-backed
        // proofs.jsonl EdgeContract rows. Here proofs carries a bare
        // producer→consumer EdgeContract (the production conversation-path
        // shape, no `computed_from`) — V must still be non-empty because the
        // RO-Crate declares an ImageObject output.
        let pkg = pkg_with_outputs_and_proofs(
            vec![json!({
                "@id": "runtime/outputs/differential_expression/figures/volcano.png",
                "@type": ["File", "ImageObject"]
            })],
            vec![json!({
                "from_node": "counts", "from_port": "out",
                "to_node": "differential_expression", "to_port": "in",
                "proof": {}
            })],
        );
        let v = project_evidence_subgraph(&pkg);
        let nodes: Vec<&Value> = v.iter().filter(|x| x.get("type").is_some()).collect();
        assert_eq!(nodes.len(), 1, "one V node per output entity; got {v:?}");
        assert_eq!(
            nodes[0]["type"],
            json!("Figure"),
            "ImageObject → Figure node"
        );
        assert!(
            nodes[0]["id"].as_str().unwrap().starts_with("V:"),
            "V node id prefix-tagged"
        );
        // The V node carries a `computed_from` edge to the producing E step.
        let edges: Vec<&Value> = v.iter().filter(|x| x.get("predicate").is_some()).collect();
        assert!(
            edges
                .iter()
                .any(|e| e["predicate"] == json!("computed_from")),
            "each V Figure has a computed_from edge to its E producer; got {v:?}"
        );
    }

    #[test]
    fn evidence_subgraph_not_identical_to_execution_rows() {
        use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract};
        // V (Evidence) and E (Execution) must NOT be backed by the same rows:
        // E projects from the proofs EdgeContract, V from the output entity.
        let edge = serde_json::to_value(EdgeContract {
            from_node: "data_acquisition".into(),
            from_port: "out".into(),
            to_node: "qc".into(),
            to_port: "in".into(),
            kind: crate::workflow_contracts::edge::EdgeKind::Unproven,
            proof: CompatibilityProof::default(),
            chain_of_custody: None,
        })
        .unwrap();
        let pkg = pkg_with_outputs_and_proofs(
            vec![json!({
                "@id": "runtime/outputs/qc/figures/per_sample_metric_bar.png",
                "@type": ["File", "ImageObject"]
            })],
            vec![edge],
        );
        let v = project_subgraph('V', &pkg);
        let e = project_subgraph('E', &pkg);
        assert_ne!(v, e, "V and E must be distinct sub-graphs");
        // E projects the WorkflowStep; V projects the Figure.
        assert!(e
            .iter()
            .any(|n| n.get("type").and_then(Value::as_str) == Some("WorkflowStep")));
        assert!(v
            .iter()
            .any(|n| n.get("type").and_then(Value::as_str) == Some("Figure")));
    }

    #[test]
    fn evidence_subgraph_empty_when_no_output_entities() {
        // No RO-Crate output entities and no real-path proofs outputs → empty V
        // (schema-valid; the pre-execution minimal-package case).
        let pkg = pkg_with_outputs_and_proofs(vec![], vec![]);
        assert!(project_evidence_subgraph(&pkg).is_empty());
    }

    #[test]
    fn assumption_projects_blocker() {
        use crate::workflow_contracts::evidence::{Assumption, AssumptionSource, RiskClass};
        let a = Assumption {
            id: "a_1".into(),
            statement: "Assumed default normalization.".into(),
            source: AssumptionSource::LlmInferred {
                confidence: "0.8".into(),
            },
            affects_nodes: vec![],
            risk: RiskClass::Low,
            resolution: Default::default(),
            chain_of_custody: None,
        };
        let (nodes, edges) = a.project();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].node_type, SpecNodeType::Blocker);
        assert!(edges.is_empty());
    }
}
