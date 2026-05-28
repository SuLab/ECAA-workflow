//! `scripps-workflow chat-llm` — dev/test REPL that exercises the
//! same `ConversationService` as the web UI. Offline-only; end users
//! interact with the web UI.

use anyhow::{Context, Result};
use colored::Colorize;
use rustyline::DefaultEditor;
use ecaa_workflow_conversation::{
    AnthropicClient, ConversationService, LlmBackend, SessionStore,
};
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) fn run_chat_llm(config_dir: &str, output: &str) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("creating tokio runtime")?;
    runtime.block_on(run_chat_llm_async(config_dir, output))
}

async fn run_chat_llm_async(config_dir: &str, output: &str) -> Result<()> {
    let config_path = PathBuf::from(config_dir);
    let session_dir = sessions_dir();
    let store = SessionStore::open(&session_dir)
        .await
        .with_context(|| format!("opening session store '{}'", session_dir.display()))?;

    let llm: Arc<dyn LlmBackend> = Arc::new(
        AnthropicClient::new()
            .context("SWFC_ANTHROPIC_API_KEY required for chat-llm — set the env var first (legacy ANTHROPIC_API_KEY also accepted with a deprecation warning)")?,
    );
    let service = ConversationService::new(llm, store, config_path);
    let (session_id, greeting) = service
        .start_session(false)
        .await
        .map_err(|e| anyhow::anyhow!("starting session: {}", e))?;

    println!(
        "{}",
        "Scripps Workflow — chat-llm (LLM-mediated)".bold().cyan()
    );
    println!(
        "{}",
        "Slash commands: /confirm /reject /quit (no other slash commands)".dimmed()
    );
    println!();
    println!("{}", greeting.content);
    println!();

    let mut rl = DefaultEditor::new()?;
    while let Ok(raw) = rl.readline("> ") {
        let line = raw.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let _ = rl.add_history_entry(&line);

        match line.as_str() {
            "/quit" | "/exit" => break,
            "/confirm" => {
                if let Err(e) = service.confirm(session_id).await {
                    println!("{} {}", "error:".red(), e);
                    continue;
                }
                println!("{}", "[confirmed]".green().dimmed());
                // Drive one more LLM turn so the assistant can react and emit.
                // The session is now ReadyToEmit with user_confirmed=true; the
                // system prompt instructs the LLM to call emit_package
                // immediately in this state. The nudge text must NOT read like
                // a soft cue ("please continue") — observed failure mode was
                // the LLM treating it as license to ask another intake
                // question. Be explicit so the LLM follows the protocol.
                match service
                    .send_turn(
                        session_id,
                        "Confirmation received — session state is now \
                         ReadyToEmit and user_confirmed=true. Call \
                         emit_package now (alone in this turn, no arguments). \
                         Do not ask any more intake questions."
                            .into(),
                        None,
                    )
                    .await
                {
                    Ok(turn) => println!("\n{}\n", turn.content),
                    Err(e) => println!("{} {}", "error:".red(), e),
                }
            }
            "/reject" => {
                if let Err(e) = service.reject(session_id).await {
                    println!("{} {}", "error:".red(), e);
                    continue;
                }
                println!("{}", "[returned to intake]".yellow().dimmed());
            }
            _ => match service.send_turn(session_id, line, None).await {
                Ok(turn) => {
                    println!("\n{}", turn.content);
                    if turn.confirmation_card.is_some() {
                        println!(
                            "{}",
                            "  [Type /confirm to accept, /reject to revise]".dimmed()
                        );
                    }
                    if !turn.quick_replies.is_empty() {
                        println!("  options: {}", turn.quick_replies.join(" | ").cyan());
                    }
                    println!();
                }
                Err(e) => println!("{} {}", "error:".red(), e),
            },
        }
    }

    // Final state — if the user's session was Emitted, point at the package.
    if let Some(session) = service.get_session(session_id).await {
        if let Some(path) = session.emitted_package_path {
            println!(
                "{} package emitted at {}",
                "✓".green().bold(),
                path.display().to_string().cyan()
            );
        }
    }
    let _ = output;
    Ok(())
}

fn sessions_dir() -> PathBuf {
    if let Ok(d) = std::env::var("SWFC_CHAT_SESSIONS_DIR") {
        return PathBuf::from(d);
    }
    if let Some(home) = dirs_home() {
        return home.join(".scripps-workflow/sessions");
    }
    PathBuf::from("./.scripps-sessions")
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
