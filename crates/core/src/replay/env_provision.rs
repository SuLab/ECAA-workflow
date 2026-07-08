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
    /// Re-use the exact image by digest recorded in the package. `conda_prefix`
    /// is the package's runtime-provisioned conda env (e.g.
    /// `runtime/cache/conda-envs/<name>`) that the agent activated via
    /// `conda run` at record time; when `Some`, the script is run through
    /// `conda run -p <prefix>` inside the container (the interpreter's libraries
    /// live in that env, not the base image), and the prefix is bind-mounted at
    /// its recorded path so the env's baked absolute paths resolve.
    Container {
        digest: String,
        conda_prefix: Option<PathBuf>,
    },
    /// A locally rebuilt image (from a Dockerfile or build spec in the package).
    RebuiltImage {
        tag: String,
        conda_prefix: Option<PathBuf>,
    },
    /// A host conda environment reconstructed from the task's `env.lock`.
    HostConda { prefix: PathBuf },
    /// No suitable environment found; re-execution is unavailable.
    None,
    /// Test-only variant: run `bash <script>` directly on the host shell.
    /// Gated by `#[cfg(test)]`; never present in production builds.
    #[cfg(test)]
    Shell,
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
                return ExecEnv::Container {
                    digest,
                    conda_prefix: detect_shipped_conda_prefix(pkg),
                };
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
            return ExecEnv::RebuiltImage {
                tag,
                conda_prefix: detect_shipped_conda_prefix(pkg),
            };
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

/// The package's runtime-provisioned conda env, if exactly one is shipped under
/// `runtime/cache/conda-envs/<name>`. That directory is where the execution
/// agent materialised the per-run conda env (e.g. `ecaa-bioc` carrying R +
/// DESeq2); the interpreter's libraries live there, not in the base image, so
/// the Container tier must run scripts through `conda run -p <this>`. Returns
/// `None` when the dir is absent or holds anything other than exactly one env
/// (ambiguous — fall back to the bare interpreter rather than guess).
fn detect_shipped_conda_prefix(pkg: &Path) -> Option<PathBuf> {
    let dir = pkg.join("runtime/cache/conda-envs");
    let mut envs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    envs.sort();
    // Exactly one env → use it; zero or many (ambiguous) → fall back to the
    // bare interpreter rather than guess the wrong env.
    if envs.len() == 1 {
        envs.pop()
    } else {
        None
    }
}

impl ExecEnv {
    /// Short label for the chosen tier.
    pub fn tier_name(&self) -> &'static str {
        match self {
            ExecEnv::Container { .. } => "container",
            ExecEnv::RebuiltImage { .. } => "rebuilt",
            ExecEnv::HostConda { .. } => "host",
            ExecEnv::None => "none",
            #[cfg(test)]
            ExecEnv::Shell => "shell",
        }
    }

    /// Build the full argument vector (program + args) for running `script`
    /// inside this execution environment.
    ///
    /// The returned vector's first element is the program to spawn; subsequent
    /// elements are its arguments. This is a pure function — it inspects no
    /// filesystem state and spawns nothing — making it fully unit-testable.
    ///
    /// Returns `Err` for `ExecEnv::None` or an unrecognised script extension.
    pub fn build_command(
        &self,
        script: &Path,
        env: &BTreeMap<String, String>,
        cwd: &Path,
    ) -> io::Result<Vec<String>> {
        let interp = interpreter_for(script)?;
        let script_str = script.display().to_string();
        let cwd_str = cwd.display().to_string();

        match self {
            ExecEnv::Container { digest, conda_prefix }
            | ExecEnv::RebuiltImage { tag: digest, conda_prefix } => {
                let mut args = vec![
                    "docker".to_string(),
                    "run".to_string(),
                    "--rm".to_string(),
                    "-v".to_string(),
                    format!("{cwd_str}:{cwd_str}"),
                    "-w".to_string(),
                    cwd_str.clone(),
                ];
                // Bind-mount the recorded conda env at its own absolute path so
                // `conda run -p <prefix>` (below) resolves the env and its baked
                // paths; the env lives outside the staged scratch (`cwd`).
                if let Some(prefix) = conda_prefix {
                    let p = prefix.display().to_string();
                    args.push("-v".to_string());
                    args.push(format!("{p}:{p}"));
                }
                // Run as the OWNER of the working dir. The image may default to a
                // non-root user (e.g. bio-min runs as uid 1001), which cannot
                // write into the host-owned scratch tree mounted at `cwd` — a
                // script writing its outputs/progress log then fails with EACCES.
                // The scratch dir is created by the replay caller as the host
                // user, so its owner uid:gid is exactly who must run the script.
                if let Ok(md) = std::fs::metadata(cwd) {
                    use std::os::unix::fs::MetadataExt;
                    args.push("--user".to_string());
                    args.push(format!("{}:{}", md.uid(), md.gid()));
                }
                // A host uid with no `/etc/passwd` entry inside the image has no
                // usable `$HOME`; point it at the writable working dir so tools
                // that consult `$HOME` (caches, config) do not fail. A recorded
                // HOME in `env` takes precedence.
                let mut saw_home = false;
                for (k, v) in env {
                    if k == "HOME" {
                        saw_home = true;
                    }
                    args.push("--env".to_string());
                    args.push(format!("{k}={v}"));
                }
                if !saw_home {
                    args.push("--env".to_string());
                    args.push(format!("HOME={cwd_str}"));
                }
                args.push(digest.clone());
                // Run the interpreter THROUGH the recorded conda env when present:
                // its libraries (e.g. DESeq2) live in that env, not the base image,
                // so a bare `Rscript` would fail. `--no-capture-output` keeps the
                // child's stderr intact for failed-run diagnosis (mirrors HostConda).
                if let Some(prefix) = conda_prefix {
                    args.push("conda".to_string());
                    args.push("run".to_string());
                    args.push("--no-capture-output".to_string());
                    args.push("-p".to_string());
                    args.push(prefix.display().to_string());
                }
                args.push(interp.to_string());
                args.push(script_str);
                Ok(args)
            }

            ExecEnv::HostConda { prefix } => {
                // `--no-capture-output` prevents conda ≥ 4.9 from buffering the
                // child's stdout/stderr into its own wrapper, ensuring the
                // `Output` returned to the caller contains the interpreter's
                // actual output (especially stderr, needed for failed-run
                // diagnosis).
                let mut args = vec![
                    "conda".to_string(),
                    "run".to_string(),
                    "--no-capture-output".to_string(),
                    "-p".to_string(),
                    prefix.display().to_string(),
                    "env".to_string(),
                ];
                for (k, v) in env {
                    args.push(format!("{k}={v}"));
                }
                args.push(interp.to_string());
                args.push(script_str);
                Ok(args)
            }

            ExecEnv::None => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no execution environment available (ExecEnv::None)",
            )),

            #[cfg(test)]
            ExecEnv::Shell => {
                // Test-only: run the script directly via `bash`.
                // Env vars are passed as `KEY=VALUE` prefix args to `env`.
                let mut args = vec!["env".to_string()];
                for (k, v) in env {
                    args.push(format!("{k}={v}"));
                }
                args.push("bash".to_string());
                args.push(script_str);
                Ok(args)
            }
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
        let argv = self.build_command(script, env, cwd)?;
        // argv[0] is the program; argv[1..] are its arguments.
        let (program, args) = argv.split_first().expect("build_command returns non-empty vec");
        let mut cmd = Command::new(program);
        cmd.args(args);
        // For HostConda, conda run manages the working directory via `-p`; for
        // docker, it is set inside the container via `-w`. For safety we also
        // set the process cwd to match in both cases.
        cmd.current_dir(cwd);
        cmd.output()
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
                digest: digest.to_string(),
                conda_prefix: None,
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
            ExecEnv::Container { digest: "d".into(), conda_prefix: None }.tier_name(),
            "container"
        );
        assert_eq!(
            ExecEnv::RebuiltImage { tag: "t".into(), conda_prefix: None }.tier_name(),
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

    // ---- build_command arg-vector tests ----

    /// HostConda with sorted env vars produces the expected argument vector,
    /// including `--no-capture-output` immediately after `run`.
    #[test]
    fn build_command_host_conda_arg_vector() {
        let prefix = PathBuf::from("/opt/conda/envs/ecaa-replay");
        let env_obj = ExecEnv::HostConda { prefix: prefix.clone() };
        let mut env = BTreeMap::new();
        env.insert("TZ".to_string(), "UTC".to_string());
        env.insert("LC_ALL".to_string(), "C.UTF-8".to_string());
        let script = Path::new("/work/analysis.R");
        let cwd = Path::new("/work");

        let argv = env_obj.build_command(script, &env, cwd).unwrap();

        // BTreeMap iterates in sorted key order: LC_ALL before TZ.
        assert_eq!(
            argv,
            vec![
                "conda",
                "run",
                "--no-capture-output",
                "-p",
                "/opt/conda/envs/ecaa-replay",
                "env",
                "LC_ALL=C.UTF-8",
                "TZ=UTC",
                "Rscript",
                "/work/analysis.R",
            ]
        );
    }

    /// Container variant produces the expected docker arg vector, including
    /// `--user <owner>` (so a non-root image default user can write the
    /// host-owned scratch) and a writable `HOME`.
    #[test]
    fn build_command_container_arg_vector() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let cwd_str = cwd.display().to_string();
        let md = std::fs::metadata(cwd).unwrap();
        let user = format!("{}:{}", md.uid(), md.gid());

        let env_obj = ExecEnv::Container { digest: "sha256:abc123".to_string(), conda_prefix: None };
        let mut env = BTreeMap::new();
        env.insert("SOURCE_DATE_EPOCH".to_string(), "1000000".to_string());
        env.insert("PYTHONHASHSEED".to_string(), "0".to_string());
        let script = cwd.join("run.py");

        let argv = env_obj.build_command(&script, &env, cwd).unwrap();

        assert_eq!(argv[0], "docker");
        assert_eq!(argv[1], "run");
        assert_eq!(argv[2], "--rm");
        assert!(argv
            .windows(2)
            .any(|w| w[0] == "-v" && w[1] == format!("{cwd_str}:{cwd_str}")));
        assert!(argv.windows(2).any(|w| w[0] == "-w" && w[1] == cwd_str));
        assert!(
            argv.windows(2).any(|w| w[0] == "--user" && w[1] == user),
            "container re-exec must run as the working-dir owner: {argv:?}"
        );
        // BTreeMap sorted: PYTHONHASHSEED before SOURCE_DATE_EPOCH.
        assert!(argv.contains(&"PYTHONHASHSEED=0".to_string()));
        assert!(argv.contains(&"SOURCE_DATE_EPOCH=1000000".to_string()));
        assert!(
            argv.contains(&format!("HOME={cwd_str}")),
            "HOME must point at the writable working dir: {argv:?}"
        );
        let n = argv.len();
        assert_eq!(argv[n - 3], "sha256:abc123");
        assert_eq!(argv[n - 2], "python3");
        assert_eq!(argv[n - 1], script.display().to_string());
    }

    /// With a recorded conda env, the Container tier must run the script THROUGH
    /// `conda run -p <prefix>` (its libraries live in that env, not the base
    /// image) and bind-mount the prefix at its own path.
    #[test]
    fn build_command_container_activates_conda_env() {
        let prefix = PathBuf::from("/pkg/runtime/cache/conda-envs/ecaa-bioc");
        let env_obj = ExecEnv::Container {
            digest: "sha256:abc".to_string(),
            conda_prefix: Some(prefix.clone()),
        };
        let env = BTreeMap::new();
        let script = Path::new("/scratch/differential_expression/scripts/01_run_deseq2.R");
        let argv = env_obj.build_command(script, &env, Path::new("/scratch")).unwrap();
        let joined = argv.join(" ");
        let p = prefix.display().to_string();
        assert!(
            argv.windows(2).any(|w| w[0] == "-v" && w[1] == format!("{p}:{p}")),
            "must bind-mount the conda prefix at its own path; got: {joined}"
        );
        assert!(
            joined.contains(&format!("conda run --no-capture-output -p {p}")),
            "must invoke the interpreter via `conda run -p <prefix>`; got: {joined}"
        );
        let ci = argv.iter().position(|a| a == "conda").expect("conda present");
        let ri = argv.iter().position(|a| a == "Rscript").expect("Rscript present");
        let si = argv.iter().position(|a| a == script.display().to_string().as_str()).unwrap();
        assert!(ci < ri && ri < si, "conda run must wrap the interpreter+script; got: {joined}");
    }

    /// provision() must detect the single runtime-provisioned conda env shipped
    /// under runtime/cache/conda-envs/ and attach it to the Container tier.
    #[test]
    fn provision_container_detects_single_shipped_conda_env() {
        let tmp = tempfile::tempdir().unwrap();
        let task = tmp.path().join("runtime/outputs/differential_expression");
        std::fs::create_dir_all(&task).unwrap();
        std::fs::write(
            task.join("determinism-env.json"),
            r#"{"task_container_digest":"sha256:deadbeef"}"#,
        )
        .unwrap();
        let env_dir = tmp.path().join("runtime/cache/conda-envs/ecaa-bioc");
        std::fs::create_dir_all(&env_dir).unwrap();
        let opts = ProvisionOpts {
            allow_rebuild: false,
            docker_probe: || true,
            conda_probe: || false,
        };
        match provision(tmp.path(), &opts) {
            ExecEnv::Container { conda_prefix, .. } => {
                assert_eq!(conda_prefix, Some(env_dir), "must detect the single shipped conda env");
            }
            other => panic!("expected Container tier, got {other:?}"),
        }
    }

    /// RebuiltImage variant produces the same docker structure as Container
    /// (same code path), verified here with a .sh script → bash interpreter.
    #[test]
    fn build_command_rebuilt_image_arg_vector() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let cwd_str = cwd.display().to_string();
        let md = std::fs::metadata(cwd).unwrap();
        let user = format!("{}:{}", md.uid(), md.gid());

        let env_obj = ExecEnv::RebuiltImage { tag: "ecaa-replay:mypkg".to_string(), conda_prefix: None };
        let env: BTreeMap<String, String> = BTreeMap::new();
        let script = cwd.join("setup.sh");

        let argv = env_obj.build_command(&script, &env, cwd).unwrap();

        assert_eq!(argv[0], "docker");
        assert!(argv.windows(2).any(|w| w[0] == "--user" && w[1] == user));
        // No HOME in env → re-exec injects HOME pointing at the writable cwd.
        assert!(argv.contains(&format!("HOME={cwd_str}")));
        let n = argv.len();
        assert_eq!(argv[n - 3], "ecaa-replay:mypkg");
        assert_eq!(argv[n - 2], "bash");
        assert_eq!(argv[n - 1], script.display().to_string());
    }

    /// build_command returns Err for ExecEnv::None.
    #[test]
    fn build_command_none_returns_error() {
        let script = Path::new("/work/run.R");
        let cwd = Path::new("/work");
        let result = ExecEnv::None.build_command(script, &BTreeMap::new(), cwd);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
    }

    /// Interpreter dispatch within build_command: .R → Rscript, .py → python3,
    /// .sh → bash (spot-checked via HostConda to keep it concise).
    #[test]
    fn build_command_interpreter_dispatch_by_extension() {
        let prefix = PathBuf::from("/env");
        let env: BTreeMap<String, String> = BTreeMap::new();
        let cwd = Path::new("/w");

        let r_argv = ExecEnv::HostConda { prefix: prefix.clone() }
            .build_command(Path::new("/w/a.R"), &env, cwd)
            .unwrap();
        assert_eq!(r_argv[r_argv.len() - 2], "Rscript");

        let py_argv = ExecEnv::HostConda { prefix: prefix.clone() }
            .build_command(Path::new("/w/b.py"), &env, cwd)
            .unwrap();
        assert_eq!(py_argv[py_argv.len() - 2], "python3");

        let sh_argv = ExecEnv::HostConda { prefix: prefix.clone() }
            .build_command(Path::new("/w/c.sh"), &env, cwd)
            .unwrap();
        assert_eq!(sh_argv[sh_argv.len() - 2], "bash");
    }
}
