//! scripps-workflow-server — Axum HTTP + SSE host for the web UI.
//!
//! Bin shim. The lib at `lib.rs::run` does the actual
//! work; this file only parses the env, installs the tracing
//! subscriber, and hands off to the lib so integration tests under
//! `crates/server/tests/` can reach the same routing modules.

fn main() {
    // Wire `tracing-subscriber` so the macros sprinkled
    // through `crates/conversation` (tool_loop spans, state-machine
    // transitions, retry warnings) actually emit at runtime.
    // RUST_LOG controls the filter via EnvFilter; without it we
    // default to info+ from our own crates and warn+ from deps so
    // a fresh `cargo run` shows session_state_advance + retry events
    // without drowning in reqwest internals. The OTel / Langfuse OTLP
    // sink (S5.22) layers on top of this subscriber via
    // tracing-opentelemetry once we ship that dep.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(
            "info,scripps_workflow_conversation=info,scripps_workflow_server=info,reqwest=warn,hyper=warn",
        )
    });
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .with_level(true)
        .init();

    // Route panics through tracing so they appear in the structured log
    // stream rather than going to stderr unformatted. The default hook
    // still prints the backtrace when RUST_BACKTRACE is set; this hook
    // does not replace that — it adds a structured tracing::error! event
    // so operators relying on centralized log ingestion see the panic.
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
            .unwrap_or("<non-string panic payload>");
        tracing::error!(
            panic.location = location.as_deref().unwrap_or("<unknown>"),
            panic.payload = %payload,
            "panic caught in panic hook"
        );
    }));

    // Build the tokio runtime by hand instead of `#[tokio::main]` so the
    // worker thread stack is sized to 8 MiB. Defense-in-depth for the
    // bounded-depth `BoundedJson` extractor: even if a route that
    // bypassed the depth cap (or a deeply-recursive serde structure)
    // tried to deserialize past the depth limit, an 8 MiB stack keeps a
    // serde_json recursive visitor below the OS stack guard page. The
    // OS default is 8 MiB on Linux and 1 MiB on macOS / Windows; pinning
    // here keeps the worst case homogeneous across CI runners and
    // production hosts. 4 MiB is the documented minimum; 8 MiB doubles
    // it without exhausting per-process VM on the harness host.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 * 1024 * 1024)
        .build()
        .expect("build tokio runtime");
    runtime.block_on(scripps_workflow_server::run());
}
