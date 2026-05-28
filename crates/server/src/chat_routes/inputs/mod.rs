//! SME data-input registration endpoints.
//!
//! Two registration paths today:
//!
//! - `POST /api/chat/session/:id/inputs/path` — SME points the
//!   server at a directory already on the filesystem. Server walks
//!   the tree, computes per-file size + sha256, persists into
//!   `Session.inputs`. Phase B covers this. Lives in `list.rs`.
//!
//! - `POST /api/chat/session/:id/inputs/upload` — chunked multipart
//!   upload from the browser. Files land under
//!   `<ECAA_UPLOAD_ROOT>/<session_id>/`. Lives in `upload.rs`.
//!
//! Plus list (`GET /inputs`) in `list.rs` and delete
//! (`DELETE /inputs/:input_id`) in `delete.rs` for the Inputs inspector
//! tab.
//!
//! Multi-user posture: every endpoint reads the session's `owner_user`
//! and the request's `X-Scripps-User` header; the only enforcement
//! today is that the path-allowlist substitutes `${USER}` with the
//! session's owner. Real cross-user permission checks land with phase F
//! once an auth proxy is in front of the server.
//!
//! Split from a single 1275-LOC `inputs.rs` into
//! `mod.rs` (this file: thin re-export hub + `routes()` + `ROUTES`),
//! `list.rs` (register-path / list + path-validation + manifest
//! helpers), `delete.rs` (deletion), `upload.rs` (chunked upload +
//! finalize + upload-only helpers). Each submodule keeps its tests
//! co-located with its handlers. Cross-cutting helpers
//! (`allowlisted_roots`, `max_file_bytes`, `max_total_bytes`,
//! `max_files`, `validate_input_path`, `build_manifest`, `file_sha256`,
//! `RegisterPathRequest`, `DEFAULT_*`) live in `list.rs` and are
//! reached from `upload.rs` via `super::list::*`.

use super::ChatAppState;

mod delete;
mod list;
mod upload;

// Re-export the public handlers so callers that reach in via
// `chat_routes::inputs::<name>` still resolve, and so the established
// `pub use chat_routes::{...}` re-export pattern keeps working in
// `chat_routes/mod.rs` if it's ever extended to surface these names.
// `#[allow(unused_imports)]` mirrors the established §S16.1 pattern
// in `chat_routes/mod.rs` — the parent module declares `mod inputs;`
// privately, so these re-exports stay dormant until / unless they're
// promoted upward via `pub use inputs::...`.
#[allow(unused_imports)]
pub(super) use delete::delete_input;
#[allow(unused_imports)]
pub(super) use list::{list_inputs, register_input_path};
#[allow(unused_imports)]
pub(super) use upload::{finalize_upload, upload_input_chunk};

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` (in the
/// parent `chat_routes/mod.rs`) merges every submodule's builder into
/// the single chat surface. Flat aggregate kept in display order so
/// the entry sits at `chat_routes::inputs::ROUTES` exactly where
/// `chat_routes::ALL_ROUTES` indexes it. Each submodule keeps a
/// matching `pub(super) const ROUTES` slice next to its handler list
/// as inline documentation — see `list::ROUTES`, `upload::ROUTES`,
/// `delete::ROUTES`.
pub(super) const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/chat/session/:id/inputs"),
    ("POST", "/api/chat/session/:id/inputs/path"),
    ("POST", "/api/chat/session/:id/inputs/upload"),
    (
        "POST",
        "/api/chat/session/:id/inputs/upload/:upload_token/finalize",
    ),
    ("DELETE", "/api/chat/session/:id/inputs/:input_id"),
];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .merge(list::routes())
        .merge(upload::routes())
        .merge(delete::routes())
}

/// Compile-time gate: the flat `ROUTES` aggregate above must equal the
/// concatenation of each submodule's per-file `ROUTES` slice. Catches
/// drift if a route is added in a submodule but not surfaced in the
/// flat aggregate (or vice versa).
#[cfg(test)]
const _: () = {
    assert!(ROUTES.len() == list::ROUTES.len() + upload::ROUTES.len() + delete::ROUTES.len());
};

/// Test-shared helpers: every test module under `inputs/` needs the
/// same allowlisted-root setup. Hosted here so `list::tests`,
/// `delete::tests`, and `upload::tests` can each `use super::super::test_helpers::*`
/// without re-implementing the SHARED_ROOT one-shot.
#[cfg(test)]
pub(super) mod test_helpers {
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use tempfile::TempDir;

    /// Process-wide test root. Set once via `ECAA_INPUT_ROOTS` to the
    /// canonicalized temp dir parent, then each test creates a unique
    /// sub-directory under it. Avoids the race where parallel tests
    /// overwrite each other's env var with their own narrower
    /// allowlist (which then rejects the other tests' paths).
    ///
    /// R-30 — replaces a prior `Box::leak(Box::new(TempDir::new()))`
    /// pattern. `Arc<TempDir>` parked in the `OnceLock` has identical
    /// lifetime (the OnceLock lives for the test-process lifetime, so
    /// the inner Arc never drops to refcount zero during the run) but
    /// is heap-traceable: valgrind / heaptrack no longer flag the
    /// TempDir handle as "definitely lost", and the eventual drop on
    /// process exit fires the TempDir's cleanup hook rather than
    /// relying on the OS to reap `/tmp`.
    static SHARED_ROOT: OnceLock<(PathBuf, std::sync::Arc<TempDir>)> = OnceLock::new();

    /// Initialize the shared allowlist once. The temp parent dir lives
    /// for the test process lifetime via the `Arc<TempDir>` parked in
    /// the OnceLock — no leak.
    pub(crate) fn ensure_shared_root() -> &'static PathBuf {
        &SHARED_ROOT
            .get_or_init(|| {
                let parent = std::sync::Arc::new(TempDir::new().unwrap());
                let canonical = parent.path().canonicalize().unwrap();
                std::env::set_var("ECAA_INPUT_ROOTS", canonical.display().to_string());
                (canonical, parent)
            })
            .0
    }

    /// Helper: create a unique sub-dir inside the shared allowlisted
    /// root with two test files, return its canonical path.
    pub(crate) fn allowlisted_temp() -> PathBuf {
        let parent = ensure_shared_root();
        let unique = format!("case-{}", uuid::Uuid::new_v4().simple());
        let dir = parent.join(unique);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("matrix.tsv"), b"barcode\tcount\nAAA\t1\n").unwrap();
        std::fs::create_dir(dir.join("samples")).unwrap();
        std::fs::write(dir.join("samples").join("s1.h5"), b"\x89HDF5fake").unwrap();
        dir.canonicalize().unwrap()
    }
}
