//! Deposit readability transforms, applied to the *exported* package (not the
//! emitted source tree) so they benefit the published deposit without a
//! re-run. Three transforms, all pure functions of files already in `dst`:
//!
//!  1. [`reorder_workflow_tasks`] — re-emit `WORKFLOW.json` with the `tasks`
//!     object's members in dependency (execution) order instead of the
//!     alphabetical order the derived serializer produces, and with the big
//!     `tasks` block moved last. A reviewer reads the steps top-to-bottom in
//!     the order they ran. **Execution-safe by construction**: the harness
//!     deserializes `tasks` into a `BTreeMap` (re-sorting keys) and drives
//!     execution from the separate `execution_order` vector + `depends_on`
//!     edges, so file key order is irrelevant to the run. A round-trip check
//!     guarantees the rewrite changes only key order, never data.
//!  2. [`write_artifacts_manifest`] — `ARTIFACTS.md`, a per-step output map:
//!     every result/figure/sidecar grouped by the step that produced it, with
//!     sizes and a one-line kind, so a reviewer can find any file by stage.
//!  3. [`augment_readme`] — append a "Package navigation" section to
//!     `README.md` with the inline execution-order table and direct links to
//!     `AUDIT-REPORT.md`, `ARTIFACTS.md`, `EXECUTION-ORDER.md`, the narrative
//!     report, and the audit sidecars.
//!
//! All three are best-effort and idempotent: a missing input or a sanity-check
//! failure leaves the file untouched rather than corrupting the deposit.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

/// Human-readable byte size, deterministic (no locale).
fn human_size(bytes: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", U[i])
}

/// One-line kind label from a file extension (for the artifact map).
fn kind_of(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "tsv" | "csv" => "table",
        "png" | "svg" | "pdf" | "jpg" | "jpeg" => "figure",
        "json" => "data / provenance",
        "jsonl" => "provenance log",
        "md" => "report",
        "txt" => "text",
        "log" => "log",
        "html" => "html",
        "ttl" => "RDF graph",
        "py" | "r" => "script",
        _ => "file",
    }
}

// ── 1. WORKFLOW.json task reorder ────────────────────────────────────────────

/// Re-emit `dst/WORKFLOW.json` with `tasks` in execution order, tasks-block
/// last. No-op (returns `Ok`) if the file is absent, malformed, has no
/// `execution_order`, or if the rewrite would not round-trip to the same value.
pub(super) fn reorder_workflow_tasks(dst: &Path) -> Result<()> {
    let path = dst.join("WORKFLOW.json");
    let raw = match std::fs::read(&path) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    let root: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let obj = match root.as_object() {
        Some(o) => o,
        None => return Ok(()),
    };
    let tasks = match obj.get("tasks").and_then(Value::as_object) {
        Some(t) if !t.is_empty() => t,
        _ => return Ok(()),
    };
    let exec_order: Vec<String> = obj
        .get("execution_order")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if exec_order.is_empty() {
        return Ok(());
    }

    // Ordered keys: execution order first (present tasks only), then any task
    // not named in execution_order, alphabetically (tasks is a BTreeMap-backed
    // object so its iteration is already sorted).
    let mut ordered: Vec<&String> = exec_order
        .iter()
        .filter(|k| tasks.contains_key(*k))
        .collect();
    for k in tasks.keys() {
        if !exec_order.contains(k) {
            ordered.push(k);
        }
    }
    // Safety: must be a permutation of exactly the task keys.
    if ordered.len() != tasks.len() {
        return Ok(());
    }

    let out = serialize_workflow_ordered(obj, tasks, &ordered)
        .context("assembling reordered WORKFLOW.json")?;

    // Adversarial safety net: the rewrite must parse back to a value EQUAL to
    // the original. serde_json's Map is a BTreeMap, so equality ignores key
    // order — it can only hold if every key and value survived unchanged.
    let reparsed: Value =
        serde_json::from_str(&out).context("re-parsing reordered WORKFLOW.json")?;
    if reparsed != root {
        anyhow::bail!(
            "reordered WORKFLOW.json is not value-equal to the original; refusing to write"
        );
    }

    std::fs::write(&path, out).with_context(|| format!("writing reordered {}", path.display()))?;
    Ok(())
}

/// Assemble the WORKFLOW.json text: all non-`tasks` top-level keys first (in the
/// object's natural sorted order, each value serde-pretty), then the `tasks`
/// object last with its members in `ordered`. Values come verbatim from serde
/// (correct escaping/number formatting); only object/member *order* is ours.
fn serialize_workflow_ordered(
    obj: &serde_json::Map<String, Value>,
    tasks: &serde_json::Map<String, Value>,
    ordered: &[&String],
) -> Result<String> {
    let mut out = String::from("{\n");
    for (k, v) in obj.iter().filter(|(k, _)| k.as_str() != "tasks") {
        let key = serde_json::to_string(k)?;
        let val = serde_json::to_string_pretty(v)?.replace('\n', "\n  ");
        let _ = writeln!(out, "  {key}: {val},");
    }
    out.push_str("  \"tasks\": {\n");
    for (i, k) in ordered.iter().enumerate() {
        let key = serde_json::to_string(k)?;
        let val = serde_json::to_string_pretty(&tasks[*k])?.replace('\n', "\n    ");
        let _ = write!(out, "    {key}: {val}");
        if i + 1 < ordered.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  }\n}\n");
    Ok(out)
}

// ── 2. ARTIFACTS.md per-step output manifest ─────────────────────────────────

/// Write `dst/ARTIFACTS.md` — a per-step map of the package's files. Best-effort;
/// degrades to a flat listing if `execution-order.json` is absent.
pub(super) fn write_artifacts_manifest(dst: &Path) -> Result<()> {
    let rt = dst.join("runtime");
    let mut m = String::new();
    let _ = writeln!(m, "# Artifact map\n");
    let _ = writeln!(
        m,
        "Every file in this package, grouped by the step that produced it, with \
         size and kind. This is the \"where is each file\" index; the scientific \
         narrative is in `final_report.md`, the audit/provenance surface in \
         `AUDIT-REPORT.md`, and the run order in `EXECUTION-ORDER.md`.\n"
    );

    // Per-step outputs, in execution order.
    let order = read_json(&rt.join("execution-order.json"))
        .and_then(|eo| eo.get("order").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    if !order.is_empty() {
        let _ = writeln!(m, "## Per-step outputs\n");
        for step in &order {
            let idx = step.get("index").and_then(Value::as_u64).unwrap_or(0);
            let tid = step.get("task_id").and_then(Value::as_str).unwrap_or("");
            let odir = step.get("output_dir").and_then(Value::as_str).unwrap_or("");
            let files = list_files(&dst.join(odir));
            if files.is_empty() {
                let _ = writeln!(
                    m,
                    "### {idx:02} · `{tid}`\n\n_No output files retained in this profile._\n"
                );
                continue;
            }
            let _ = writeln!(m, "### {idx:02} · `{tid}`\n");
            let _ = writeln!(m, "| File | Size | Kind |");
            let _ = writeln!(m, "| :---- | ----: | :---- |");
            for (rel, size) in files {
                let _ = writeln!(m, "| `{rel}` | {} | {} |", human_size(size), kind_of(&rel));
            }
            let _ = writeln!(m);
        }
    }

    // Package-level + provenance files (everything not under runtime/outputs/).
    let mut top: Vec<(String, u64)> = list_files(dst)
        .into_iter()
        .filter(|(rel, _)| !rel.starts_with("runtime/outputs/"))
        .collect();
    top.sort();
    if !top.is_empty() {
        let _ = writeln!(m, "## Package-level & provenance files\n");
        let _ = writeln!(m, "| File | Size | Kind |");
        let _ = writeln!(m, "| :---- | ----: | :---- |");
        for (rel, size) in top {
            let _ = writeln!(m, "| `{rel}` | {} | {} |", human_size(size), kind_of(&rel));
        }
        let _ = writeln!(m);
    }

    let _ = writeln!(
        m,
        "---\n_Generated by `ecaa-workflow` from the exported deposit tree._"
    );
    std::fs::write(dst.join("ARTIFACTS.md"), m)
        .with_context(|| format!("writing artifact map under {}", dst.display()))?;
    Ok(())
}

/// Files (recursive) under `dir`, as `(package-relative path, size)`, sorted.
/// `dir` is a subtree of the package; paths are returned relative to the
/// package root (i.e. relative to `dir`'s nearest ancestor we don't know here,
/// so callers pass the package root for top-level and an output dir for steps).
fn list_files(dir: &Path) -> Vec<(String, u64)> {
    // We want package-relative paths. Recover the package root by walking up:
    // callers pass either the package root itself or `<root>/<output_dir>`.
    // Simpler + robust: compute paths relative to `dir`, then the caller's
    // table heading already names the step's directory. But the README/links
    // want package-relative paths, so for the per-step tables we prefix the
    // step dir. We therefore return paths relative to `dir` and let the caller
    // not prefix (the step heading carries the dir). For the top-level listing
    // `dir` IS the package root, so relative == package-relative. To keep both
    // correct we always return package-relative by detecting the package root
    // as the first ancestor containing `WORKFLOW.json` or the checksum
    // manifest.
    let root = package_root(dir).unwrap_or_else(|| dir.to_path_buf());
    let mut out = Vec::new();
    for e in walkdir::WalkDir::new(dir).sort_by_file_name() {
        let Ok(e) = e else { continue };
        if !e.file_type().is_file() {
            continue;
        }
        let size = e.metadata().map(|md| md.len()).unwrap_or(0);
        let rel = e
            .path()
            .strip_prefix(&root)
            .unwrap_or(e.path())
            .to_string_lossy()
            .replace('\\', "/");
        out.push((rel, size));
    }
    out.sort();
    out
}

/// Nearest ancestor (inclusive) that looks like a package root.
fn package_root(dir: &Path) -> Option<std::path::PathBuf> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.join("WORKFLOW.json").exists() || d.join("manifest-sha512.txt").exists() {
            return Some(d.to_path_buf());
        }
        cur = d.parent();
    }
    None
}

// ── 3. README navigation section ─────────────────────────────────────────────

/// Append a "Package navigation" section to `dst/README.md` with the inline
/// execution-order table and direct links to the audit/provenance artifacts.
/// Idempotent (skips if already present); no-op if README is absent.
pub(super) fn augment_readme(dst: &Path) -> Result<()> {
    let path = dst.join("README.md");
    let mut text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };
    if text.contains("## Package navigation") {
        return Ok(());
    }
    let nav = build_nav_section(dst);

    // Insert before the generated-by footer if present, else before the
    // re-run section, else append.
    if let Some(idx) = text.find("\n_Generated deterministically") {
        text.insert_str(idx + 1, &nav);
    } else if let Some(idx) = text.find("\n## 4. Re-run it") {
        text.insert_str(idx + 1, &nav);
    } else {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&nav);
    }
    std::fs::write(&path, text).with_context(|| format!("writing augmented {}", path.display()))?;
    Ok(())
}

// ── 4. Register kept-but-dark payload into the @graph (zero dark payload) ────

/// Checksum-seal / RO-Crate structural files that are not RO-Crate data entities and
/// so are never registered in the `@graph`.
const STRUCTURAL: &[&str] = &[
    "ro-crate-metadata.json",
    "ro-crate-preview.html",
    "seal-info.json",
    "seal-tagmanifest-sha512.txt",
    "bagit.txt",
    "bag-info.txt",
    "manifest-sha512.txt",
    "tagmanifest-sha512.txt",
];

/// Register every kept payload file that is not yet an `@graph` entity as a
/// `File` node in the root Dataset's `hasPart`, so the deposit carries **no dark
/// payload** — every content file on disk is declared in the RO-Crate, matching
/// the discipline of the most detailed real-world WRROC crates (which register
/// every file they include). Per-task outputs are linked (`about`) to their
/// `#step-<task>` node when one exists. Returns the number of entities added.
/// Runs last, after every other file (reordered WORKFLOW.json, ARTIFACTS.md,
/// AUDIT-REPORT.md, README) exists, so all are captured.
pub(super) fn register_deposit_entities(dst: &Path) -> Result<usize> {
    let meta_path = dst.join("ro-crate-metadata.json");
    let raw = match std::fs::read(&meta_path) {
        Ok(r) => r,
        Err(_) => return Ok(0),
    };
    let mut doc: Value = serde_json::from_slice(&raw).context("parsing ro-crate-metadata.json")?;
    let graph = match doc.get_mut("@graph").and_then(Value::as_array_mut) {
        Some(g) => g,
        None => return Ok(0),
    };
    let existing: HashSet<String> = graph
        .iter()
        .filter_map(|e| e.get("@id").and_then(Value::as_str).map(String::from))
        .collect();

    // Collect kept files not yet declared, excluding the structural set.
    let mut new_nodes: Vec<Value> = Vec::new();
    let mut new_haspart: Vec<Value> = Vec::new();
    for e in walkdir::WalkDir::new(dst).sort_by_file_name() {
        let Ok(e) = e else { continue };
        if !e.file_type().is_file() {
            continue;
        }
        let rel = match e.path().strip_prefix(dst) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        if STRUCTURAL.contains(&rel.as_str()) || existing.contains(&rel) {
            continue;
        }
        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
        let (name, desc, about) = describe(&rel, &existing);
        // No `sha512` here: the BagIt `manifest-sha512.txt` (re-sealed after this
        // step) is the authoritative integrity layer over every file, and some of
        // these files (AUDIT-REPORT.md) are regenerated after registration — a
        // recorded `@graph` hash would go stale. `contentSize` + `encodingFormat`
        // keep the node descriptive.
        let node = serde_json::json!({
            "@id": rel,
            "@type": file_type(&rel),
            "name": name,
            "description": desc,
            "encodingFormat": mime(&rel),
            "contentSize": size,
            "about": { "@id": about },
        });
        new_nodes.push(node);
        new_haspart.push(serde_json::json!({ "@id": rel }));
    }
    if new_nodes.is_empty() {
        return Ok(0);
    }
    let added = new_nodes.len();

    // Extend the root Dataset's hasPart, then append the new File nodes.
    for e in graph.iter_mut() {
        if e.get("@id").and_then(Value::as_str) == Some("./") {
            match e.get_mut("hasPart").and_then(Value::as_array_mut) {
                Some(hp) => hp.append(&mut new_haspart),
                None => {
                    e["hasPart"] = Value::Array(std::mem::take(&mut new_haspart));
                }
            }
            break;
        }
    }
    graph.extend(new_nodes);

    let out = serde_json::to_string_pretty(&doc).context("serializing ro-crate-metadata.json")?;
    std::fs::write(&meta_path, out)
        .with_context(|| format!("writing registered {}", meta_path.display()))?;
    Ok(added)
}

fn file_type(rel: &str) -> Value {
    let ext = rel.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "svg" => serde_json::json!(["File", "ImageObject"]),
        _ => Value::String("File".into()),
    }
}

fn mime(rel: &str) -> &'static str {
    let ext = rel.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "json" => "application/json",
        "jsonl" => "application/jsonl",
        "md" => "text/markdown",
        "tsv" => "text/tab-separated-values",
        "csv" => "text/csv",
        "txt" => "text/plain",
        "png" => "image/png",
        "pdf" => "application/pdf",
        "svg" => "image/svg+xml",
        "html" => "text/html",
        "mac" => "application/octet-stream",
        _ => "application/octet-stream",
    }
}

/// `(name, description, about-@id)` for a file. Per-task files link to their
/// `#step-<task>` node when it exists; everything else is `about` the crate root.
fn describe(rel: &str, existing: &HashSet<String>) -> (String, String, String) {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    // Per-task: runtime/outputs/<task>/…
    let task = rel
        .strip_prefix("runtime/outputs/")
        .and_then(|s| s.split('/').next())
        .map(str::to_string);
    let about = match &task {
        Some(t) if existing.contains(&format!("#step-{t}")) => format!("#step-{t}"),
        _ => "./".to_string(),
    };
    let t = task.as_deref().unwrap_or("");
    let (name, desc) = match base {
        "agent-code.json" => (
            format!("{t} — agent code"),
            format!("Agent prompt, response, and executed code for stage '{t}'."),
        ),
        "result.json" => (
            format!("{t} — result record"),
            format!("Structured result record for stage '{t}'."),
        ),
        "validation_report.json" => (
            format!("{t} — validation report"),
            format!("Validation obligations and outcomes for stage '{t}'."),
        ),
        "decision.json" => (
            format!("{t} — method decision"),
            format!("Method-selection decision record for stage '{t}'."),
        ),
        "method_landscape.json" => (
            "Method landscape".into(),
            "Surveyed method landscape (axes + candidate methods) from method discovery.".into(),
        ),
        "curated_pools.json" => (
            "Curated candidate pools".into(),
            "Curated candidate method pools per analysis stage.".into(),
        ),
        "AUDIT-REPORT.md" => (
            "Audit & provenance report".into(),
            "Human-readable rendering of the audit/provenance sidecars.".into(),
        ),
        "ARTIFACTS.md" => (
            "Artifact map".into(),
            "Per-step map of every file in the package, with sizes.".into(),
        ),
        "EXECUTION-ORDER.md" => (
            "Execution order".into(),
            "The steps in dependency (execution) order.".into(),
        ),
        "SNAPSHOTS.md" => (
            "Evidence snapshots index".into(),
            "Index of the literature-evidence snapshots.".into(),
        ),
        "execution-order.json" => (
            "Execution order (machine-readable)".into(),
            "Topological execution order of the workflow steps.".into(),
        ),
        "repair-status.json" => (
            "Repair pass status".into(),
            "End-of-run repair pass verdict.".into(),
        ),
        b if b.ends_with(".signed.json") => (
            "Signed claim verification".into(),
            "Cryptographically signed claim-verification verdicts.".into(),
        ),
        b if b.ends_with(".mac") => (
            "Decisions log integrity tag".into(),
            "Detached integrity tag (MAC) over the decisions log.".into(),
        ),
        _ if rel.contains("/evidence/snapshots/") => (
            "Literature evidence snapshot".into(),
            "Content-addressed literature-evidence snapshot supporting the citation grounding."
                .into(),
        ),
        _ => (base.to_string(), format!("Package file '{rel}'.")),
    };
    (name, desc, about)
}

fn build_nav_section(dst: &Path) -> String {
    let rt = dst.join("runtime");
    let mut s = String::new();
    let _ = writeln!(s, "## Package navigation\n");

    // Direct links to the human-facing surfaces, listing only those present.
    let _ = writeln!(s, "| Read this | For |");
    let _ = writeln!(s, "| :---- | :---- |");
    let link = |s: &mut String, rel: &str, what: &str| {
        if dst.join(rel).exists() {
            let _ = writeln!(s, "| [`{rel}`]({rel}) | {what} |");
        }
    };
    // Narrative report can live at the package root or under a reporting step.
    if dst.join("final_report.md").exists() {
        link(
            &mut s,
            "final_report.md",
            "the scientific narrative — the answer",
        );
    }
    link(&mut s, "AUDIT-REPORT.md", "claim-verification verdicts, audit-proof invariants, decisions, assumptions, proofs, validation, cost — the accountability layer");
    link(
        &mut s,
        "ARTIFACTS.md",
        "every file mapped to the step that produced it, with sizes",
    );
    link(
        &mut s,
        "runtime/EXECUTION-ORDER.md",
        "the steps in dependency order",
    );
    link(
        &mut s,
        "ro-crate-metadata.json",
        "RO-Crate / Workflow-Run-Crate provenance metadata",
    );
    link(
        &mut s,
        "runtime/claim-verification.json",
        "machine-readable claim verdicts (source of AUDIT-REPORT.md)",
    );
    link(
        &mut s,
        "runtime/audit-proof-report.json",
        "machine-readable audit-proof invariants",
    );
    link(
        &mut s,
        "runtime/decisions.jsonl",
        "the decision log with authorities",
    );
    link(
        &mut s,
        "runtime/proofs.jsonl",
        "typed data-flow proofs, one per graph edge",
    );
    let _ = writeln!(s);

    // Inline execution-order table so the landing page itself shows the steps.
    let order = read_json(&rt.join("execution-order.json"))
        .and_then(|eo| eo.get("order").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    if !order.is_empty() {
        let _ = writeln!(s, "### Steps, in execution order\n");
        let _ = writeln!(s, "| # | Step | Outputs |");
        let _ = writeln!(s, "| ----: | :---- | :---- |");
        for step in &order {
            let idx = step.get("index").and_then(Value::as_u64).unwrap_or(0);
            let tid = step.get("task_id").and_then(Value::as_str).unwrap_or("");
            let odir = step.get("output_dir").and_then(Value::as_str).unwrap_or("");
            let _ = writeln!(s, "| {idx:02} | `{tid}` | [`{odir}`]({odir}) |");
        }
        let _ = writeln!(s);
    }
    s
}
