//! §3.16 — compact serialization for tabular tool-result payloads.
//!
//! Homogeneous JSON arrays-of-objects (e.g. metrics tables, claim rows)
//! carry enormous serialization overhead when re-encoded as pretty-
//! printed JSON: every key name repeats on every row. For large result
//! payloads that persist across tool-loop iterations, that overhead
//! dominates the per-turn token bill.
//!
//! This helper detects tabular shapes and renders them as a compact
//! CSV-like string. Non-tabular payloads fall through to
//! `serde_json::to_string`. The LLM reads both formats fine, and
//! third-party production measurement (Statsig blog, 2025) reports
//! 40–50% byte reduction on tabular payloads from the same swap.
//!
//! Heuristic — ONLY compact when:
//! 1. Top-level value is a non-empty `Value::Array`.
//! 2. Every element is a `Value::Object` with the same set of keys.
//! 3. Every value inside those objects is a primitive (string, number,
//!    bool, or null). Nested arrays/objects would lose fidelity in
//!    CSV and are better as JSON.
//!
//! Any deviation keeps the original JSON.

use serde_json::Value;

/// Return a compact string representation of `payload` suitable for a
/// tool_result content block. Tabular payloads render as CSV; everything
/// else round-trips through `serde_json::to_string`.
pub(crate) fn compact_tabular(payload: &Value) -> String {
    match try_csv(payload) {
        Some(csv) => csv,
        None => payload.to_string(),
    }
}

/// Plan S2.9 — soft cap on tool_result content sent to the LLM. A
/// runaway tool that returns megabytes of output would otherwise (a)
/// blow the token budget for the turn, (b) push real content out of
/// the context window when context-management triggers a clear, and
/// (c) silently increase chat_cost_usd by ~10× per iteration. We cap
/// at `TOOL_RESULT_SOFT_CAP_BYTES` and replace the tail with a
/// truncation marker. The full content stays in the in-memory
/// `ToolResult.content` for the audit log + decisions.jsonl; only the
/// LLM-facing tool_result block is truncated.
pub(crate) const TOOL_RESULT_SOFT_CAP_BYTES: usize = 100 * 1024;

/// Truncate `s` at `TOOL_RESULT_SOFT_CAP_BYTES` (counting bytes, not
/// chars — UTF-8 boundary safe). On truncation, append a marker that
/// tells the LLM the content was capped and points at the in-memory
/// audit log via the running tool's name + index for cross-reference.
pub(crate) fn cap_tool_result_length(s: String) -> String {
    if s.len() <= TOOL_RESULT_SOFT_CAP_BYTES {
        return s;
    }
    // Find a UTF-8 boundary at-or-below the cap so split_at doesn't
    // panic mid-codepoint.
    let mut cut = TOOL_RESULT_SOFT_CAP_BYTES;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let original_bytes = s.len();
    let mut truncated = s;
    truncated.truncate(cut);
    truncated.push_str(&format!(
        "\n\n[truncated: tool_result was {} bytes; capped at {}. \
         Full output preserved in the session audit log.]",
        original_bytes, TOOL_RESULT_SOFT_CAP_BYTES,
    ));
    truncated
}

fn try_csv(payload: &Value) -> Option<String> {
    let array = payload.as_array()?;
    if array.is_empty() {
        return None;
    }
    // Header = sorted key set of the first row. Every subsequent row
    // must have an identical key set (order-independent).
    let first = array.first()?.as_object()?;
    let mut header: Vec<&str> = first.keys().map(|s| s.as_str()).collect();
    header.sort_unstable();

    for row in array {
        let obj = row.as_object()?;
        if obj.len() != header.len() {
            return None;
        }
        for key in &header {
            let v = obj.get(*key)?;
            if !is_csv_primitive(v) {
                return None;
            }
        }
    }

    let mut out = String::new();
    out.push_str(&header.join(","));
    out.push('\n');
    for row in array {
        let obj = row.as_object()?;
        let cells: Vec<String> = header
            .iter()
            .map(|k| csv_cell(obj.get(*k).unwrap_or(&Value::Null)))
            .collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    Some(out)
}

fn is_csv_primitive(v: &Value) -> bool {
    matches!(
        v,
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null
    )
}

fn csv_cell(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => csv_quote(s),
        _ => v.to_string(),
    }
}

fn csv_quote(s: &str) -> String {
    // RFC 4180-style quoting: wrap in double quotes when the cell
    // contains a comma, quote, CR, or LF; escape inner quotes by
    // doubling them.
    if s.contains([',', '"', '\n', '\r']) {
        let escaped = s.replace('"', "\"\"");
        format!("\"{}\"", escaped)
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn homogeneous_array_of_primitives_renders_csv() {
        let v = json!([
            {"gene": "BRCA1", "lfc": 1.2, "padj": 0.001},
            {"gene": "TP53", "lfc": -0.8, "padj": 0.05},
        ]);
        let out = compact_tabular(&v);
        assert_eq!(out, "gene,lfc,padj\nBRCA1,1.2,0.001\nTP53,-0.8,0.05\n");
    }

    #[test]
    fn missing_key_falls_back_to_json() {
        // Different key set across rows — CSV would lose data.
        let v = json!([
            {"gene": "BRCA1", "lfc": 1.2},
            {"gene": "TP53", "padj": 0.05},
        ]);
        let out = compact_tabular(&v);
        assert_eq!(out, v.to_string());
    }

    #[test]
    fn nested_object_falls_back_to_json() {
        // Nested arrays/objects don't round-trip through CSV.
        let v = json!([
            {"gene": "BRCA1", "meta": {"chr": 17}},
        ]);
        let out = compact_tabular(&v);
        assert_eq!(out, v.to_string());
    }

    #[test]
    fn empty_array_falls_back_to_json() {
        let v = json!([]);
        assert_eq!(compact_tabular(&v), "[]");
    }

    #[test]
    fn scalar_value_falls_back_to_json() {
        let v = json!({"task_id": "x", "status": "completed"});
        assert_eq!(compact_tabular(&v), v.to_string());
    }

    #[test]
    fn csv_cell_quotes_special_chars() {
        let v = json!([
            {"label": "a,b", "note": "line1\nline2", "quoted": "say \"hi\""},
        ]);
        let out = compact_tabular(&v);
        // Header line + one data line.
        assert!(out.starts_with("label,note,quoted\n"));
        assert!(out.contains("\"a,b\""));
        assert!(out.contains("\"line1\nline2\""));
        assert!(out.contains("\"say \"\"hi\"\"\""));
    }

    #[test]
    fn null_cells_render_empty() {
        let v = json!([
            {"a": 1, "b": null},
            {"a": 2, "b": "x"},
        ]);
        let out = compact_tabular(&v);
        assert_eq!(out, "a,b\n1,\n2,x\n");
    }

    #[test]
    fn compact_csv_is_smaller_than_json_for_realistic_payload() {
        // §3.16 win is ~40–50% on tabular payloads. Sanity-check the
        // savings on a realistic metrics table — if this regresses
        // there's probably a bug in the CSV path making it too verbose.
        let rows: Vec<Value> = (0..50)
            .map(|i| {
                json!({
                    "gene_id": format!("ENSG{:011}", i),
                    "symbol": format!("GENE{}", i),
                    "log2_fold_change": (i as f64) * 0.1,
                    "padj": 0.001 * (i as f64 + 1.0),
                })
            })
            .collect();
        let v = Value::Array(rows);
        let csv = compact_tabular(&v);
        let json_len = v.to_string().len();
        let csv_len = csv.len();
        assert!(
            csv_len as f64 / json_len as f64 <= 0.7,
            "expected CSV to be ≤ 70% of JSON size; got CSV={} JSON={}",
            csv_len,
            json_len
        );
    }
}
