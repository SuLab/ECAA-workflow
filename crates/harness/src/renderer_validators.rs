//! Renderer-output validators for the flexible plotting pipeline.
//!
//! Four `ValidatorRunner` implementations that assert publication-quality
//! properties of figures produced by a drafted renderer module:
//!
//! - `ContrastScorerRunner` — WCAG contrast score via
//!   `lib/plotting/quality_scorer.py`.
//! - `DpiLintRunner` — PNG DPI metadata must be exactly 300.
//! - `ThemeParityRunner` — PNG must use only colors from `theme.json`
//!   palette (shells to `lib/plotting/tests/lint_theme_parity.py`).
//! - `DeterminismRunner` — re-renders twice via `BubblewrapRunner` with
//!   `SandboxPolicy::default_strict()` and asserts byte-identical PNG
//!   output. Falls back to unwrapped Python when
//!   `SWFC_LOCAL_SANDBOX != bubblewrap` (with a warning log) so the
//!   runner functions in dev mode without a bubblewrap install.
//!
//! None of these runners are added to `default_runners()` by default —
//! they are opt-in for the renderer promotion gate. They complement the
//! `RENDERER_VALIDATION_BUNDLE` obligations defined in
//! `crates/core/src/validation_obligations.rs`.
//!
//! ## Sandbox usage
//!
//! `DeterminismRunner` spawns the drafted Python module twice in
//! independent `tempfile::TempDir` scratch directories. When
//! `SWFC_LOCAL_SANDBOX=bubblewrap` the spawns are wrapped in
//! `BubblewrapRunner::wrap()` with `SandboxPolicy::default_strict()`
//! (deny_network, deny_secrets, deny_host_fs). This catches drafter bugs
//! that try to fetch remote resources or read host credentials at render
//! time. When the sandbox is off the runner logs a warning but proceeds
//! unwrapped — the determinism contract is still checked; only the
//! isolation guarantee is relaxed.

use crate::sandbox_enforcer::BubblewrapRunner;
use crate::validators::{ValidatorOutcome, ValidatorRunner};
use ecaa_workflow_core::sandbox_policy::SandboxPolicy;
use std::fs;
use std::path::{Path, PathBuf};

/// Minimal PNG chunk parser — reads the `pHYs` chunk to extract DPI.
///
/// Returns `None` when the chunk is absent or the PNG is malformed.
/// The implementation is self-contained (no png/image crate dep) to
/// avoid adding a dependency to the harness for a narrow static check.
fn read_png_dpi(data: &[u8]) -> Option<u32> {
    // PNG signature: 8 bytes. pHYs chunk: `pHYs` tag at offset 4 inside
    // the chunk data, 4 bytes X density, 4 bytes Y density, 1 byte unit.
    // Unit 1 = metre. 96 DPI = 3780 pixels/metre; 300 DPI = 11811.
    if data.len() < 8 || data[..8] != [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A] {
        return None; // Not a PNG
    }
    let mut pos = 8usize;
    while pos + 12 <= data.len() {
        let chunk_len = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        let tag = &data[pos + 4..pos + 8];
        if tag == b"pHYs" && pos + 8 + chunk_len <= data.len() {
            // pHYs: 4 bytes X, 4 bytes Y, 1 byte unit (1 = metre)
            if chunk_len >= 9 {
                let x = u32::from_be_bytes(data[pos + 8..pos + 12].try_into().ok()?);
                let unit = data[pos + 16];
                if unit == 1 {
                    // pixels/metre → DPI: dpi = px_per_m * 0.0254
                    let dpi = (x as f64 * 0.0254).round() as u32;
                    return Some(dpi);
                }
                return Some(x); // unit 0 = unknown; return raw value
            }
        }
        // Skip to next chunk: 4 len + 4 tag + chunk_len + 4 CRC
        pos += 12 + chunk_len;
    }
    None
}

/// Validator that asserts the rendered PNG has a WCAG contrast score
/// meeting the configured minimum threshold.
///
/// Shells to `lib/plotting/quality_scorer.py` passing the PNG path.
/// The script exits 0 with a JSON line `{"contrast_score": <f64>}` on
/// stdout; exit non-zero or missing field → `ValidatorOutcome::Failed`.
///
/// Threshold: 4.5 (WCAG AA). Configurable via a constructor argument.
pub struct ContrastScorerRunner {
    /// Minimum acceptable WCAG contrast ratio (default 4.5 = AA).
    pub min_contrast: f64,
    /// Path to `lib/plotting/quality_scorer.py` relative to the harness
    /// CWD. Defaults to `lib/plotting/quality_scorer.py`.
    pub scorer_script: String,
}

impl Default for ContrastScorerRunner {
    fn default() -> Self {
        Self {
            min_contrast: 4.5,
            scorer_script: "lib/plotting/quality_scorer.py".into(),
        }
    }
}

impl ValidatorRunner for ContrastScorerRunner {
    fn obligation_id(&self) -> &'static str {
        "renderer_contrast_wcag"
    }

    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        // Look for a PNG file in the artifact directory. Use the first
        // `*.png` found alphabetically so the check is deterministic.
        let png_path = match find_first_png(artifact_path) {
            Some(p) => p,
            None => {
                return ValidatorOutcome::Errored {
                    reason: format!(
                        "no PNG file found in artifact directory {}",
                        artifact_path.display()
                    ),
                }
            }
        };

        let output = match std::process::Command::new("python")
            .arg(&self.scorer_script)
            .arg("--png")
            .arg(&png_path)
            .arg("--output-json")
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                return ValidatorOutcome::Errored {
                    reason: format!("failed to invoke quality_scorer.py: {}", e),
                }
            }
        };

        if !output.status.success() {
            return ValidatorOutcome::Errored {
                reason: format!(
                    "quality_scorer.py exited non-zero: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            };
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let score: f64 = match parse_json_f64_field(&stdout, "contrast_score") {
            Some(s) => s,
            None => {
                return ValidatorOutcome::Errored {
                    reason: format!(
                        "quality_scorer.py output missing contrast_score field: {}",
                        stdout
                    ),
                }
            }
        };

        if score >= self.min_contrast {
            ValidatorOutcome::Passed
        } else {
            ValidatorOutcome::Failed {
                message: format!(
                    "WCAG contrast score {:.2} below minimum {:.2} (WCAG AA = 4.5)",
                    score, self.min_contrast
                ),
            }
        }
    }
}

/// Validator that asserts the rendered PNG has exactly 300 DPI in its
/// `pHYs` metadata chunk.
///
/// Pure Rust — no external process. Reads the `pHYs` chunk from the
/// PNG binary directly.
pub struct DpiLintRunner;

impl ValidatorRunner for DpiLintRunner {
    fn obligation_id(&self) -> &'static str {
        "renderer_dpi_300"
    }

    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        let png_path = match find_first_png(artifact_path) {
            Some(p) => p,
            None => {
                return ValidatorOutcome::Errored {
                    reason: format!(
                        "no PNG file found in artifact directory {}",
                        artifact_path.display()
                    ),
                }
            }
        };

        let data = match std::fs::read(&png_path) {
            Ok(d) => d,
            Err(e) => {
                return ValidatorOutcome::Errored {
                    reason: format!("read error at {}: {}", png_path.display(), e),
                }
            }
        };

        match read_png_dpi(&data) {
            Some(300) => ValidatorOutcome::Passed,
            Some(dpi) => ValidatorOutcome::Failed {
                message: format!("PNG DPI is {} but expected 300", dpi),
            },
            None => ValidatorOutcome::Errored {
                reason: format!("pHYs chunk missing or unreadable in {}", png_path.display()),
            },
        }
    }
}

/// Validator that asserts the rendered PNG used only colors from the
/// `theme.json` palette.
///
/// Shells to `lib/plotting/tests/lint_theme_parity.py` which reads the
/// PNG pixel data and checks each non-background color against the
/// theme's Wong/Glasbey palette. Exit 0 = pass; exit non-zero = fail
/// With a JSON line `{"off_palette_colors": ["#rrggbb",...]}`.
pub struct ThemeParityRunner {
    /// Path to `lib/plotting/tests/lint_theme_parity.py`.
    pub lint_script: String,
    /// Path to `lib/plotting/theme.json`. Defaults to
    /// `lib/plotting/theme.json`.
    pub theme_path: String,
}

impl Default for ThemeParityRunner {
    fn default() -> Self {
        Self {
            lint_script: "lib/plotting/tests/lint_theme_parity.py".into(),
            theme_path: "lib/plotting/theme.json".into(),
        }
    }
}

impl ValidatorRunner for ThemeParityRunner {
    fn obligation_id(&self) -> &'static str {
        "renderer_theme_parity"
    }

    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        let png_path = match find_first_png(artifact_path) {
            Some(p) => p,
            None => {
                return ValidatorOutcome::Errored {
                    reason: format!(
                        "no PNG file found in artifact directory {}",
                        artifact_path.display()
                    ),
                }
            }
        };

        let output = match std::process::Command::new("python")
            .arg(&self.lint_script)
            .arg("--png")
            .arg(&png_path)
            .arg("--theme")
            .arg(&self.theme_path)
            .arg("--output-json")
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                return ValidatorOutcome::Errored {
                    reason: format!("failed to invoke lint_theme_parity.py: {}", e),
                }
            }
        };

        if output.status.success() {
            ValidatorOutcome::Passed
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            ValidatorOutcome::Failed {
                message: format!(
                    "theme parity check failed: {} {}",
                    stderr.trim(),
                    stdout.trim()
                ),
            }
        }
    }
}

/// Validator that asserts the drafted Python renderer module produces
/// byte-identical PNG output across two independent re-renders.
///
/// The runner is parameterised with `stage_id` (used to locate the
/// drafted module at `lib/plotting/stages/_generated/<stage_id>.py`)
/// and `figure_ids` (names of the render functions to invoke). The
/// `run` method ignores the `artifact_path` argument received from the
/// trait harness — it allocates two fresh `tempfile::TempDir` scratch
/// directories, spawns Python twice, and byte-diffs the produced PNGs.
///
/// ## Sandbox
///
/// When `SWFC_LOCAL_SANDBOX=bubblewrap` both spawns are wrapped in
/// `BubblewrapRunner::wrap()` with `SandboxPolicy::default_strict()`
/// (deny_network=true, deny_secrets=true, deny_host_fs=true). This
/// catches renderer bugs that attempt to fetch remote resources or
/// read host credentials at render time.
///
/// When the sandbox is off (the default) the runner logs a warning to
/// stderr and spawns Python unwrapped. The byte-diff determinism check
/// still runs — only the isolation guarantee is reduced.
///
/// ## Phase C8
///
/// This is the real implementation. `ValidatorOutcome::Unimplemented`
/// is no longer returned. The promotion gate in
/// `crates/core/src/plot_affordance/promotion.rs` now treats the
/// determinism obligation as pass-required (the `Unimplemented`
/// advisory path has been removed from `ValidationOutcome::is_passing`).
pub struct DeterminismRunner {
    /// Stage id used to locate the drafted module. The module is
    /// expected at `<runtime_root>/lib/plotting/stages/_generated/<stage_id>.py`
    /// where `runtime_root` is derived from the `artifact_path` passed
    /// to `ValidatorRunner::run`. Callers may also construct with an
    /// explicit `module_path_override` for testing.
    pub stage_id: String,
    /// Expected figure function names. The first entry is rendered;
    /// the rest are ignored (one function is sufficient for the
    /// byte-diff determinism check).
    pub figure_ids: Vec<String>,
    /// When `Some`, overrides the conventional module path derived
    /// from `stage_id` and the artifact path. Used in tests to
    /// point at a synthetic Python module without needing a real
    /// `lib/plotting/stages/_generated/` layout.
    pub module_path_override: Option<PathBuf>,
    /// Overrides the runtime root used to build the conventional
    /// module path. When `None`, `run()` uses `artifact_path` directly
    /// as the root (callers that set up a full package layout can
    /// leave this `None`). Integration tests set this to the tmpdir.
    pub runtime_root_override: Option<PathBuf>,
}

impl DeterminismRunner {
    /// Construct a runner for `stage_id` with the given figure ids.
    pub fn new(stage_id: impl Into<String>, figure_ids: Vec<String>) -> Self {
        Self {
            stage_id: stage_id.into(),
            figure_ids,
            module_path_override: None,
            runtime_root_override: None,
        }
    }
}

impl ValidatorRunner for DeterminismRunner {
    fn obligation_id(&self) -> &'static str {
        "renderer_determinism"
    }

    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        // 1. Locate the drafted module.
        let module_path: PathBuf = if let Some(ref p) = self.module_path_override {
            p.clone()
        } else {
            let root = self
                .runtime_root_override
                .as_deref()
                .unwrap_or(artifact_path);
            root.join(format!(
                "lib/plotting/stages/_generated/{}.py",
                self.stage_id
            ))
        };

        if !module_path.exists() {
            return ValidatorOutcome::Errored {
                reason: format!("drafted module not found at {}", module_path.display()),
            };
        }

        // 2. The first figure_id drives the render. No figure_ids = error.
        let figure_id = match self.figure_ids.first() {
            Some(id) => id.clone(),
            None => {
                return ValidatorOutcome::Errored {
                    reason: "no figure_ids declared for DeterminismRunner".into(),
                };
            }
        };
        // The figure_id is interpolated raw
        // into the python script body. Without an allowlist a hostile
        // figure_id (e.g. `foo); import os; os.system('x'); m.bar(`)
        // becomes live Python. Refuse anything outside the Python
        // identifier shape before composing the script.
        if let Err(reason) = validate_figure_id(&figure_id) {
            return ValidatorOutcome::Errored { reason };
        }

        // 3. Two independent scratch directories.
        let tmp_a = match tempfile::tempdir() {
            Ok(d) => d,
            Err(e) => {
                return ValidatorOutcome::Errored {
                    reason: format!("could not allocate scratch dir A: {}", e),
                };
            }
        };
        let tmp_b = match tempfile::tempdir() {
            Ok(d) => d,
            Err(e) => {
                return ValidatorOutcome::Errored {
                    reason: format!("could not allocate scratch dir B: {}", e),
                };
            }
        };

        // 4. Determine sandbox mode.
        //
        // `BubblewrapRunner::from_env` returns:
        // Ok(None) — SWFC_LOCAL_SANDBOX=off or unset
        // Ok(Some(r)) — bubblewrap mode and bwrap binary present
        // Err(_) — bubblewrap mode but bwrap missing (hard error)
        //
        // For the determinism runner we treat a hard bwrap error as a
        // warning + fallback rather than a complete failure — the user
        // opted into the sandbox on this host but the runner shouldn't
        // prevent the determinism check from running entirely. The
        // distinction between "bwrap missing" and "policy invalid" is
        // logged so operators know their sandbox is not active.
        let bwrap_runner = match BubblewrapRunner::from_env(artifact_path.to_path_buf()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "[determinism_runner] WARNING: bubblewrap requested but unavailable; \
                     falling back to unwrapped python. Reason: {}",
                    e
                );
                None
            }
        };

        if bwrap_runner.is_none() {
            eprintln!(
                "[determinism_runner] WARNING: SWFC_LOCAL_SANDBOX != bubblewrap; \
                 determinism re-renders will run without sandbox isolation"
            );
        }

        let policy = SandboxPolicy::default_strict();

        // 5. Helper: spawn python for one render pass.
        //    The drafted module exposes one function per figure_id;
        //    we invoke it with `out_dir=<scratch_path>` as a keyword arg.
        //
        //    Path interpolation is via env vars rather than raw-string
        //    Python literals (`r'{module}'`). Raw strings still terminate
        //    on a bare apostrophe, so any path containing `'` would
        //    break out of the string context — at minimum a render
        //    failure, at worst expression injection if the path is
        //    attacker-controlled. Passing through `os.environ[...]`
        //    avoids the quoting surface entirely.
        //
        //    The python body is written to a NamedTempFile and passed
        //    as a positional argv to `python <path>` instead of `python
        //    -c <body>`. `python -c` would still keep the source inline
        //    on the process command line where it's visible in `ps`
        //    output (and any `figure_id` slip that bypassed the
        //    identifier check above would leak into argv); the
        //    tempfile path keeps the script body off argv entirely.
        let render_one = |out_dir: &Path| -> Result<PathBuf, String> {
            // figure_id becomes an attribute name (`m.<fig>(...)`).
            // Refuse anything outside a Python-identifier shape so a
            // hostile id like `eval('rm');foo` can't run via
            // attribute lookup.
            if !figure_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
                || figure_id.is_empty()
                || figure_id.chars().next().is_some_and(|c| c.is_ascii_digit())
            {
                return Err(format!(
                    "refusing render with non-identifier figure_id: {figure_id:?}"
                ));
            }
            let python_body = format!(
                "import importlib.util, os, sys\n\
                 _module = os.environ['SWFC_RENDERER_MODULE']\n\
                 _out = os.environ['SWFC_RENDERER_OUT_DIR']\n\
                 spec = importlib.util.spec_from_file_location('_m', _module)\n\
                 m = importlib.util.module_from_spec(spec)\n\
                 spec.loader.exec_module(m)\n\
                 m.{figure_id}(out_dir=_out)\n"
            );

            // Stage the body to a tempfile under `out_dir` so it shares
            // the scratch lifetime — bwrap binds out_dir RW, so a
            // script inside it is readable by the sandboxed python
            // without an extra `--ro-bind` directive. Using a stable
            // basename keeps the determinism contract intact: two
            // passes write byte-identical script bodies to the same
            // basename in independent tempdirs.
            let script_path = out_dir.join("_render_entry.py");
            std::fs::write(&script_path, &python_body)
                .map_err(|e| format!("could not stage render script: {}", e))?;

            let module_str = module_path.to_string_lossy().into_owned();
            let out_str = out_dir.to_string_lossy().into_owned();
            let script_str = script_path.to_string_lossy().into_owned();
            // bwrap's default_strict policy `--unsetenv`s any var
            // outside `allow_envs` (PATH/LANG/LC_ALL/TZ). Extend the
            // allowlist with our two passthrough names so the Python
            // body's `os.environ[...]` lookups succeed inside the
            // sandbox without weakening the policy for unrelated env.
            let mut bwrap_policy = policy.clone();
            if !bwrap_policy
                .allow_envs
                .iter()
                .any(|s| s == "SWFC_RENDERER_MODULE")
            {
                bwrap_policy.allow_envs.push("SWFC_RENDERER_MODULE".into());
            }
            if !bwrap_policy
                .allow_envs
                .iter()
                .any(|s| s == "SWFC_RENDERER_OUT_DIR")
            {
                bwrap_policy.allow_envs.push("SWFC_RENDERER_OUT_DIR".into());
            }
            let mut cmd: std::process::Command = if let Some(ref runner) = bwrap_runner {
                let mut c = runner.wrap("python", &[script_str.as_str()], &bwrap_policy);
                c.env("SWFC_RENDERER_MODULE", &module_str);
                c.env("SWFC_RENDERER_OUT_DIR", &out_str);
                c
            } else {
                let mut c = std::process::Command::new("python");
                c.arg(&script_str);
                c.env("SWFC_RENDERER_MODULE", &module_str);
                c.env("SWFC_RENDERER_OUT_DIR", &out_str);
                c
            };

            let output = cmd
                .output()
                .map_err(|e| format!("python spawn failed: {}", e))?;

            if !output.status.success() {
                return Err(format!(
                    "python render exited non-zero ({}): {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }

            // Locate the produced PNG (first alphabetically).
            find_first_png(out_dir)
                .ok_or_else(|| format!("no PNG produced in scratch dir {}", out_dir.display()))
        };

        // 6. Two renders.
        let png_a = match render_one(tmp_a.path()) {
            Ok(p) => p,
            Err(e) => {
                return ValidatorOutcome::Errored {
                    reason: format!("render pass A failed: {}", e),
                };
            }
        };
        let png_b = match render_one(tmp_b.path()) {
            Ok(p) => p,
            Err(e) => {
                return ValidatorOutcome::Errored {
                    reason: format!("render pass B failed: {}", e),
                };
            }
        };

        // 7. Byte-diff.
        let bytes_a = match fs::read(&png_a) {
            Ok(b) => b,
            Err(e) => {
                return ValidatorOutcome::Errored {
                    reason: format!("read PNG A failed ({}): {}", png_a.display(), e),
                };
            }
        };
        let bytes_b = match fs::read(&png_b) {
            Ok(b) => b,
            Err(e) => {
                return ValidatorOutcome::Errored {
                    reason: format!("read PNG B failed ({}): {}", png_b.display(), e),
                };
            }
        };

        if bytes_a == bytes_b {
            ValidatorOutcome::Passed
        } else {
            let first_diff = bytes_a
                .iter()
                .zip(&bytes_b)
                .position(|(x, y)| x != y)
                .unwrap_or(0);
            ValidatorOutcome::Failed {
                message: format!(
                    "PNG byte-diff: a={} bytes, b={} bytes, first divergence at byte {}",
                    bytes_a.len(),
                    bytes_b.len(),
                    first_diff,
                ),
            }
        }
    }
}

/// Allowlist validator for `figure_id` values
/// that flow into the python render script in `DeterminismRunner`.
///
/// The Python script reads `m.{figure_id}(out_dir=...)` — a hostile
/// figure id like `foo); import os; os.system('x'); m.bar(` would
/// break out of the call and run arbitrary Python. Allow only the
/// strict Python-identifier shape: `[A-Za-z_][A-Za-z0-9_]*`.
fn validate_figure_id(s: &str) -> Result<&str, String> {
    if s.is_empty() {
        return Err("figure_id empty".into());
    }
    // `unwrap` is safe — we just checked `is_empty`.
    let first = s.chars().next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(format!("figure_id must start with [A-Za-z_]: {s:?}"));
    }
    if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!("figure_id may only contain [A-Za-z0-9_]: {s:?}"));
    }
    Ok(s)
}

/// Find the first `*.png` file (alphabetical) in `dir`.
fn find_first_png(dir: &Path) -> Option<std::path::PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext.eq_ignore_ascii_case("png"))
                .unwrap_or(false)
        })
        .map(|e| e.path())
        .collect();
    entries.sort();
    entries.into_iter().next()
}

/// Parse a single float field from a JSON line.
/// `{"key": <f64>}` → `Some(f64)`
fn parse_json_f64_field(s: &str, key: &str) -> Option<f64> {
    let v: serde_json::Value = serde_json::from_str(s.trim()).ok()?;
    v.get(key)?.as_f64()
}

/// Construct a `Vec<Box<dyn ValidatorRunner>>` containing the three
/// runners that don't require stage-specific configuration.
///
/// `DeterminismRunner` is deliberately excluded here because it requires
/// `stage_id` + `figure_ids` that are only known per-proposal. Callers
/// building a full renderer validation bundle should construct a
/// `DeterminismRunner::new(stage_id, figure_ids)` and push it themselves.
///
/// These are NOT added to `default_runners()` to keep the default set
/// free of Python subprocess dependencies for non-renderer tasks.
pub fn renderer_runners() -> Vec<Box<dyn ValidatorRunner>> {
    vec![
        Box::new(ContrastScorerRunner::default()) as Box<dyn ValidatorRunner>,
        Box::new(DpiLintRunner) as Box<dyn ValidatorRunner>,
        Box::new(ThemeParityRunner::default()) as Box<dyn ValidatorRunner>,
    ]
}

/// Construct a full renderer validation bundle including
/// `DeterminismRunner`. Use this when `stage_id` and `figure_ids` are
/// available at call site.
pub fn renderer_runners_with_determinism(
    stage_id: impl Into<String>,
    figure_ids: Vec<String>,
) -> Vec<Box<dyn ValidatorRunner>> {
    let mut runners = renderer_runners();
    runners
        .push(Box::new(DeterminismRunner::new(stage_id, figure_ids)) as Box<dyn ValidatorRunner>);
    runners
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn dpi_lint_runner_obligation_id() {
        assert_eq!(DpiLintRunner.obligation_id(), "renderer_dpi_300");
    }

    #[test]
    fn contrast_scorer_runner_obligation_id() {
        assert_eq!(
            ContrastScorerRunner::default().obligation_id(),
            "renderer_contrast_wcag"
        );
    }

    #[test]
    fn theme_parity_runner_obligation_id() {
        assert_eq!(
            ThemeParityRunner::default().obligation_id(),
            "renderer_theme_parity"
        );
    }

    #[test]
    fn determinism_runner_obligation_id() {
        assert_eq!(
            DeterminismRunner::new("test_stage", vec![]).obligation_id(),
            "renderer_determinism"
        );
    }

    #[test]
    fn determinism_runner_errors_when_module_missing() {
        let tmp = TempDir::new().unwrap();
        // Use a nonexistent stage_id; no module_path_override so the
        // runner derives a path that won't exist.
        let runner = DeterminismRunner {
            stage_id: "nonexistent_stage".into(),
            figure_ids: vec!["my_figure".into()],
            module_path_override: None,
            runtime_root_override: Some(tmp.path().to_path_buf()),
        };
        let outcome = runner.run(tmp.path());
        assert!(
            matches!(&outcome, ValidatorOutcome::Errored { reason } if reason.contains("not found")),
            "expected Errored with 'not found', got {:?}",
            outcome
        );
    }

    #[test]
    fn determinism_runner_errors_when_no_figure_ids() {
        let tmp = TempDir::new().unwrap();
        // Write a dummy Python file so the module-exists check passes.
        fs::write(tmp.path().join("dummy.py"), b"# dummy").unwrap();
        let runner = DeterminismRunner {
            stage_id: "some_stage".into(),
            figure_ids: vec![], // empty — should error
            module_path_override: Some(tmp.path().join("dummy.py")),
            runtime_root_override: None,
        };
        let outcome = runner.run(tmp.path());
        assert!(
            matches!(&outcome, ValidatorOutcome::Errored { reason } if reason.contains("no figure_ids")),
            "expected Errored with 'no figure_ids', got {:?}",
            outcome
        );
    }

    #[test]
    fn dpi_lint_runner_errors_when_no_png_present() {
        let tmp = TempDir::new().unwrap();
        let outcome = DpiLintRunner.run(tmp.path());
        assert!(
            matches!(outcome, ValidatorOutcome::Errored { .. }),
            "expected Errored, got {:?}",
            outcome
        );
    }

    #[test]
    fn contrast_scorer_runner_errors_when_no_png_present() {
        let tmp = TempDir::new().unwrap();
        let outcome = ContrastScorerRunner::default().run(tmp.path());
        assert!(
            matches!(outcome, ValidatorOutcome::Errored { .. }),
            "expected Errored on missing PNG, got {:?}",
            outcome
        );
    }

    #[test]
    fn theme_parity_runner_errors_when_no_png_present() {
        let tmp = TempDir::new().unwrap();
        let outcome = ThemeParityRunner::default().run(tmp.path());
        assert!(
            matches!(outcome, ValidatorOutcome::Errored { .. }),
            "expected Errored on missing PNG, got {:?}",
            outcome
        );
    }

    #[test]
    fn dpi_lint_runner_fails_on_png_without_phys_chunk() {
        // Write a minimal valid PNG (1x1, IHDR only, no pHYs).
        let tmp = TempDir::new().unwrap();
        // Tiny valid PNG bytes for a 1x1 white pixel, no pHYs chunk.
        let minimal_png: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG sig
            0x00, 0x00, 0x00, 0x0D, // IHDR length = 13
            0x49, 0x48, 0x44, 0x52, // "IHDR"
            0x00, 0x00, 0x00, 0x01, // width = 1
            0x00, 0x00, 0x00, 0x01, // height = 1
            0x08, 0x02, 0x00, 0x00, 0x00, // bit depth, color type, etc.
            0x90, 0x77, 0x53, 0xDE, // CRC
            0x00, 0x00, 0x00, 0x00, // IEND length = 0
            0x49, 0x45, 0x4E, 0x44, // "IEND"
            0xAE, 0x42, 0x60, 0x82, // CRC
        ];
        fs::write(tmp.path().join("output.png"), minimal_png).unwrap();
        let outcome = DpiLintRunner.run(tmp.path());
        // No pHYs chunk → Errored (no DPI metadata), not Failed.
        assert!(
            matches!(outcome, ValidatorOutcome::Errored { .. }),
            "expected Errored for missing pHYs, got {:?}",
            outcome
        );
    }

    #[test]
    fn renderer_runners_has_three_entries() {
        // renderer_runners() excludes DeterminismRunner (requires stage_id/figure_ids).
        assert_eq!(renderer_runners().len(), 3);
    }

    #[test]
    fn renderer_runners_with_determinism_has_four_entries() {
        let runners = renderer_runners_with_determinism("test_stage", vec!["fig".into()]);
        assert_eq!(runners.len(), 4);
    }

    #[test]
    fn renderer_runners_all_have_distinct_obligation_ids() {
        let runners = renderer_runners_with_determinism("test_stage", vec!["fig".into()]);
        let mut ids: Vec<&str> = runners.iter().map(|r| r.obligation_id()).collect();
        ids.sort();
        let mut deduped = ids.clone();
        deduped.dedup();
        assert_eq!(ids, deduped, "duplicate obligation ids in renderer_runners");
    }

    #[test]
    fn parse_json_f64_field_extracts_value() {
        assert_eq!(
            parse_json_f64_field(r#"{"contrast_score": 5.2}"#, "contrast_score"),
            Some(5.2)
        );
    }

    #[test]
    fn parse_json_f64_field_missing_key() {
        assert_eq!(
            parse_json_f64_field(r#"{"other": 1.0}"#, "contrast_score"),
            None
        );
    }

    #[test]
    fn read_png_dpi_returns_none_for_non_png() {
        assert_eq!(read_png_dpi(&[0x00, 0x01, 0x02, 0x03]), None);
    }

    // ── figure_id validator ────────────────────────────────────

    #[test]
    fn figure_id_rejects_python_injection() {
        assert!(validate_figure_id("foo); import os; os.system('x'); m.bar(").is_err());
        assert!(validate_figure_id("foo)\nimport os").is_err());
        assert!(validate_figure_id("foo'").is_err());
        assert!(validate_figure_id("foo\"").is_err());
        assert!(validate_figure_id("foo bar").is_err());
    }

    #[test]
    fn figure_id_rejects_leading_digit_and_dash() {
        assert!(validate_figure_id("1plot").is_err());
        assert!(validate_figure_id("-plot").is_err());
    }

    #[test]
    fn figure_id_rejects_empty() {
        assert!(validate_figure_id("").is_err());
    }

    #[test]
    fn figure_id_accepts_canonical_identifier() {
        assert!(validate_figure_id("plot_pca_scatter").is_ok());
        assert!(validate_figure_id("plot").is_ok());
        assert!(validate_figure_id("_private").is_ok());
        assert!(validate_figure_id("Plot42").is_ok());
        assert!(validate_figure_id("fig1").is_ok());
    }

    #[test]
    fn determinism_runner_errors_on_unsafe_figure_id() {
        // Plumb a hostile figure_id through the validator chain and
        // confirm the runner returns `Errored` before composing the
        // python script.
        let tmp = TempDir::new().unwrap();
        let module_path = tmp.path().join("module.py");
        fs::write(&module_path, "def plot(out_dir): pass\n").unwrap();
        let runner = DeterminismRunner {
            stage_id: "stage".into(),
            figure_ids: vec!["plot); import os; os.system('x'); m.x(".into()],
            module_path_override: Some(module_path),
            runtime_root_override: None,
        };
        match runner.run(tmp.path()) {
            ValidatorOutcome::Errored { reason } => {
                assert!(
                    reason.contains("figure_id"),
                    "expected figure_id refusal, got {reason:?}"
                );
            }
            other => panic!("expected Errored, got {other:?}"),
        }
    }
}
