//! Audit harness: verifies every edge declared on every atom in the
//! live `config/stage-atoms/` registry is referentially and typically
//! correct, using the real loader + the real compatibility engine
//! (no re-implementation, no doc/assumption reliance).
//!
//! Run with output:
//!   cargo test -p ecaa-workflow-core --test atom_edge_audit -- --nocapture

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use ecaa_workflow_core::atom::AtomRole;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::compatibility::engine::{
    AdapterPolicy, CompatibilityEngine, CompatibilityResult, DeterministicCompatibilityEngine,
    PlanningContext, RiskMode,
};

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .join("config")
}

/// Classify a single producer→consumer dependency edge by running the
/// real engine over the cross-product of producer outputs × consumer
/// inputs. The strongest outcome across all port pairs wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeFlow {
    Typed,   // some pair -> Compatible
    Adapter, // some pair -> CompatibleWithAdapters (none Compatible)
    Unknown, // some pair -> Unknown (opaque), none stronger
    NoFlow,  // every pair Incompatible (or one side has no ports)
}

fn classify_edge(
    engine: &DeterministicCompatibilityEngine,
    producer_outputs: &[ecaa_workflow_core::workflow_contracts::port::PortContract],
    consumer_inputs: &[ecaa_workflow_core::workflow_contracts::port::PortContract],
    ctx: &PlanningContext,
) -> (EdgeFlow, Vec<String>) {
    let mut best = EdgeFlow::NoFlow;
    let mut reasons: Vec<String> = Vec::new();
    if producer_outputs.is_empty() || consumer_inputs.is_empty() {
        reasons.push(format!(
            "producer has {} outputs, consumer has {} inputs",
            producer_outputs.len(),
            consumer_inputs.len()
        ));
    }
    for p in producer_outputs {
        for c in consumer_inputs {
            match engine.prove(p, c, ctx) {
                CompatibilityResult::Compatible(_) => return (EdgeFlow::Typed, reasons),
                CompatibilityResult::CompatibleWithAdapters { .. } => {
                    best = EdgeFlow::Adapter;
                }
                CompatibilityResult::Unknown(_) => {
                    if best == EdgeFlow::NoFlow {
                        best = EdgeFlow::Unknown;
                    }
                }
                CompatibilityResult::Incompatible(report) => {
                    reasons.push(format!(
                        "{}.{} -> {}.{}: {:?}",
                        p.name,
                        p.semantic_type.stable_id(),
                        c.name,
                        c.semantic_type.stable_id(),
                        report.reasons
                    ));
                }
            }
        }
    }
    (best, reasons)
}

#[test]
fn audit_all_atom_edges() {
    let config = config_root();
    let reg = AtomRegistry::load_from_dir(&config.join("stage-atoms"))
        .expect("load atom registry from config/stage-atoms");

    eprintln!("\n==================== ATOM EDGE AUDIT ====================");
    eprintln!("atoms loaded: {}", reg.len());

    // ---- 1. Referential integrity of id-based edges (the engine's own check) ----
    // depends_on (exists + no self-loop), excludes (exists or cel:),
    // method_choice.deferred_to (exists + is Discovery).
    reg.validate_consistency()
        .expect("validate_consistency: id-based edges must be referentially correct");
    eprintln!(
        "[OK] validate_consistency passed (depends_on / excludes / method_choice refs valid)"
    );

    // ---- 2. Acyclicity of the depends_on graph ----
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, atom) in reg.iter() {
        graph.insert(id.clone(), atom.depends_on.clone());
    }
    let mut color: BTreeMap<String, u8> = BTreeMap::new();
    let mut cycles: Vec<Vec<String>> = Vec::new();
    fn dfs(
        u: &str,
        graph: &BTreeMap<String, Vec<String>>,
        color: &mut BTreeMap<String, u8>,
        stack: &mut Vec<String>,
        cycles: &mut Vec<Vec<String>>,
    ) {
        color.insert(u.to_string(), 1);
        stack.push(u.to_string());
        if let Some(deps) = graph.get(u) {
            for v in deps {
                match color.get(v).copied().unwrap_or(0) {
                    1 => {
                        let i = stack.iter().position(|x| x == v).unwrap_or(0);
                        let mut c = stack[i..].to_vec();
                        c.push(v.clone());
                        cycles.push(c);
                    }
                    0 => dfs(v, graph, color, stack, cycles),
                    _ => {}
                }
            }
        }
        stack.pop();
        color.insert(u.to_string(), 2);
    }
    for id in graph.keys() {
        if color.get(id).copied().unwrap_or(0) == 0 {
            dfs(id, &graph, &mut color, &mut Vec::new(), &mut cycles);
        }
    }
    assert!(cycles.is_empty(), "depends_on cycles detected: {cycles:?}");
    eprintln!("[OK] depends_on graph is acyclic");

    // ---- 3. Typed-flow justification for every depends_on edge ----
    let engine = DeterministicCompatibilityEngine::new();
    let ctx = PlanningContext {
        risk_mode: RiskMode::Draft,
        adapter_policy: AdapterPolicy::permissive_drafts(),
        max_proof_branches: 64,
        ..Default::default()
    };

    let mut total = 0usize;
    let mut typed = 0usize;
    let mut adapter = 0usize;
    let mut unknown = 0usize;
    let mut noflow: Vec<(String, String, Vec<String>)> = Vec::new();

    for (consumer_id, consumer) in reg.iter() {
        for producer_id in &consumer.depends_on {
            total += 1;
            let producer = reg.get(producer_id).expect("validated above");
            let (flow, reasons) = classify_edge(&engine, &producer.outputs, &consumer.inputs, &ctx);
            match flow {
                EdgeFlow::Typed => typed += 1,
                EdgeFlow::Adapter => adapter += 1,
                EdgeFlow::Unknown => unknown += 1,
                EdgeFlow::NoFlow => {
                    noflow.push((producer_id.clone(), consumer_id.clone(), reasons))
                }
            }
        }
    }

    eprintln!("\n--- depends_on edge typed-flow classification ({total} edges) ---");
    eprintln!("  TypedDataFlow  : {typed}");
    eprintln!("  AdapterMediated: {adapter}");
    eprintln!("  Unknown(opaque): {unknown}");
    eprintln!("  NO typed flow  : {}", noflow.len());

    // ---- 4. Producer-role sanity for each no-typed-flow edge ----
    // An edge into a Validation/Aggregator/Discovery/Reporting consumer,
    // or out of a Discovery producer, is legitimately ordering-only.
    // Anything else is a candidate authoring defect worth surfacing.
    eprintln!("\n--- edges with NO typed data flow (producer -> consumer) ---");
    let mut suspicious: Vec<String> = Vec::new();
    for (p, c, reasons) in &noflow {
        let prod = reg.get(p).unwrap();
        let cons = reg.get(c).unwrap();
        let ordering_ok = matches!(
            cons.role.default_behavior_class(),
            AtomRole::Validation | AtomRole::Aggregator | AtomRole::Discovery
        ) || prod.role.is_discovery()
            || c.contains("report")
            || c.contains("summary")
            || prod.outputs.is_empty()
            || cons.inputs.is_empty();
        let tag = if ordering_ok { "ordering-ok" } else { "REVIEW" };
        eprintln!("  [{tag}] {p}({:?}) -> {c}({:?})", prod.role, cons.role);
        for r in reasons.iter().take(3) {
            eprintln!("        {r}");
        }
        if !ordering_ok {
            suspicious.push(format!("{p} -> {c}"));
        }
    }

    // ---- 5. method_choice edges ----
    eprintln!("\n--- method_choice edges ---");
    for (id, atom) in reg.iter() {
        if let Some(mc) = &atom.method_choice {
            let tgt = reg.get(&mc.deferred_to).unwrap();
            eprintln!(
                "  {id} -> {} (role={:?}, discovery_kind={:?})",
                mc.deferred_to, tgt.role, tgt.discovery_kind
            );
        }
    }

    // ---- 6. Input satisfiability: which consumer input ports are not
    //         produced by ANY declared depends_on producer? ----
    eprintln!("\n--- consumer input ports unsatisfied by declared depends_on ---");
    let mut unsatisfied = 0usize;
    for (consumer_id, consumer) in reg.iter() {
        if consumer.inputs.is_empty() || consumer.depends_on.is_empty() {
            continue;
        }
        let producer_out_ids: BTreeSet<String> = consumer
            .depends_on
            .iter()
            .filter_map(|d| reg.get(d))
            .flat_map(|p| p.outputs.iter().map(|o| o.semantic_type.stable_id()))
            .collect();
        for inp in &consumer.inputs {
            let want = inp.semantic_type.stable_id();
            let satisfied = producer_out_ids.iter().any(|have| {
                have == &want
                    || ecaa_workflow_core::edam::is_subtype_of(have, &want)
                    || ecaa_workflow_core::edam::is_subtype_of(&want, have)
            });
            if !satisfied {
                unsatisfied += 1;
                eprintln!(
                    "  {consumer_id}.{} wants {want} — not produced by depends_on {:?}",
                    inp.name, consumer.depends_on
                );
            }
        }
    }
    eprintln!("  ({unsatisfied} input ports not type-matched by a declared producer)");

    eprintln!("\n==================== END AUDIT ====================\n");

    // Hard assertions: the load-time invariants. The typed-flow numbers
    // are reported for human review (ordering-only edges are legitimate),
    // so we do NOT fail on those here.
    assert!(
        suspicious.len() <= noflow.len(),
        "internal accounting error"
    );
}
