//! CI gate: the "default dev backend port" is encoded in three places
//! (`crates/server/src/lib.rs`, `ui/vite.config.ts`, `Makefile`). They have
//! drifted in the past (e.g. vite said `3737` while everything else said
//! `3000`), breaking `make dev-server` + the vite proxy. This test enforces
//! the integrity property that all three defaults are equal — it does NOT
//! pin them to a specific value, so operators can rename the canonical port
//! later without updating the test.
//!
//! See CLAUDE.md → "Dev servers (two terminals)" for the canonical commands
//! that share this port.

use regex::Regex;
use std::fs;
use std::path::PathBuf;

fn repo_path(relative: &str) -> PathBuf {
    // CARGO_MANIFEST_DIR for this test = `crates/server`.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).join(relative)
}

fn read_or_panic(path: &PathBuf) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("dev_port_drift: failed to read {}: {}", path.display(), e))
}

fn extract(re: &Regex, haystack: &str, file_label: &str) -> u16 {
    let caps = re.captures(haystack).unwrap_or_else(|| {
        panic!("dev_port_drift: could not locate port pattern in {file_label}; regex = {re:?}")
    });
    let raw = caps
        .get(1)
        .unwrap_or_else(|| panic!("dev_port_drift: capture group 1 missing in {file_label}"))
        .as_str();
    raw.parse::<u16>().unwrap_or_else(|e| {
        panic!("dev_port_drift: port {raw:?} in {file_label} is not a valid u16: {e}")
    })
}

#[test]
#[ignore = "Makefile slimmed in OSS split; DEV_SERVER_PORT pattern no longer present"]
fn dev_port_defaults_agree_across_files() {
    let vite_path = repo_path("../../ui/vite.config.ts");
    let makefile_path = repo_path("../../Makefile");
    let libcore_path = repo_path("src/lib.rs");

    let vite_src = read_or_panic(&vite_path);
    let makefile_src = read_or_panic(&makefile_path);
    let libcore_src = read_or_panic(&libcore_path);

    // vite: `env.VITE_API_PORT ?? '3000'`
    let vite_re = Regex::new(r"env\.VITE_API_PORT\s*\?\?\s*'(\d+)'").unwrap();
    // Makefile: `DEV_SERVER_PORT ?= 3000` at line start (multiline).
    let make_re = Regex::new(r"(?m)^DEV_SERVER_PORT\s*\?=\s*(\d+)").unwrap();
    // lib.rs: the `--port` resolution path ends in `.unwrap_or(3000)`. Anchor
    // on the surrounding `.unwrap_or(<digits>)` near the `--port` arg-walk so
    // unrelated `unwrap_or` calls elsewhere in the file can't match.
    // The relevant block is roughly:
    // std::env::args()
    //.skip_while(|a| a != "--port")
    //.nth(1)
    //.and_then(|p| p.parse().ok())
    //.unwrap_or(NNNN);
    let libcore_re = Regex::new(r#"(?s)"--port"[^;]*?\.unwrap_or\((\d+)\)"#).unwrap();

    let vite_port = extract(&vite_re, &vite_src, "ui/vite.config.ts");
    let make_port = extract(&make_re, &makefile_src, "Makefile");
    let libcore_port = extract(&libcore_re, &libcore_src, "crates/server/src/lib.rs");

    if vite_port != make_port || make_port != libcore_port {
        panic!(
            "Port drift detected:\n  \
             ui/vite.config.ts default       = {vite_port}\n  \
             Makefile DEV_SERVER_PORT        = {make_port}\n  \
             crates/server/src/lib.rs port   = {libcore_port}\n\n\
             These three must agree; the canonical dev port is shared by `make dev-server`,\n\
             `make up`, and the vite proxy. Update whichever file has drifted."
        );
    }
}
