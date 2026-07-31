//! Regression coverage for workflow servers that run as uid 0.
//!
//! Claude Code refuses `--dangerously-skip-permissions` as root. The agent
//! wrapper must therefore resolve a non-root container identity instead of
//! forwarding the workflow-server process uid verbatim.

use std::process::Command;

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("workspace root must be two levels above CARGO_MANIFEST_DIR")
        .to_path_buf()
}

fn numeric_id(flag: &str) -> u32 {
    let output = Command::new("id")
        .arg(flag)
        .output()
        .expect("id command must run");
    assert!(output.status.success(), "id {flag} must succeed");
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("id output must be numeric")
}

#[test]
fn root_host_resolves_to_a_non_root_container_identity() {
    let root = workspace_root();
    let helper = root.join("scripts/agent-claude-common.sh");
    let temp = tempfile::tempdir().expect("create temporary ownership anchor");
    let package = temp.path().join("package");
    std::fs::create_dir_all(&package).expect("create package");

    let script = format!(
        "source '{}'; resolve_container_user_identity '' '{}' '{}' 0 0",
        helper.display(),
        package.display(),
        temp.path().display()
    );
    let output = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env_remove("ECAA_AGENT_CONTAINER_UID")
        .env_remove("ECAA_AGENT_CONTAINER_GID")
        .output()
        .expect("run container identity resolver");
    assert!(
        output.status.success(),
        "resolver failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let identity = String::from_utf8_lossy(&output.stdout);
    let (uid, gid) = identity
        .trim()
        .split_once(':')
        .expect("resolver must emit uid:gid");
    let uid: u32 = uid.parse().expect("resolved uid must be numeric");
    let gid: u32 = gid.parse().expect("resolved gid must be numeric");
    assert_ne!(uid, 0, "root must never reach the Claude task container");
    assert_ne!(
        gid, 0,
        "root group must never reach the Claude task container"
    );

    let caller_uid = numeric_id("-u");
    let caller_gid = numeric_id("-g");
    if caller_uid != 0 {
        assert_eq!(uid, caller_uid, "mount owner should be preserved");
        assert_eq!(gid, caller_gid, "mount group should be preserved");
    }
}

#[test]
fn docker_run_and_renderer_use_the_resolved_identity() {
    let wrapper = std::fs::read_to_string(workspace_root().join("scripts/agent-claude.sh"))
        .expect("read agent-claude.sh");

    assert!(
        wrapper
            .contains(r#"DOCKER_USER_ARGS=(--user "$AGENT_CONTAINER_UID:$AGENT_CONTAINER_GID")"#),
        "Claude task containers must use the resolved non-root identity"
    );
    assert!(
        wrapper.contains(
            r#""$CONTAINER_IMAGE" "$AGENT_CONTAINER_UID:$AGENT_CONTAINER_GID" "$ECAA_DOCKER_TMPFS_TMP_SIZE""#
        ),
        "post-task rendering must retain the same output ownership"
    );
    assert!(
        !wrapper.contains(r#"DOCKER_USER_ARGS=(--user "$(id -u):$(id -g)")"#),
        "the root-forwarding container user path must not return"
    );
}
