// crates/core/src/replay/env_provision.rs
//
// Tiered execution-environment provisioning for replay mode.
//
// Given a downloaded ECAA package, chooses the best available execution
// environment for re-running the saved compute scripts, in order of
// reproducibility:
//
//   Tier 1  Container — exact recorded image digest (best)
//   Tier 2  RebuiltImage — rebuild from a Dockerfile/build spec in the package
//   Tier 3  HostConda — conda env reconstructed from the task's env.lock
//   Tier 4  None — no environment available; re-execution is not possible

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// The execution environment chosen by `provision`.
#[derive(Debug, PartialEq, Eq)]
pub enum ExecEnv {
    /// Re-use the exact image by digest recorded in the package.
    Container { digest: String },
    /// A locally rebuilt image (from a Dockerfile or build spec in the package).
    RebuiltImage { tag: String },
    /// A host conda environment reconstructed from the task's `env.lock`.
    HostConda { prefix: PathBuf },
    /// No suitable environment found; re-execution is unavailable.
    None,
}

/// Options that control and instrument the provisioning decision.
///
/// Both probe functions are injected so tests can run hermetically without
/// making any real Docker or conda system calls.
pub struct ProvisionOpts {
    /// Allow rebuilding the image from a Dockerfile/build spec found in the
    /// package when the recorded digest is unavailable.
    pub allow_rebuild: bool,
    /// Returns `true` when Docker is available on the host.
    pub docker_probe: fn() -> bool,
    /// Returns `true` when conda is available on the host.
    pub conda_probe: fn() -> bool,
}

/// Select an execution environment for re-running the compute tasks in `pkg`.
///
/// Reads the first compute task's `runtime/outputs/<task>/determinism-env.json`
/// for the `task_container_digest`, then applies the tier waterfall:
///
/// 1. **Container** — `opts.docker_probe()` true, digest non-empty.
/// 2. **RebuiltImage** — `opts.allow_rebuild` true, a Dockerfile is present
///    directly under `runtime/outputs/<task>/` or at the package root.
/// 3. **HostConda** — `opts.conda_probe()` true, `env.lock` exists under
///    `runtime/outputs/<task>/`.
/// 4. **None** — fallback.
pub fn provision(pkg: &Path, opts: &ProvisionOpts) -> ExecEnv {
    // Find the first eligible compute task by scanning runtime/outputs/ in
    // lexicographic order. A task is "eligible" if it has a determinism-env.json.
    let outputs = pkg.join("runtime/outputs");
    let first_task_dir = find_first_task_dir(&outputs);

    // --- Tier 1: Container by digest ---
    if (opts.docker_probe)() {
        if let Some(ref task_dir) = first_task_dir {
            let digest = read_container_digest(task_dir);
            if !digest.is_empty() {
                return ExecEnv::Container { digest };
            }
        }
    }

    // --- Tier 2: Rebuilt image ---
    // Rebuilding produces an image that must be run via `docker run`, so Docker
    // must be available — mirror the same guard used in Tier 1.
    if opts.allow_rebuild && (opts.docker_probe)() {
        if let Some(dockerfile) = find_build_spec(pkg, first_task_dir.as_deref()) {
            let tag = format!(
                "ecaa-replay:{}",
                dockerfile
                    .parent()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "pkg".to_string())
            );
            return ExecEnv::RebuiltImage { tag };
        }
    }

    // --- Tier 3: Host conda ---
    if (opts.conda_probe)() {
        if let Some(ref task_dir) = first_task_dir {
            let lock = task_dir.join("env.lock");
            if lock.exists() {
                // The conda prefix lives adjacent to the package under a
                // `.conda-envs/` directory named after the package root.
                let prefix = pkg.join(".conda-envs").join(
                    pkg.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "ecaa-replay-env".to_string()),
                );
                return ExecEnv::HostConda { prefix };
            }
        }
    }

    ExecEnv::None
}

impl ExecEnv {
    /// Short label for the chosen tier.
    pub fn tier_name(&self) -> &'static str {
        match self {
            ExecEnv::Container { .. } => "container",
            ExecEnv::RebuiltImage { .. } => "rebuilt",
            ExecEnv::HostConda { .. } => "host",
            ExecEnv::None => "none",
        }
    }

    /// Run `script` inside this execution environment.
    ///
    /// - `env` — key/value pairs to inject (recorded `captured_env_vars` +
    ///   determinism-pinning vars: `SOURCE_DATE_EPOCH`, `PYTHONHASHSEED`,
    ///   `LC_ALL`, `TZ`).
    /// - `cwd` — working directory for the script (mounted into the container
    ///   at the same absolute path).
    ///
    /// The interpreter is inferred from the script's extension:
    /// `.R` → `Rscript`, `.py` → `python3`, `.sh` → `bash`.
    ///
    /// Returns an `io::Error` when `ExecEnv::None` (no environment available).
    pub fn run_script(
        &self,
        script: &Path,
        env: &BTreeMap<String, String>,
        cwd: &Path,
    ) -> io::Result<Output> {
        let interp = interpreter_for(script)?;

        match self {
            ExecEnv::Container { digest } => {
                let mut cmd = Command::new("docker");
                cmd.arg("run").arg("--rm");
                // Mount the working directory at the same path inside the container.
                cmd.arg("-v").arg(format!("{}:{}", cwd.display(), cwd.display()));
                cmd.arg("-w").arg(cwd);
                for (k, v) in env {
                    cmd.arg("--env").arg(format!("{k}={v}"));
                }
                cmd.arg(digest);
                cmd.arg(interp);
                cmd.arg(script);
                cmd.output()
            }

            ExecEnv::RebuiltImage { tag } => {
                let mut cmd = Command::new("docker");
                cmd.arg("run").arg("--rm");
                cmd.arg("-v").arg(format!("{}:{}", cwd.display(), cwd.display()));
                cmd.arg("-w").arg(cwd);
                for (k, v) in env {
                    cmd.arg("--env").arg(format!("{k}={v}"));
                }
                cmd.arg(tag);
                cmd.arg(interp);
                cmd.arg(script);
                cmd.output()
            }

            ExecEnv::HostConda { prefix } => {
                // Use `conda run -p <prefix> env K=V ... <interp> <script>` so
                // that the recorded env vars are guaranteed set for the
                // interpreter regardless of conda version or activation-hook
                // behaviour.  `env` (from coreutils) is always present inside
                // any conda environment and sets variables immediately before
                // exec'ing the interpreter.
                let mut cmd = Command::new("conda");
                cmd.arg("run").arg("-p").arg(prefix);
                cmd.arg("env");
                for (k, v) in env {
                    cmd.arg(format!("{k}={v}"));
                }
                cmd.arg(interp);
                cmd.arg(script);
                cmd.current_dir(cwd);
                cmd.output()
            }

            ExecEnv::None => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no execution environment available (ExecEnv::None)",
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Return the first task directory (lex order) that contains a
/// `determinism-env.json` file, or `None`.
fn find_first_task_dir(outputs: &Path) -> Option<PathBuf> {
    if !outputs.is_dir() {
        return None;
    }
    let mut dirs: Vec<_> = std::fs::read_dir(outputs)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    dirs.sort_by_key(|e| e.file_name());

    dirs.into_iter()
        .map(|e| e.path())
        .find(|d| d.join("determinism-env.json").exists())
}

/// Read `task_container_digest` from `<task_dir>/determinism-env.json`.
/// Returns an empty string if the file is absent or the field is missing.
fn read_container_digest(task_dir: &Path) -> String {
    let path = task_dir.join("determinism-env.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("task_container_digest").cloned())
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Find a Dockerfile (build spec) in the package. Checks:
/// 1. `<task_dir>/Dockerfile`
/// 2. `<pkg_root>/Dockerfile`
fn find_build_spec(pkg: &Path, task_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(td) = task_dir {
        let df = td.join("Dockerfile");
        if df.exists() {
            return Some(df);
        }
    }
    let df = pkg.join("Dockerfile");
    if df.exists() {
        return Some(df);
    }
    None
}

/// Return the interpreter string for a script based on file extension.
fn interpreter_for(script: &Path) -> io::Result<&'static str> {
    match script.extension().and_then(|e| e.to_str()) {
        Some("R") => Ok("Rscript"),
        Some("py") => Ok("python3"),
        Some("sh") => Ok("bash"),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "unrecognised script extension {:?}; expected .R, .py, or .sh",
                other
            ),
        )),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Write a minimal `determinism-env.json` under `<root>/runtime/outputs/<task>/`.
    fn write_det_env(root: &Path, task: &str, digest: &str) {
        let task_dir = root.join("runtime/outputs").join(task);
        fs::create_dir_all(&task_dir).unwrap();
        let content = serde_json::json!({
            "schema_version": "1",
            "captured_env_vars": ["PYTHONHASHSEED", "SOURCE_DATE_EPOCH", "TZ", "LANG", "LC_ALL"],
            "source_date_epoch": "1782173329",
            "lang": "C.UTF-8",
            "lc_all": "C.UTF-8",
            "tz": "UTC",
            "pythonhashseed": "0",
            "task_container_digest": digest
        });
        fs::write(
            task_dir.join("determinism-env.json"),
            serde_json::to_string(&content).unwrap(),
        )
        .unwrap();
    }

    /// Write a stub `env.lock` under `<root>/runtime/outputs/<task>/`.
    fn write_env_lock(root: &Path, task: &str) {
        let task_dir = root.join("runtime/outputs").join(task);
        fs::create_dir_all(&task_dir).unwrap();
        fs::write(task_dir.join("env.lock"), "# conda lock stub\n").unwrap();
    }

    // ---- Tier 4 (None) tests ----

    /// With both probes returning false and no fallback, provision → None.
    #[test]
    fn provision_none_when_both_probes_false() {
        let tmp = tempfile::tempdir().unwrap();
        write_det_env(tmp.path(), "differential_expression", "sha256:abcd1234");

        let opts = ProvisionOpts {
            allow_rebuild: false,
            docker_probe: || false,
            conda_probe: || false,
        };
        let env = provision(tmp.path(), &opts);
        assert_eq!(env, ExecEnv::None);
        assert_eq!(env.tier_name(), "none");
    }

    /// With an empty package (no runtime/outputs), provision → None regardless
    /// of probes.
    #[test]
    fn provision_none_empty_package() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = ProvisionOpts {
            allow_rebuild: true,
            docker_probe: || true,
            conda_probe: || true,
        };
        let env = provision(tmp.path(), &opts);
        assert_eq!(env, ExecEnv::None);
    }

    // ---- Tier 3 (HostConda) tests ----

    /// conda_probe=true + env.lock present + docker_probe=false → HostConda.
    #[test]
    fn provision_host_conda_when_docker_absent_and_lock_present() {
        let tmp = tempfile::tempdir().unwrap();
        write_det_env(tmp.path(), "differential_expression", "sha256:abcd1234");
        write_env_lock(tmp.path(), "differential_expression");

        let opts = ProvisionOpts {
            allow_rebuild: false,
            docker_probe: || false,
            conda_probe: || true,
        };
        let env = provision(tmp.path(), &opts);
        assert!(
            matches!(env, ExecEnv::HostConda { .. }),
            "expected HostConda, got {:?}",
            env
        );
        assert_eq!(env.tier_name(), "host");
    }

    /// conda_probe=true but no env.lock present → falls through to None.
    #[test]
    fn provision_none_when_conda_available_but_no_lock() {
        let tmp = tempfile::tempdir().unwrap();
        write_det_env(tmp.path(), "differential_expression", "sha256:abcd1234");
        // No env.lock written.

        let opts = ProvisionOpts {
            allow_rebuild: false,
            docker_probe: || false,
            conda_probe: || true,
        };
        let env = provision(tmp.path(), &opts);
        assert_eq!(env, ExecEnv::None);
    }

    // ---- Tier 1 (Container) tests ----

    /// docker_probe=true + non-empty digest → Container tier selected.
    #[test]
    fn provision_container_when_docker_available_and_digest_present() {
        let tmp = tempfile::tempdir().unwrap();
        let digest = "sha256:0809cab6067dae3fcef66b2d70685e9ba041ec0597f1d534b6981e40d35d0ef5";
        write_det_env(tmp.path(), "differential_expression", digest);

        let opts = ProvisionOpts {
            allow_rebuild: false,
            docker_probe: || true,
            conda_probe: || false,
        };
        let env = provision(tmp.path(), &opts);
        assert_eq!(
            env,
            ExecEnv::Container {
                digest: digest.to_string()
            }
        );
        assert_eq!(env.tier_name(), "container");
    }

    /// docker_probe=true but digest is empty string → falls through past container tier.
    #[test]
    fn provision_skips_container_when_digest_empty() {
        let tmp = tempfile::tempdir().unwrap();
        write_det_env(tmp.path(), "differential_expression", "");
        write_env_lock(tmp.path(), "differential_expression");

        let opts = ProvisionOpts {
            allow_rebuild: false,
            docker_probe: || true,
            conda_probe: || true,
        };
        let env = provision(tmp.path(), &opts);
        // Should fall through to HostConda since conda_probe=true + env.lock present.
        assert!(
            matches!(env, ExecEnv::HostConda { .. }),
            "expected HostConda fallback when digest empty, got {:?}",
            env
        );
    }

    // ---- Tier 2 (RebuiltImage) tests ----

    /// allow_rebuild=true + docker_probe=true + Dockerfile at package root + empty
    /// digest → falls through Tier 1 and selects RebuiltImage.
    #[test]
    fn provision_rebuilt_when_dockerfile_present() {
        let tmp = tempfile::tempdir().unwrap();
        write_det_env(tmp.path(), "differential_expression", "");
        // Write a Dockerfile at package root.
        fs::write(tmp.path().join("Dockerfile"), "FROM ubuntu:22.04\n").unwrap();

        let opts = ProvisionOpts {
            allow_rebuild: true,
            docker_probe: || true, // Docker required for RebuiltImage (Tier 2 guard)
            conda_probe: || false,
        };
        let env = provision(tmp.path(), &opts);
        assert!(
            matches!(env, ExecEnv::RebuiltImage { .. }),
            "expected RebuiltImage, got {:?}",
            env
        );
        assert_eq!(env.tier_name(), "rebuilt");
    }

    /// docker_probe=true + empty digest + allow_rebuild=true + Dockerfile present
    /// → falls through Tier 1 (empty digest) and selects RebuiltImage (Tier 2).
    /// This also validates that the docker_probe guard on Tier 2 is satisfied.
    #[test]
    fn provision_rebuilt_when_docker_available_and_digest_empty() {
        let tmp = tempfile::tempdir().unwrap();
        write_det_env(tmp.path(), "differential_expression", ""); // empty digest → skip Tier 1
        fs::write(tmp.path().join("Dockerfile"), "FROM ubuntu:22.04\n").unwrap();

        let opts = ProvisionOpts {
            allow_rebuild: true,
            docker_probe: || true, // Docker present — required for Tier 2
            conda_probe: || false,
        };
        let env = provision(tmp.path(), &opts);
        assert!(
            matches!(env, ExecEnv::RebuiltImage { .. }),
            "expected RebuiltImage (Tier 2) with docker_probe=true and empty digest, got {:?}",
            env
        );
        assert_eq!(env.tier_name(), "rebuilt");
    }

    // ---- tier_name coverage ----

    #[test]
    fn tier_name_all_variants() {
        assert_eq!(
            ExecEnv::Container { digest: "d".into() }.tier_name(),
            "container"
        );
        assert_eq!(
            ExecEnv::RebuiltImage { tag: "t".into() }.tier_name(),
            "rebuilt"
        );
        assert_eq!(
            ExecEnv::HostConda {
                prefix: PathBuf::from("/env")
            }
            .tier_name(),
            "host"
        );
        assert_eq!(ExecEnv::None.tier_name(), "none");
    }

    // ---- run_script::None returns error ----

    #[test]
    fn run_script_none_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("test.R");
        fs::write(&script, "1+1\n").unwrap();
        let result = ExecEnv::None.run_script(&script, &BTreeMap::new(), tmp.path());
        assert!(result.is_err(), "ExecEnv::None::run_script must return Err");
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    // ---- interpreter_for ----

    #[test]
    fn interpreter_dispatch() {
        assert_eq!(interpreter_for(Path::new("foo.R")).unwrap(), "Rscript");
        assert_eq!(interpreter_for(Path::new("foo.py")).unwrap(), "python3");
        assert_eq!(interpreter_for(Path::new("foo.sh")).unwrap(), "bash");
        let err = interpreter_for(Path::new("foo.txt")).unwrap_err();
        assert!(
            err.to_string().contains("txt"),
            "error message should name the offending extension; got: {err}"
        );
    }
}
