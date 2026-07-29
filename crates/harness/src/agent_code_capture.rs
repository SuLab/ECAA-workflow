//! Post-dispatch backfill of `agent-code.json.executed_code` (CV-6).
//!
//! `agent-claude.sh` extracts `executed_code` heuristically from the
//! agent log (triple-backtick code fences / shebang blocks). For stages
//! whose agent authored and ran standalone scripts under
//! `runtime/outputs/<task_id>/scripts/` — the R/Python analysis stages
//! (`differential_expression`, `review_prior_work`,
//! `contextualize_findings_with_literature`) — that log heuristic finds
//! nothing, leaving `executed_code` empty and `language:"unknown"`.
//!
//! This runs after the agent exits and, when the heuristic capture came
//! up empty, backfills the record from the scripts that actually ran so
//! the deposit's code-provenance sidecar is truthful. It is idempotent
//! (only fills when `executed_code` is empty) and never fabricates: if
//! there are no scripts on disk it leaves the record untouched.
//!
//! `agent-code.json` is excluded from the byte-reproducibility baseline
//! (see `core::agent_code`), so a post-hoc rewrite here does not perturb
//! emit determinism.

use ecaa_workflow_core::agent_code::AgentCodeRecord;
use std::path::Path;

/// Upper bound on the concatenated executed-code payload written back
/// into the record, so a runaway multi-megabyte script set cannot bloat
/// the sidecar. The scripts themselves remain on disk under `scripts/`.
const MAX_EXECUTED_CODE_BYTES: usize = 256 * 1024;

/// Backfill `runtime/outputs/<task_id>/agent-code.json` `executed_code`
/// + `language` from the task's `scripts/` directory when the
/// heuristic capture left them empty/unknown.
///
/// Returns `true` when the record was rewritten, `false` otherwise
/// (no record, record already populated, no scripts, or a write error).
pub fn backfill_executed_code(package_root: &Path, task_id: &str) -> bool {
    let out = package_root.join("runtime/outputs").join(task_id);
    let ac_path = out.join("agent-code.json");
    let raw = match std::fs::read_to_string(&ac_path) {
        Ok(r) => r,
        Err(_) => return false, // nothing captured to backfill
    };
    let mut rec: AgentCodeRecord = match serde_json::from_str(&raw) {
        Ok(r) => r,
        Err(_) => return false,
    };
    // Idempotent: only fill when the heuristic capture found nothing.
    if !rec.executed_code.trim().is_empty() {
        return false;
    }

    let scripts = collect_scripts(&out.join("scripts"));
    if scripts.is_empty() {
        return false;
    }
    let (code, language) = assemble(&scripts);
    if code.is_empty() {
        return false;
    }

    rec.executed_code = code;
    rec.language = language;
    let serialized = match serde_json::to_string_pretty(&rec) {
        Ok(s) => s,
        Err(_) => return false,
    };
    ecaa_workflow_core::fs_helpers::atomic_write_bytes_sync(&ac_path, serialized.as_bytes()).is_ok()
}

/// Read the code scripts directly under `scripts_dir`, sorted by file
/// name for determinism. Only regular files whose extension marks them
/// as source (`.py`/`.R`/`.r`/`.sh`/`.bash`) or that carry a `#!`
/// shebang are included; READMEs, manifests, and data files are skipped.
fn collect_scripts(scripts_dir: &Path) -> Vec<(String, String)> {
    let mut entries: Vec<std::path::PathBuf> = match std::fs::read_dir(scripts_dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect(),
        Err(_) => return Vec::new(),
    };
    entries.sort();
    let mut out = Vec::new();
    for p in entries {
        let name = match p.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let content = match std::fs::read_to_string(&p) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if detect_language(&name, &content).is_none() {
            continue;
        }
        out.push((name, content));
    }
    out
}

/// Concatenate the scripts (each under a `# === scripts/<name> ===`
/// header) into a single `executed_code` payload, capped at
/// `MAX_EXECUTED_CODE_BYTES`, and derive the `language` label from the
/// distinct languages present (comma-joined in first-seen order).
fn assemble(scripts: &[(String, String)]) -> (String, String) {
    let mut code = String::new();
    let mut langs: Vec<String> = Vec::new();
    for (name, content) in scripts {
        if let Some(lang) = detect_language(name, content) {
            if !langs.iter().any(|l| l == &lang) {
                langs.push(lang);
            }
        }
        let header = format!("# === scripts/{name} ===\n");
        if code.len() + header.len() + content.len() > MAX_EXECUTED_CODE_BYTES {
            let remaining = MAX_EXECUTED_CODE_BYTES.saturating_sub(code.len() + header.len());
            code.push_str(&header);
            code.push_str(truncate_on_char_boundary(content, remaining));
            code.push_str("\n# … truncated …\n");
            break;
        }
        code.push_str(&header);
        code.push_str(content);
        if !content.ends_with('\n') {
            code.push('\n');
        }
    }
    let language = if langs.is_empty() {
        "unknown".to_string()
    } else {
        langs.join(", ")
    };
    (code, language)
}

fn truncate_on_char_boundary(s: &str, mut max: usize) -> &str {
    if max >= s.len() {
        return s;
    }
    while max > 0 && !s.is_char_boundary(max) {
        max -= 1;
    }
    &s[..max]
}

/// Infer the language of a script from its extension, falling back to a
/// shebang / content heuristic. Returns `None` for files that are not
/// recognisably source code (so they are excluded from the capture).
fn detect_language(name: &str, content: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".py") {
        return Some("Python".to_string());
    }
    if lower.ends_with(".r") {
        return Some("R".to_string());
    }
    if lower.ends_with(".sh") || lower.ends_with(".bash") {
        return Some("Bash".to_string());
    }
    let first = content.lines().next().unwrap_or("");
    if first.starts_with("#!") {
        if first.contains("python") {
            return Some("Python".to_string());
        }
        if first.contains("Rscript") || first.contains("/R") {
            return Some("R".to_string());
        }
        if first.contains("bash") || first.contains("/sh") || first.contains("zsh") {
            return Some("Bash".to_string());
        }
        return Some("Bash".to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, body: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn read_record(pkg: &Path, task: &str) -> AgentCodeRecord {
        let raw = std::fs::read_to_string(
            pkg.join("runtime/outputs")
                .join(task)
                .join("agent-code.json"),
        )
        .unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    fn empty_agent_code_json() -> &'static str {
        r#"{"prompt":"p","response_text":"","executed_code":"","language":"unknown","started_at":"2026-07-18T17:56:39Z","completed_at":"2026-07-18T18:02:14Z"}"#
    }

    #[test]
    fn backfills_r_script() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let out = pkg.join("runtime/outputs/differential_expression");
        write(&out.join("agent-code.json"), empty_agent_code_json());
        write(
            &out.join("scripts/01_deseq2_de.R"),
            "library(DESeq2)\nres <- results(dds)\n",
        );
        assert!(backfill_executed_code(pkg, "differential_expression"));
        let rec = read_record(pkg, "differential_expression");
        assert!(
            rec.executed_code.contains("library(DESeq2)"),
            "{}",
            rec.executed_code
        );
        assert!(rec.executed_code.contains("scripts/01_deseq2_de.R"));
        assert_eq!(rec.language, "R");
    }

    #[test]
    fn backfills_python_script() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let out = pkg.join("runtime/outputs/review_prior_work");
        write(&out.join("agent-code.json"), empty_agent_code_json());
        write(
            &out.join("scripts/01_retrieve_literature.py"),
            "import requests\nprint('go')\n",
        );
        assert!(backfill_executed_code(pkg, "review_prior_work"));
        let rec = read_record(pkg, "review_prior_work");
        assert_eq!(rec.language, "Python");
        assert!(rec.executed_code.contains("import requests"));
    }

    #[test]
    fn backfills_mixed_language_scripts_in_sorted_order() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let out = pkg.join("runtime/outputs/contextualize_findings_with_literature");
        write(&out.join("agent-code.json"), empty_agent_code_json());
        write(
            &out.join("scripts/01_annotate_ensembl.R"),
            "library(biomaRt)\n",
        );
        write(
            &out.join("scripts/02_build_claims_matrix.py"),
            "import csv\n",
        );
        assert!(backfill_executed_code(
            pkg,
            "contextualize_findings_with_literature"
        ));
        let rec = read_record(pkg, "contextualize_findings_with_literature");
        // Sorted file order → R first, then Python.
        assert_eq!(rec.language, "R, Python");
        let r_pos = rec.executed_code.find("biomaRt").unwrap();
        let py_pos = rec.executed_code.find("import csv").unwrap();
        assert!(
            r_pos < py_pos,
            "scripts must be concatenated in sorted order"
        );
    }

    #[test]
    fn does_not_clobber_already_populated_record() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let out = pkg.join("runtime/outputs/qc");
        write(
            &out.join("agent-code.json"),
            r#"{"prompt":"p","response_text":"","executed_code":"already here","language":"Bash","started_at":"a","completed_at":"b"}"#,
        );
        write(&out.join("scripts/01.py"), "print(1)\n");
        assert!(!backfill_executed_code(pkg, "qc"));
        let rec = read_record(pkg, "qc");
        assert_eq!(rec.executed_code, "already here");
        assert_eq!(rec.language, "Bash");
    }

    #[test]
    fn no_scripts_leaves_record_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let out = pkg.join("runtime/outputs/qc");
        write(&out.join("agent-code.json"), empty_agent_code_json());
        assert!(!backfill_executed_code(pkg, "qc"));
        let rec = read_record(pkg, "qc");
        assert_eq!(rec.executed_code, "");
        assert_eq!(rec.language, "unknown");
    }

    #[test]
    fn skips_non_source_files() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let out = pkg.join("runtime/outputs/de");
        write(&out.join("agent-code.json"), empty_agent_code_json());
        write(&out.join("scripts/README.md"), "not code\n");
        write(&out.join("scripts/data.csv"), "a,b\n1,2\n");
        // Only a non-source set present → no backfill.
        assert!(!backfill_executed_code(pkg, "de"));
    }
}
