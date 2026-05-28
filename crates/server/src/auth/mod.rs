//! `crate::auth` — auth + authZ middleware module.
//!
//! `auth_middleware` is the bearer-token gate (authentication).
//! [`verify_owner_middleware`] is the per-request authZ layer
//! applied to session-scoped routes; it compares the authenticated
//! user (`X-Scripps-User` header injected by the upstream auth
//! proxy) against the session's `owner_user` field. Deny-default:
//! mismatched / missing header → 403 Forbidden.

// Bearer-token logic lives in `bearer.rs`; sibling `verify_owner.rs`
// holds the per-session authZ layer.
mod bearer;
pub mod principal;
mod verify_owner;

pub use bearer::{auth_middleware, AuthConfig};
pub use principal::{extract_principal, AuditActor, RequestPrincipal, ShareScope};
pub use verify_owner::{
    owner_authz_disabled, verify_owner_middleware, OwnerAuthzError, OWNER_USER_HEADER,
};
