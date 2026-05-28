use ecaa_workflow_conversation::tools::literature_context::literature_context_enabled;
use std::sync::{Mutex, OnceLock};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn with_disabled_env(value: Option<&str>, f: impl FnOnce()) {
    let _guard = env_lock().lock().unwrap();
    let saved = std::env::var("SWFC_LITERATURE_CONTEXT_DISABLED").ok();
    match value {
        Some(v) => std::env::set_var("SWFC_LITERATURE_CONTEXT_DISABLED", v),
        None => std::env::remove_var("SWFC_LITERATURE_CONTEXT_DISABLED"),
    }
    f();
    match saved {
        Some(v) => std::env::set_var("SWFC_LITERATURE_CONTEXT_DISABLED", v),
        None => std::env::remove_var("SWFC_LITERATURE_CONTEXT_DISABLED"),
    }
}

#[test]
fn literature_context_enabled_by_default() {
    with_disabled_env(None, || assert!(literature_context_enabled()));
}

#[test]
fn literature_context_disabled_when_env_is_one() {
    with_disabled_env(Some("1"), || assert!(!literature_context_enabled()));
}

#[test]
fn literature_context_disabled_when_env_is_true() {
    with_disabled_env(Some("true"), || assert!(!literature_context_enabled()));
}
