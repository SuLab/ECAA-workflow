//! Property-based test for the chat_routes path-jail helpers.
//!
//! No string we feed to `safe_segment_join` should result in a path
//! outside the jail root: either the helper rejects (Err), or the
//! joined path `starts_with(root)`. Catches future regressions if a
//! contributor splices a string into `pkg.join(...)` without the
//! helper.

use proptest::prelude::*;
use scripps_workflow_server::chat_routes::{
    assert_under_root, safe_relative_join, safe_segment_join,
};
use std::path::{Path, PathBuf};

proptest! {
    /// Single-segment join never escapes the root, regardless of what
    /// random bytes (within the regex shape) get fed in. `Err` is the
    /// safe outcome — the helper noticed something suspicious and
    /// refused to splice.
    #[test]
    fn safe_segment_never_escapes(s in "[^\\x00]{0,128}") {
        let root = Path::new("/tmp/pkg-test");
        if let Ok(p) = safe_segment_join(root, &s) {
            prop_assert!(
                p.starts_with(root),
                "joined path must stay under root: {:?}",
                p
            );
        }
    }

    /// Multi-segment relative join never escapes either.
    #[test]
    fn safe_relative_never_escapes(parts in proptest::collection::vec("[^\\x00/\\\\]{0,32}", 0..6)) {
        let root = Path::new("/tmp/pkg-test");
        let mut rel = PathBuf::new();
        for p in &parts {
            rel.push(p);
        }
        if let Ok(joined) = safe_relative_join(root, &rel) {
            prop_assert!(
                joined.starts_with(root),
                "joined path must stay under root: {:?}",
                joined
            );
        }
    }
}

/// Sanity check: `assert_under_root` against a real tempdir rejects
/// paths planted outside the root via a symlink. Not strictly a fuzz
/// case but pinned alongside so the fuzz harness file owns the
/// "jail-can't-be-bypassed" property.
#[test]
fn assert_under_root_rejects_planted_symlink_escape() {
    let tmp = tempfile::TempDir::new().unwrap();
    let pkg = tmp.path().join("pkg");
    std::fs::create_dir(&pkg).unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, pkg.join("link")).unwrap();
    let target = pkg.join("link").join("escape.txt");
    std::fs::write(&target, b"x").unwrap();
    assert!(
        assert_under_root(&pkg, &target).is_err(),
        "symlink escape must be rejected"
    );
}
