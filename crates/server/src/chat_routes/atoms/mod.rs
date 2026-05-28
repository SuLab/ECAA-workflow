//! Atom-safety-policy per-atom mutation surface.
//!
//! Today the only endpoint is `POST /api/chat/session/:id/atom/:atom_id/add-runtime-package`,
//! which powers the BlockerCard's `ProvisioningDenied` "Add `<package>`
//! to atom.runtime_packages" affordance. Future per-atom mutations
//! (override safety policy, override container image, etc.) belong
//! here too.
//!
//! Each per-domain submodule exposes
//! `pub const ROUTES` + `pub fn routes()`; the top-level
//! `chat_routes/mod.rs` merges them all into the single chat surface.

use super::ChatAppState;

pub(super) mod add_runtime_package;

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface.
pub(super) const ROUTES: &[(&str, &str)] = &[(
    "POST",
    "/api/chat/session/:id/atom/:atom_id/add-runtime-package",
)];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().merge(add_runtime_package::routes())
}
