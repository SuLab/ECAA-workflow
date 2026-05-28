//! Static greeting-turn constructor. Separate module so `Session::new`
//! callers and HTTP-route handlers can share one canonical greeting
//! without pulling in the rest of `ConversationService`.

use crate::session::{AssistantIntent, Turn};

/// Construct the canonical static greeting turn shown at session start.
pub fn greeting_turn() -> Turn {
    let mut t = Turn::assistant(
        "Hi! Tell me about the analysis you're planning — what kind of \
         data, what question, what you've already done. I'll work through \
         it with you and pull together the package when we're ready.",
    );
    t.intent = Some(AssistantIntent::Greeting);
    t
}
