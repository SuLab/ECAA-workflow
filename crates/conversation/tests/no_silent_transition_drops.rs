//! Tolerated state-transition failures must be logged.
//!
//! Silent `let _ =...try_transition(...)` drops hide the exact races this
//! audit is meant to surface. If a transition is intentionally
//! Best-effort, wrap it in `if let Err(err) =...` and emit a
//! `tracing::warn!` with the session id, trigger, state, and error.

#[test]
fn no_silent_transition_drops_in_source() {
    let crate_root = env!("CARGO_MANIFEST_DIR");
    let mut found: Vec<String> = Vec::new();

    for entry in walkdir::WalkDir::new(crate_root)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        if path.file_name().and_then(|s| s.to_str()) == Some("no_silent_transition_drops.rs") {
            continue;
        }
        if path.components().any(|c| c.as_os_str() == "target") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for (lineno, line) in content.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("///") {
                continue;
            }
            if line.contains("let _ =") && line.contains("try_transition") {
                found.push(format!(
                    "{}:{}: {}",
                    path.display(),
                    lineno + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        found.is_empty(),
        "silent `let _ = ...try_transition(...)` drops detected; log tolerated failures:\n{}",
        found.join("\n")
    );
}
