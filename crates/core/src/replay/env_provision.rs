// crates/core/src/replay/env_provision.rs
//
// Tiered execution-environment provisioning for replay mode.
//
// Given a downloaded ECAA package, chooses the best available execution
// environment for re-running the saved compute scripts, in order of
// reproducibility. The tier waterfall is:
//
//   Container → InstallFromLock → RebuiltImage → None
//
//   Container       — exact recorded image digest (best). When a conda env
//                     shipped in the package is present, scripts run through it.
//   InstallFromLock — recorded image digest + a pinned EXPLICIT conda lock but
//                     no shipped env: install the env from the lock into the
//                     image at replay time (one gated, network-bearing step),
//                     then run hermetically. Chosen inside the Container tier.
//   RebuiltImage    — rebuild from a Dockerfile/build spec in the package.
//   None            — no environment available; re-execution is not possible.

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
        /// Absolute path the env was CREATED at (its R-libs / interpreter
        /// prefixes are baked to this path). The env is bind-mounted
        /// `conda_prefix:conda_mount_at` and run via `conda run -p conda_mount_at`,
        /// so a relocated package's env still resolves its baked paths. `None`
        /// (or equal to `conda_prefix`) mounts at the on-disk path unchanged.
        conda_mount_at: Option<PathBuf>,
    },
    /// A locally rebuilt image (from a Dockerfile or build spec in the package).
    RebuiltImage {
        tag: String,
        conda_prefix: Option<PathBuf>,
        conda_mount_at: Option<PathBuf>,
    },
    /// Install a conda env from the package's recorded EXPLICIT lock
    /// (`env.explicit.lock` — a `conda create --file`-compatible pinned
    /// URL+md5 spec) into a fresh prefix inside `digest`, then run scripts
    /// through it. The install is a deterministic, network-bearing step
    /// performed once by `run_replay` before the task loop; execution then
    /// behaves like `Container { conda_prefix: <installed> }` run hermetically.
    /// This is the portable, self-contained re-execution path for packages that
    /// LOG their environment rather than ship its bytes.
    InstallFromLock { digest: String, lock: PathBuf },
    /// No suitable environment found; re-execution is unavailable.
    None,
    /// Test-only variant: run `bash <script>` directly on the host shell.
    /// Gated by `#[cfg(test)]`; never present in production builds.
    #[cfg(test)]
    Shell,
}

/// Options that control and instrument the provisioning decision.
///
/// The probe function is injected so tests can run hermetically without
/// making any real Docker system calls.
pub struct ProvisionOpts {
    /// Allow rebuilding the image from a Dockerfile/build spec found in the
    /// package when the recorded digest is unavailable.
    pub allow_rebuild: bool,
    /// Returns `true` when Docker is available on the host.
    pub docker_probe: fn() -> bool,
    /// Returns `true` when the given image (by digest/ID/tag) is present
    /// locally. Injected so tests can drive the fallback deterministically.
    pub image_probe: fn(&str) -> bool,
    /// A current base image to fall back to when the recorded per-task snapshot
    /// image is absent (e.g. garbage-collected). `None` disables the fallback,
    /// in which case an absent recorded image is kept (and fails loudly at run
    /// time, preserving exact-image reproducibility semantics).
    pub fallback_image: Option<String>,
}

/// Resolve the recorded image against local availability. Returns the effective
/// digest to use. When the recorded image is absent and a `fallback_image` is
/// configured and present, swaps to it and logs a loud **image-drift** warning —
/// the replay then reproduces against a DIFFERENT image than recorded, trading
/// exact-image fidelity for availability. `run_replay` surfaces the drift in the
/// report's `env_tier`. When no usable fallback exists the recorded digest is
/// returned unchanged (it fails loudly downstream).
fn resolve_recorded_image(recorded: &str, opts: &ProvisionOpts) -> String {
    if recorded.is_empty() || (opts.image_probe)(recorded) {
        return recorded.to_string();
    }
    if let Some(fb) = &opts.fallback_image {
        if !fb.is_empty() && (opts.image_probe)(fb) {
            tracing::warn!(
                "replay: recorded image {recorded} is absent locally; falling back to {fb} \
                 (IMAGE DRIFT — reproducing against a different image than was recorded)"
            );
            return fb.clone();
        }
    }
    recorded.to_string()
}

/// Select an execution environment for re-running the compute tasks in `pkg`.
///
/// Reads the first compute task's `runtime/outputs/<task>/determinism-env.json`
/// for the `task_container_digest`, then applies the tier waterfall:
///
/// 1. **Container** — `opts.docker_probe()` true, digest non-empty. Inside
///    this tier, a recorded EXPLICIT lock with no shipped env selects the
///    **InstallFromLock** variant.
/// 2. **RebuiltImage** — `opts.allow_rebuild` true, a Dockerfile is present
///    directly under `runtime/outputs/<task>/` or at the package root.
/// 3. **None** — fallback.
pub fn provision(pkg: &Path, opts: &ProvisionOpts, recorded_root: &str) -> ExecEnv {
    // Find the first eligible compute task by scanning runtime/outputs/ in
    // lexicographic order. A task is "eligible" if it has a determinism-env.json.
    let outputs = pkg.join("runtime/outputs");
    let first_task_dir = find_first_task_dir(&outputs);
    let conda_prefix = detect_shipped_conda_prefix(pkg);
    let conda_mount_at = recorded_conda_mount(pkg, conda_prefix.as_deref(), recorded_root);

    // --- Tier 1: Container by digest ---
    if (opts.docker_probe)() {
        if let Some(ref task_dir) = first_task_dir {
            // Resolve the recorded image against local availability, falling back
            // to a current base image (with a loud drift warning) when the exact
            // recorded snapshot has been garbage-collected.
            let digest = resolve_recorded_image(&read_container_digest(task_dir), opts);
            if !digest.is_empty() {
                // 1a. A conda env shipped in the package → run through it.
                if conda_prefix.is_some() {
                    return ExecEnv::Container {
                        digest,
                        conda_prefix,
                        conda_mount_at,
                    };
                }
                // 1b. No shipped env, but a recorded EXPLICIT lock → install the
                // env deterministically from the lock into the image (the lean,
                // self-contained re-execution path).
                if let Some(lock) = detect_explicit_lock(pkg) {
                    return ExecEnv::InstallFromLock { digest, lock };
                }
                // 1c. Neither → run the image's bare interpreter.
                return ExecEnv::Container {
                    digest,
                    conda_prefix,
                    conda_mount_at,
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
                conda_prefix,
                conda_mount_at,
            };
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

/// The absolute path the shipped conda env was CREATED at, so a relocated
/// package's env can be bind-mounted back to its baked prefix.
///
/// The env's R-libs / interpreter shebangs hardcode the path it was built at
/// (`<recorded_root>/runtime/cache/conda-envs/<name>`); when the package is
/// later moved (e.g. exported as a deposit under a new directory name) the env
/// on disk lives at `<pkg>/runtime/cache/conda-envs/<name>` but its innards
/// still reference the recorded path. Mapping the on-disk env to that recorded
/// path inside the container is what makes `conda run -p <recorded>` resolve.
///
/// Returns `None` (⇒ mount at the on-disk path unchanged) when there is no
/// shipped env, `recorded_root` is empty/unknown, the on-disk path is not under
/// `pkg`, or the recorded path already equals the on-disk path.
fn recorded_conda_mount(
    pkg: &Path,
    on_disk: Option<&Path>,
    recorded_root: &str,
) -> Option<PathBuf> {
    let on_disk = on_disk?;
    if recorded_root.is_empty() {
        return None;
    }
    let rel = on_disk.strip_prefix(pkg).ok()?;
    let mount_at = Path::new(recorded_root).join(rel);
    (mount_at != on_disk).then_some(mount_at)
}

/// The package's recorded EXPLICIT conda lock, if present — a
/// `conda create --file`-compatible pinned (URL+md5) spec that lets replay
/// deterministically re-install the analysis environment rather than ship its
/// bytes. Preference: a package-level `runtime/env.explicit.lock`, else the
/// first `runtime/outputs/<task>/env.explicit.lock` in lexicographic order.
/// (`env.lock` — R `sessionInfo()` — is NOT a lock and is deliberately ignored.)
fn detect_explicit_lock(pkg: &Path) -> Option<PathBuf> {
    let pkg_level = pkg.join("runtime/env.explicit.lock");
    if pkg_level.is_file() {
        return Some(pkg_level);
    }
    let outputs = pkg.join("runtime/outputs");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&outputs)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.into_iter()
        .map(|d| d.join("env.explicit.lock"))
        .find(|p| p.is_file())
}

/// Build the `docker run …` argv that deterministically installs a conda env
/// from `lock` into `env_target` (a fresh prefix under the replay scratch)
/// inside `image`. Pure + unit-testable. The install is the ONE network-bearing
/// replay step; the subsequent script runs are network-isolated.
///
/// `env_target` must live under the caller's writable scratch; its parent is
/// bind-mounted so the freshly-created env persists on the host for the run,
/// and the container runs as the scratch owner so conda can write it.
fn build_install_command(image: &str, lock: &Path, env_target: &Path) -> Vec<String> {
    let lock_s = lock.display().to_string();
    let target_s = env_target.display().to_string();
    // Mount the scratch parent so the created env is host-visible + persists.
    let parent = env_target
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| target_s.clone());
    let mut args = vec![
        "docker".to_string(),
        "run".to_string(),
        "--rm".to_string(),
        // conda create must reach the package registries recorded in the lock,
        // so (unlike script execution) this step is NOT network-isolated. It is
        // still deterministic: the lock pins every package by URL + md5, and its
        // hosts are gated against the registry allowlist before this runs.
        //
        // Even with network ON, bound the install of an UNTRUSTED lock: drop all
        // Linux capabilities, forbid privilege escalation, cap the process table
        // (fork-bomb defense), and cap memory so a hostile post-install hook
        // can't wedge the host. The lock is mounted read-only.
        "--cap-drop".to_string(),
        "ALL".to_string(),
        "--security-opt".to_string(),
        "no-new-privileges".to_string(),
        "--pids-limit".to_string(),
        "512".to_string(),
        "--memory".to_string(),
        "4g".to_string(),
        "-v".to_string(),
        format!("{parent}:{parent}"),
        "-v".to_string(),
        format!("{lock_s}:{lock_s}:ro"),
    ];
    if let Ok(md) = std::fs::metadata(&parent) {
        use std::os::unix::fs::MetadataExt;
        args.push("--user".to_string());
        args.push(format!("{}:{}", md.uid(), md.gid()));
    }
    // conda needs a writable HOME/pkgs cache; point them at the scratch parent.
    args.push("--env".to_string());
    args.push(format!("HOME={parent}"));
    args.push(image.to_string());
    args.push("conda".to_string());
    args.push("create".to_string());
    args.push("-y".to_string());
    args.push("-p".to_string());
    args.push(target_s);
    args.push("--file".to_string());
    args.push(lock_s);
    // Name the environment format explicitly. conda 26.x removed format
    // auto-detection for a `--file` install (`EnvironmentSpecPluginNotDetected:
    // Unable to detect the environment format … add --env-spec`); env.explicit.lock
    // is always the `explicit` format (`@EXPLICIT` + pinned URL+md5 package
    // lines), so name it. `--environment-specifier` is the non-deprecated spelling
    // (the shorter `--env-spec` alias is pending deprecation).
    args.push("--environment-specifier".to_string());
    args.push("explicit".to_string());
    args
}

/// Run [`build_install_command`], returning `Ok(())` on a zero exit. Stderr is
/// surfaced in the error so a failed install is diagnosable.
pub fn install_conda_env_from_lock(image: &str, lock: &Path, env_target: &Path) -> io::Result<()> {
    let argv = build_install_command(image, lock, env_target);
    let (program, rest) = argv.split_first().expect("non-empty argv");
    let out = Command::new(program).args(rest).output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "conda create from lock {} failed (status {:?}): {}",
            lock.display(),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )))
    }
}

impl ExecEnv {
    /// Short label for the chosen tier.
    pub fn tier_name(&self) -> &'static str {
        match self {
            ExecEnv::Container { .. } => "container",
            ExecEnv::RebuiltImage { .. } => "rebuilt",
            ExecEnv::InstallFromLock { .. } => "install-from-lock",
            ExecEnv::None => "none",
            #[cfg(test)]
            ExecEnv::Shell => "shell",
        }
    }

    /// The image the env will actually run against (digest/ID/tag), if any. Used
    /// by `run_replay` to detect image drift (effective image ≠ recorded).
    pub(crate) fn effective_image(&self) -> Option<&str> {
        match self {
            ExecEnv::Container { digest, .. } | ExecEnv::InstallFromLock { digest, .. } => {
                Some(digest.as_str())
            }
            ExecEnv::RebuiltImage { tag, .. } => Some(tag.as_str()),
            ExecEnv::None => None,
            #[cfg(test)]
            ExecEnv::Shell => None,
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

        // Route the interpreter THROUGH the recorded conda env only for R.
        //
        // The ECAA execution model builds the shipped/installed conda env for the
        // R + Bioconductor stack — DESeq2 / clusterProfiler / GO.db live there,
        // not in the base image — while Python and shell stages run against the
        // base image's scientific-Python stack (numpy / pandas / matplotlib).
        // This mirrors the per-task environments the agent actually recorded (R
        // stages' `sessionInfo()` → the conda env; Python stages → the base
        // image's python 3.11). Wrapping a Python script in `conda run -p <R-env>`
        // resolves `python3` to the R env's interpreter, which lacks numpy, and
        // fails with `ModuleNotFoundError` — the reason multi-language replay
        // never reproduced. So the conda env is bind-mounted and activated only
        // when the script is R; Python/shell run the base image interpreter.
        let route_through_conda = interp == "Rscript";

        match self {
            ExecEnv::Container { digest, conda_prefix, conda_mount_at }
            | ExecEnv::RebuiltImage { tag: digest, conda_prefix, conda_mount_at } => {
                let mut args = vec![
                    "docker".to_string(),
                    "run".to_string(),
                    "--rm".to_string(),
                    // Hardening for replay re-execution. Replay must be hermetic
                    // (no external inputs → deterministic, correct for
                    // reproducibility) AND must bound an untrusted image pulled
                    // from an imported package: no network egress, no Linux
                    // capabilities, no privilege escalation, and a bounded
                    // process table so a fork-bomb can't wedge the host.
                    "--network".to_string(),
                    "none".to_string(),
                    "--cap-drop".to_string(),
                    "ALL".to_string(),
                    "--security-opt".to_string(),
                    "no-new-privileges".to_string(),
                    "--pids-limit".to_string(),
                    "512".to_string(),
                    "-v".to_string(),
                    format!("{cwd_str}:{cwd_str}"),
                    "-w".to_string(),
                    cwd_str.clone(),
                ];
                // Bind-mount the shipped conda env at the absolute path it was
                // CREATED at (`conda_mount_at`, falling back to the on-disk path)
                // so `conda run -p <that path>` (below) resolves the env and its
                // baked R-lib / interpreter prefixes even when the package was
                // relocated (e.g. a deposit whose env now lives at a new path).
                // Only mounted for R scripts (see `route_through_conda`): a Python
                // stage runs against the base image and never touches this env.
                if route_through_conda {
                    if let Some(prefix) = conda_prefix {
                        let src = prefix.display().to_string();
                        let dst = conda_mount_at.as_deref().unwrap_or(prefix).display().to_string();
                        args.push("-v".to_string());
                        args.push(format!("{src}:{dst}"));
                    }
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
                // Run the R interpreter THROUGH the recorded conda env: its
                // libraries (e.g. DESeq2) live in that env, not the base image, so
                // a bare `Rscript` would fail. `--no-capture-output` keeps the
                // child's stderr intact for failed-run diagnosis. Python/shell
                // stages skip this and run the base image interpreter directly
                // (see `route_through_conda`).
                if route_through_conda {
                    if let Some(prefix) = conda_prefix {
                        let dst = conda_mount_at.as_deref().unwrap_or(prefix).display().to_string();
                        args.push("conda".to_string());
                        args.push("run".to_string());
                        args.push("--no-capture-output".to_string());
                        args.push("-p".to_string());
                        args.push(dst);
                    }
                }
                args.push(interp.to_string());
                args.push(script_str);
                Ok(args)
            }

            ExecEnv::None => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no execution environment available (ExecEnv::None)",
            )),

            ExecEnv::InstallFromLock { .. } => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "InstallFromLock must be materialized into a Container (conda env installed \
                 from the lock) by run_replay before scripts are run",
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
        // The container tiers set the working directory inside the container via
        // `-w`; for safety we also set the process cwd to match.
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

/// The recorded `task_container_digest` of the package's first eligible compute
/// task (lex order), or empty when none. `run_replay` compares this to the
/// env's effective image to detect fallback-induced image drift.
pub(crate) fn recorded_image(pkg: &Path) -> String {
    find_first_task_dir(&pkg.join("runtime/outputs"))
        .map(|d| read_container_digest(&d))
        .unwrap_or_default()
}

/// Read `task_container_digest` from `<task_dir>/determinism-env.json`.
/// Returns an empty string if the file is absent or the field is missing.
pub(crate) fn read_container_digest(task_dir: &Path) -> String {
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

/// Return `true` when `script` has an extension this runner knows how to
/// dispatch (`.R`, `.py`, `.sh`) — i.e. `interpreter_for` would succeed.
///
/// The recorded runs write logs/manifests/data files (`.log`, `.json`, `.tsv`,
/// …) into the same `scripts/` directory as the real compute scripts. The
/// replay runner must execute only genuine scripts and treat everything else as
/// inert co-located artifacts; this predicate is that gate (single source of
/// truth with `interpreter_for`).
pub fn is_runnable_script(script: &Path) -> bool {
    interpreter_for(script).is_ok()
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

    // ---- None-tier tests ----

    /// Docker absent + no rebuild + no fallback tier → None.
    #[test]
    fn provision_none_when_docker_absent() {
        let tmp = tempfile::tempdir().unwrap();
        write_det_env(tmp.path(), "differential_expression", "sha256:abcd1234");

        let opts = ProvisionOpts {
            allow_rebuild: false,
            docker_probe: || false,
            image_probe: |_| true,
            fallback_image: None,
        };
        let env = provision(tmp.path(), &opts, "");
        assert_eq!(env, ExecEnv::None);
        assert_eq!(env.tier_name(), "none");
    }

    /// With an empty package (no runtime/outputs), provision → None regardless
    /// of the docker probe.
    #[test]
    fn provision_none_empty_package() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = ProvisionOpts {
            allow_rebuild: true,
            docker_probe: || true,
            image_probe: |_| true,
            fallback_image: None,
        };
        let env = provision(tmp.path(), &opts, "");
        assert_eq!(env, ExecEnv::None);
    }

    /// The HostConda tier is retired: a package that ships only an `env.lock`
    /// (no container digest, no EXPLICIT lock, no Dockerfile) has NO
    /// re-executable environment → None. `env.lock` (R `sessionInfo()`) is not
    /// a provisioning source.
    #[test]
    fn provision_none_when_only_env_lock_present() {
        let tmp = tempfile::tempdir().unwrap();
        write_det_env(tmp.path(), "differential_expression", ""); // no digest
        write_env_lock(tmp.path(), "differential_expression"); // only an env.lock

        let opts = ProvisionOpts {
            allow_rebuild: false,
            docker_probe: || true, // even WITH docker there is no lock/digest to use
            image_probe: |_| true,
            fallback_image: None,
        };
        let env = provision(tmp.path(), &opts, "");
        assert_eq!(
            env,
            ExecEnv::None,
            "env.lock alone must not provision any tier (HostConda retired)"
        );
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
            image_probe: |_| true,
            fallback_image: None,
        };
        let env = provision(tmp.path(), &opts, "");
        assert_eq!(
            env,
            ExecEnv::Container {
                digest: digest.to_string(),
                conda_prefix: None,
                conda_mount_at: None,
            }
        );
        assert_eq!(env.tier_name(), "container");
    }

    /// docker_probe=true but digest is empty string → Container tier is skipped;
    /// with no EXPLICIT lock and no Dockerfile the result is None (the retired
    /// HostConda tier no longer catches an `env.lock`).
    #[test]
    fn provision_none_when_digest_empty_and_env_lock_only() {
        let tmp = tempfile::tempdir().unwrap();
        write_det_env(tmp.path(), "differential_expression", "");
        write_env_lock(tmp.path(), "differential_expression");

        let opts = ProvisionOpts {
            allow_rebuild: false,
            docker_probe: || true,
            image_probe: |_| true,
            fallback_image: None,
        };
        let env = provision(tmp.path(), &opts, "");
        assert_eq!(
            env,
            ExecEnv::None,
            "empty digest + only env.lock must yield None, got {:?}",
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
            image_probe: |_| true,
            fallback_image: None,
        };
        let env = provision(tmp.path(), &opts, "");
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
            image_probe: |_| true,
            fallback_image: None,
        };
        let env = provision(tmp.path(), &opts, "");
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
            ExecEnv::Container { digest: "d".into(), conda_prefix: None, conda_mount_at: None }.tier_name(),
            "container"
        );
        assert_eq!(
            ExecEnv::RebuiltImage { tag: "t".into(), conda_prefix: None, conda_mount_at: None }.tier_name(),
            "rebuilt"
        );
        assert_eq!(
            ExecEnv::InstallFromLock { digest: "d".into(), lock: PathBuf::from("/l") }.tier_name(),
            "install-from-lock"
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

        let env_obj = ExecEnv::Container { digest: "sha256:abc123".to_string(), conda_prefix: None, conda_mount_at: None };
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

    /// Replay re-execution must be hermetic + safe against an untrusted imported
    /// image: the docker argv carries `--network none`, `--cap-drop ALL`,
    /// `--security-opt no-new-privileges`, and a bounded `--pids-limit`, all
    /// positioned after `--rm` and before the image reference.
    #[test]
    fn build_command_container_is_hardened() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        let env_obj = ExecEnv::Container { digest: "sha256:abc123".to_string(), conda_prefix: None, conda_mount_at: None };
        let env = BTreeMap::new();
        let script = cwd.join("run.py");

        let argv = env_obj.build_command(&script, &env, cwd).unwrap();

        assert!(
            argv.windows(2).any(|w| w[0] == "--network" && w[1] == "none"),
            "replay container must have no network egress: {argv:?}"
        );
        assert!(
            argv.windows(2).any(|w| w[0] == "--cap-drop" && w[1] == "ALL"),
            "replay container must drop all capabilities: {argv:?}"
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--security-opt" && w[1] == "no-new-privileges"),
            "replay container must forbid privilege escalation: {argv:?}"
        );
        assert!(
            argv.windows(2).any(|w| w[0] == "--pids-limit" && w[1] == "512"),
            "replay container must bound its process table: {argv:?}"
        );
        // The hardening flags precede the image reference (argv[n-3] is the
        // digest for a python script with no conda env).
        let net = argv.iter().position(|a| a == "--network").unwrap();
        let img = argv.len() - 3;
        assert!(net < img, "hardening flags must come before the image: {argv:?}");
        assert_eq!(argv[img], "sha256:abc123");
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
            // No recorded remap → mount at the on-disk path unchanged.
            conda_mount_at: None,
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

    /// A shipped conda env baked at a different absolute path (the recorded
    /// creation path) must be bind-mounted AT that recorded path and run via
    /// `conda run -p <recorded>`, so a RELOCATED deposit's env still resolves
    /// its baked R-library / interpreter prefixes. The on-disk source path is
    /// the mount source; `conda_mount_at` is the mount target.
    #[test]
    fn build_command_mounts_conda_env_at_recorded_path() {
        let on_disk = PathBuf::from("/deposit/runtime/cache/conda-envs/ecaa-bioc");
        let recorded = PathBuf::from("/orig/pkg/runtime/cache/conda-envs/ecaa-bioc");
        let env_obj = ExecEnv::Container {
            digest: "sha256:abc".to_string(),
            conda_prefix: Some(on_disk.clone()),
            conda_mount_at: Some(recorded.clone()),
        };
        let env = BTreeMap::new();
        let script = Path::new("/scratch/differential_expression/scripts/01_run_deseq2.R");
        let argv = env_obj.build_command(script, &env, Path::new("/scratch")).unwrap();
        let joined = argv.join(" ");
        assert!(
            argv.windows(2).any(|w| w[0] == "-v"
                && w[1] == format!("{}:{}", on_disk.display(), recorded.display())),
            "must bind-mount on-disk env at the RECORDED path; got: {joined}"
        );
        assert!(
            joined.contains(&format!("conda run --no-capture-output -p {}", recorded.display())),
            "must `conda run -p` the RECORDED path, not the on-disk path; got: {joined}"
        );
        assert!(
            !joined.contains(&format!("-p {}", on_disk.display())),
            "must NOT run against the on-disk path; got: {joined}"
        );
    }

    /// Python and shell stages must NOT be wrapped in `conda run -p <prefix>`:
    /// the shipped/installed conda env is the agent-built R + Bioconductor env,
    /// and its python lacks the scientific-Python stack (numpy / pandas). At
    /// record time the Python stages ran against the BASE IMAGE's python, so
    /// replay must run them there too — wrapping them in the R env's `conda run`
    /// resolves `python3` to that env and fails with `ModuleNotFoundError: numpy`
    /// (the reason multi-language replay never reproduced). R scripts still route
    /// through the env, and the R env is not even mounted into a Python container.
    #[test]
    fn build_command_python_bypasses_conda_env_r_uses_it() {
        let prefix = PathBuf::from("/pkg/runtime/cache/conda-envs/ecaa-bioc");
        let env_obj = ExecEnv::Container {
            digest: "sha256:abc".to_string(),
            conda_prefix: Some(prefix.clone()),
            conda_mount_at: None,
        };
        let env = BTreeMap::new();
        let cwd = Path::new("/scratch");

        // R → routed through the conda env (DESeq2 lives there).
        let r_argv = env_obj
            .build_command(Path::new("/scratch/de/scripts/01_de.R"), &env, cwd)
            .unwrap();
        assert!(
            r_argv.windows(2).any(|w| w[0] == "conda" && w[1] == "run"),
            "R must run via `conda run`; got: {}",
            r_argv.join(" ")
        );

        // Python → base image python3, NOT `conda run`.
        let py_argv = env_obj
            .build_command(Path::new("/scratch/qc/scripts/01_qc.py"), &env, cwd)
            .unwrap();
        assert!(
            !py_argv.windows(2).any(|w| w[0] == "conda" && w[1] == "run"),
            "Python must NOT run via `conda run`; got: {}",
            py_argv.join(" ")
        );
        assert_eq!(
            py_argv[py_argv.len() - 2],
            "python3",
            "python3 must be invoked directly; got: {}",
            py_argv.join(" ")
        );
        // The R env must not be bind-mounted into a Python container.
        let p = prefix.display().to_string();
        assert!(
            !py_argv.windows(2).any(|w| w[0] == "-v" && w[1] == format!("{p}:{p}")),
            "Python container must not mount the R conda env; got: {}",
            py_argv.join(" ")
        );
    }

    /// With a recorded digest + an explicit lock + NO shipped conda env,
    /// provision selects the InstallFromLock tier (install the env from the
    /// pinned lock into the image at replay time) rather than a bare Container.
    #[test]
    fn provision_install_from_lock_when_lock_present_no_shipped_env() {
        let tmp = tempfile::tempdir().unwrap();
        write_det_env(tmp.path(), "differential_expression", "sha256:deadbeef");
        // Package-level explicit lock, no shipped conda env dir.
        let rt = tmp.path().join("runtime");
        fs::create_dir_all(&rt).unwrap();
        fs::write(rt.join("env.explicit.lock"), "@EXPLICIT\nhttps://x/p.tar.bz2#abc\n").unwrap();

        let opts = ProvisionOpts {
            allow_rebuild: false,
            docker_probe: || true,
            image_probe: |_| true,
            fallback_image: None,
        };
        match provision(tmp.path(), &opts, "") {
            ExecEnv::InstallFromLock { digest, lock } => {
                assert_eq!(digest, "sha256:deadbeef");
                assert_eq!(lock, rt.join("env.explicit.lock"));
            }
            other => panic!("expected InstallFromLock, got {other:?}"),
        }
    }

    /// `resolve_recorded_image`: keep the recorded image when present; swap to a
    /// present fallback when it's absent (image drift); keep the recorded image
    /// when there is no usable fallback (fails loudly downstream, preserving
    /// exact-image semantics).
    #[test]
    fn resolve_recorded_image_falls_back_only_when_recorded_absent() {
        let with_fb = |probe: fn(&str) -> bool| ProvisionOpts {
            allow_rebuild: false,
            docker_probe: || true,
            image_probe: probe,
            fallback_image: Some("bio-min:local".to_string()),
        };
        // Recorded present → unchanged (no drift).
        assert_eq!(resolve_recorded_image("sha256:abc", &with_fb(|_| true)), "sha256:abc");
        // Recorded absent, fallback present → fallback.
        assert_eq!(
            resolve_recorded_image("sha256:gone", &with_fb(|i| i == "bio-min:local")),
            "bio-min:local"
        );
        // Recorded absent, fallback ALSO absent → keep recorded.
        assert_eq!(resolve_recorded_image("sha256:gone", &with_fb(|_| false)), "sha256:gone");
        // Recorded absent, fallback disabled (None) → keep recorded.
        let no_fb = ProvisionOpts {
            allow_rebuild: false,
            docker_probe: || true,
            image_probe: |_| false,
            fallback_image: None,
        };
        assert_eq!(resolve_recorded_image("sha256:gone", &no_fb), "sha256:gone");
    }

    /// End-to-end: when the recorded snapshot image is absent, `provision` binds
    /// the InstallFromLock tier to the fallback image instead of failing.
    #[test]
    fn provision_uses_fallback_image_when_recorded_absent() {
        let tmp = tempfile::tempdir().unwrap();
        write_det_env(tmp.path(), "differential_expression", "sha256:gone");
        let rt = tmp.path().join("runtime");
        fs::create_dir_all(&rt).unwrap();
        fs::write(rt.join("env.explicit.lock"), "@EXPLICIT\nhttps://x/p.tar.bz2#abc\n").unwrap();
        let opts = ProvisionOpts {
            allow_rebuild: false,
            docker_probe: || true,
            image_probe: |i| i == "bio-min:local", // recorded absent; fallback present
            fallback_image: Some("bio-min:local".to_string()),
        };
        match provision(tmp.path(), &opts, "") {
            ExecEnv::InstallFromLock { digest, .. } => {
                assert_eq!(digest, "bio-min:local", "must bind to the fallback image");
            }
            other => panic!("expected InstallFromLock on the fallback image, got {other:?}"),
        }
    }

    /// detect_explicit_lock prefers the package-level lock over a per-task one.
    #[test]
    fn detect_explicit_lock_prefers_package_level() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path().join("runtime");
        let task = rt.join("outputs/differential_expression");
        fs::create_dir_all(&task).unwrap();
        fs::write(task.join("env.explicit.lock"), "@EXPLICIT\n").unwrap();
        // Only per-task present → returns the per-task lock.
        assert_eq!(
            detect_explicit_lock(tmp.path()),
            Some(task.join("env.explicit.lock"))
        );
        // Add a package-level lock → now preferred.
        fs::write(rt.join("env.explicit.lock"), "@EXPLICIT\n").unwrap();
        assert_eq!(
            detect_explicit_lock(tmp.path()),
            Some(rt.join("env.explicit.lock"))
        );
    }

    /// build_install_command emits a deterministic `conda create -p <target>
    /// --file <lock>` docker invocation with the scratch parent bind-mounted.
    #[test]
    fn build_install_command_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join(".replay-conda-env");
        let lock = tmp.path().join("env.explicit.lock");
        fs::write(&lock, "@EXPLICIT\n").unwrap();
        let argv = build_install_command("sha256:img", &lock, &target);
        let joined = argv.join(" ");
        assert_eq!(argv[0], "docker");
        assert!(joined.contains("run --rm"), "got: {joined}");
        let parent = tmp.path().display().to_string();
        assert!(
            argv.windows(2).any(|w| w[0] == "-v" && w[1] == format!("{parent}:{parent}")),
            "must mount the scratch parent so the created env persists; got: {joined}"
        );
        assert!(
            joined.contains(&format!("conda create -y -p {} --file {}", target.display(), lock.display())),
            "must be a pinned conda create from the lock; got: {joined}"
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--environment-specifier" && w[1] == "explicit"),
            "must name the explicit env format (conda 26.x dropped --file auto-detection); got: {joined}"
        );
        // Network is NOT disabled for the install step (it must fetch packages).
        assert!(!joined.contains("--network none"), "install step must reach registries; got: {joined}");
        // ...but the install of an UNTRUSTED lock is still hardened: no caps,
        // no privilege escalation, a bounded process table, and a memory cap.
        assert!(
            argv.windows(2).any(|w| w[0] == "--cap-drop" && w[1] == "ALL"),
            "install step must drop all capabilities; got: {joined}"
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--security-opt" && w[1] == "no-new-privileges"),
            "install step must forbid privilege escalation; got: {joined}"
        );
        assert!(
            argv.windows(2).any(|w| w[0] == "--pids-limit" && w[1] == "512"),
            "install step must bound its process table; got: {joined}"
        );
        assert!(
            argv.windows(2).any(|w| w[0] == "--memory" && w[1] == "4g"),
            "install step must cap memory; got: {joined}"
        );
        // The lock is mounted read-only.
        assert!(
            joined.contains(&format!("{}:{}:ro", lock.display(), lock.display())),
            "lock must be mounted read-only; got: {joined}"
        );
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
            image_probe: |_| true,
            fallback_image: None,
        };
        match provision(tmp.path(), &opts, "") {
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

        let env_obj = ExecEnv::RebuiltImage { tag: "ecaa-replay:mypkg".to_string(), conda_prefix: None, conda_mount_at: None };
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
    /// .sh → bash (spot-checked via the Container tier, whose argv ends
    /// `… <image> <interpreter> <script>` when no conda env is present).
    fn container_env() -> ExecEnv {
        ExecEnv::Container {
            digest: "sha256:img".to_string(),
            conda_prefix: None,
            conda_mount_at: None,
        }
    }

    #[test]
    fn build_command_interpreter_dispatch_by_extension() {
        let env: BTreeMap<String, String> = BTreeMap::new();
        let cwd = Path::new("/w");

        let r_argv = container_env()
            .build_command(Path::new("/w/a.R"), &env, cwd)
            .unwrap();
        assert_eq!(r_argv[r_argv.len() - 2], "Rscript");

        let py_argv = container_env()
            .build_command(Path::new("/w/b.py"), &env, cwd)
            .unwrap();
        assert_eq!(py_argv[py_argv.len() - 2], "python3");

        let sh_argv = container_env()
            .build_command(Path::new("/w/c.sh"), &env, cwd)
            .unwrap();
        assert_eq!(sh_argv[sh_argv.len() - 2], "bash");
    }
}
