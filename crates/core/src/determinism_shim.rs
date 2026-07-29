//! Grant v19 §Authentication of Key Resources + §Aim 3A Arm B′ —
//! capture the determinism-relevant environment in effect at package
//! emit time. Emitted as `runtime/determinism-shim.json` by the
//! conversation crate's `emit::sidecars::write_determinism_shim`.
//!
//! The shim records:
//! - the determinism-relevant env vars in effect for every task: the
//!   `LANG`/`PYTHONHASHSEED`/`SOURCE_DATE_EPOCH`/`TZ` policy the harness
//!   injects into EVERY per-task container, plus any of
//!   `TZ`/`LANG`/`LC_ALL`/`PYTHONHASHSEED`/`SOURCE_DATE_EPOCH` also present
//!   on the compiler host at emit time (values not captured for privacy,
//!   just presence + locale/timezone resolution). Declaring the applied
//!   policy — not just the bare compiler host — keeps this package-level
//!   disclosure CONSISTENT with the per-task
//!   `runtime/outputs/<task_id>/determinism-env.json` the agent records
//!   at runtime (LANG=C.UTF-8, PYTHONHASHSEED=0, SOURCE_DATE_EPOCH=<run
//!   epoch>), rather than contradicting it;
//! - the numerical-library thread budget ([`THREAD_BUDGET_ENV_VARS`]) the
//!   harness also injects into every per-task container. The thread count
//!   fixes the REDUCTION ORDER of every multithreaded BLAS / OpenMP kernel,
//!   so leaving it undeclared lets a re-execution on a host with a different
//!   core count drift in the low-order bits of any reduced float — exactly
//!   the class of jitter this envelope exists to eliminate;
//! - which secret env vars are present and redacted from the capture
//!   (recorded by name only — never values);
//! - the seed policy (SOURCE_DATE_EPOCH value if the host set one, else
//!   the harness-injected applied policy);
//! - the temp-path strategy + a DETERMINISTIC symbolic root. The root
//!   deliberately does NOT capture the host `$TMPDIR` (e.g.
//!   `/tmp/claude-1000`): that value is host-specific and would leak
//!   host state into the package, defeating the byte-reproducibility
//!   intent. The harness always places per-task scratch under the
//!   package-relative `runtime/scratch/<task_id>` regardless of host
//!   `$TMPDIR`, so the symbolic root is both accurate and deterministic;
//! - the active locale + timezone;
//! - the `ablation_engaged` flag mirroring
//!   [`crate::ablation::AblationFlag::ReexecutionClass`].

use crate::ablation::{AblationFlag, AblationFlagExt};
use serde::{Deserialize, Serialize};
use std::env;
use std::path::Path;

/// Top-level payload for `runtime/determinism-shim.json`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeterminismShimSidecar {
    /// Schema version.
    pub schema_version: String,
    /// Env capture.
    pub env_capture: EnvCapture,
    /// Seed policy.
    pub seed_policy: SeedPolicy,
    /// Temp path policy.
    pub temp_path_policy: TempPathPolicy,
    /// Locale.
    pub locale: String,
    /// Timezone.
    pub timezone: String,
    /// Mirrors `ECAA_ABLATE_REEXECUTION_CLASS` per Subsystem B6.
    ///
    /// When `true`, the deterministic re-execution class is suppressed on
    /// the emit side (Arm B′ control). This bool flip is retained for
    /// backwards-compatibility and historical-session readers; the load-bearing
    /// suppression that empties `per_artifact` lives in
    /// `crates/conversation::emit::sidecars::write_reexecution_sidecar`.
    pub ablation_engaged: bool,

    /// Per-artifact / per-column non-determinism acknowledgments.
    ///
    /// This is the SINGLE SOURCE that the re-execution comparator
    /// (`crate::reexecution::classify_reexecution`) AND the audit-proof
    /// equivalence-failure invariant
    /// (`crate::audit_proof::invariants::equivalence_failure`) both read: a
    /// divergence that exceeds the semantic band is only bucketed
    /// `AcknowledgedNonDeterminism` (comparator) / satisfied (invariant) when a
    /// matching ack COVERS every diverging column here — a whole-artifact ack
    /// (`columns: None`) covers everything, a column-scoped ack covers only its
    /// listed columns. An undeclared divergence FAILS.
    ///
    /// Old shims that predate this field deserialize to an empty vec via serde
    /// `default`. An empty list is `skip_serializing_if`-omitted so the
    /// compiler-host snapshot stays byte-identical to pre-field shims (the
    /// byte-reproducibility contract — `determinism-shim.json` is NOT in the
    /// byte-diff exclusion set). `serialize_active_settings` emits it empty; the
    /// emit-time projection (atom `non_determinism` decl → this field) is wired
    /// separately and should assign through
    /// [`DeterminismShimSidecar::set_non_deterministic_artifacts`] to stay
    /// byte-stable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub non_deterministic_artifacts: Vec<NonDetAck>,
}

/// One acknowledged source of non-determinism, scoped to an artifact and
/// (optionally) a set of columns within it.
///
/// The `artifact` string may be a bare filename (`de_results.tsv`) or a
/// package-relative path
/// (`runtime/outputs/differential_expression/de_results.tsv`);
/// [`ack_for`] matches either form against a re-execution outcome id.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct NonDetAck {
    /// Artifact this ack applies to (bare filename or package-relative path).
    pub artifact: String,
    /// Columns within the artifact whose divergence is acknowledged. `None`
    /// (absent) is a WHOLE-ARTIFACT ack covering every column; `Some(cols)`
    /// covers only the named columns, so a divergence in an un-listed column
    /// still fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<Vec<String>>,
    /// The class of non-determinism.
    pub kind: NonDetKind,
    /// Human-readable justification (surfaced in provenance; never parsed).
    pub reason: String,
}

/// Class of an acknowledged non-determinism source. `#[non_exhaustive]` per
/// the public-enum SemVer contract — adding a future class is a non-breaking
/// minor change for downstream RO-Crate / schema consumers.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NonDetKind {
    /// Order-dependent multithreaded reduction (e.g. thread-count-sensitive
    /// BLAS sum ordering) that changes low-order bits run-to-run.
    MultithreadedReduction,
    /// An RNG that was never seeded, so draws differ across runs.
    UnseededRng,
    /// Empirical-Bayes / adaptive shrinkage whose fitted prior shifts the
    /// shrunken estimate within the tolerance band (e.g. DESeq2/apeglm
    /// log2 fold-change shrinkage).
    AdaptiveShrinkage,
    /// Floating-point non-associativity across differing summation orders.
    FloatingPointAssociativity,
    /// Any other declared, justified source.
    Other,
}

/// Determinism-relevant env vars: presence-captured (never value-captured)
/// + secret env vars marked as "redacted".
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EnvCapture {
    /// Captured env vars.
    pub captured_env_vars: Vec<String>,
    /// Redacted env vars.
    pub redacted_env_vars: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// SeedPolicy data.
pub struct SeedPolicy {
    /// Random seed.
    pub random_seed: Option<u64>,
    /// Seed source.
    pub seed_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// TempPathPolicy data.
pub struct TempPathPolicy {
    /// Strategy.
    pub strategy: String,
    /// Root.
    pub root: String,
}

const CAPTURED_ENV_VARS: &[&str] = &[
    "TZ",
    "LANG",
    "LC_ALL",
    "PYTHONHASHSEED",
    "SOURCE_DATE_EPOCH",
];

/// BLAS / OpenMP / numerical-library thread-budget env vars the harness sets on
/// EVERY per-task dispatch (`harness::executor::hardware_envelope`, which
/// aliases this slice as its `BLAS_THREAD_ENV_KEYS` so the two can never
/// drift). Canonical, ordered, deduplicated — this module is the single source
/// of truth; core owns it because the determinism envelope must declare it and
/// core must not depend on the harness.
///
/// Each key is read by the corresponding shared library at LIBRARY-INIT time
/// (when BLAS is dlopen'd as the language runtime starts), so the harness MUST
/// set them before spawning the agent — `Sys.setenv()` in R or
/// `os.environ[...] = ...` in Python is too late once BLAS has loaded.
///
/// Coverage spans every BLAS implementation encountered in bioinformatics
/// stacks (OpenBLAS, MKL, BLIS, Apple Accelerate, reference netlib via
/// `LD_PRELOAD`) plus closely-adjacent thread pools (OpenMP, NumExpr, Numba,
/// Rayon, TBB, Julia, Polars). All keys are set to the same value
/// (`recommended_threads`) so a single-process Rscript or Python script gets
/// the full thread budget by default; for multi-worker fan-out (BiocParallel
/// `mclapply`, joblib/loky) the agent constrains per-worker BLAS at runtime via
/// `RhpcBLASctl::blas_set_num_threads(N)` inside each worker — see
/// `prompt_role.txt` "Hardware-aware execution".
///
/// # Why the determinism envelope must declare these
///
/// The thread count fixes the ORDER in which a multithreaded reduction
/// accumulates partial sums, and floating-point addition is not associative. A
/// package that records only the seed/locale policy therefore re-executes with
/// whatever thread count the REPLAY host's core count implies, and any reduced
/// float (a column mean, a variance, a `vst` dispersion fit) shifts in its
/// low-order bits — an unrecorded, unreproducible divergence rather than an
/// acknowledged one. Declaring the names here is what makes the budget part of
/// the disclosed environment; [`crate::replay::env_provision`] re-injects the
/// recorded VALUES on replay.
///
/// Order is presentation-only: every consumer either sets all keys to the same
/// value or unions them into a `BTreeSet`, so nothing depends on this sequence.
/// It is kept stable anyway to keep diffs readable.
pub const THREAD_BUDGET_ENV_VARS: &[&str] = &[
    "OMP_NUM_THREADS",
    "OPENBLAS_NUM_THREADS",
    "GOTO_NUM_THREADS",
    "MKL_NUM_THREADS",
    "BLIS_NUM_THREADS",
    "VECLIB_MAXIMUM_THREADS",
    "NUMEXPR_NUM_THREADS",
    "NUMEXPR_MAX_THREADS",
    "TBB_NUM_THREADS",
    "RAYON_NUM_THREADS",
    "NUMBA_NUM_THREADS",
    "JULIA_NUM_THREADS",
    "POLARS_MAX_THREADS",
];

/// Determinism-relevant env vars the harness ALWAYS injects into every
/// per-task execution container (`crates/harness` — LANG=C.UTF-8,
/// PYTHONHASHSEED=0, SOURCE_DATE_EPOCH=<run epoch>, TZ=UTC). The emit-time
/// shim declares this applied-policy floor as its baseline so the
/// package-level disclosure does not CONTRADICT the per-task
/// `determinism-env.json` evidence the agent records at runtime. These names
/// are always present in `captured_env_vars`; the compiler-host presence scan
/// (`CAPTURED_ENV_VARS`) can only ADD to the set.
///
/// [`THREAD_BUDGET_ENV_VARS`] is the SECOND half of the applied-policy floor
/// (`serialize_active_settings` unions both): the harness injects those
/// unconditionally too, from `apply_blas_thread_envelope`, before any
/// per-stage `compute-resource-policy.json` lookup. They are held in a separate
/// const because the harness aliases that one directly and because they form a
/// distinct class (thread budget, not seed/locale policy).
const APPLIED_POLICY_ENV_VARS: &[&str] = &["LANG", "PYTHONHASHSEED", "SOURCE_DATE_EPOCH", "TZ"];

const REDACTED_ENV_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ECAA_ANTHROPIC_API_KEY",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
];

/// Snapshot the active determinism environment. Reads from `std::env`
/// — never opens a file or makes a network call. Pure with respect to
/// its env-var inputs.
///
/// This is the COMPILER-HOST snapshot. Container-resolved determinism
/// env (what the five seed/locale vars actually were INSIDE the
/// execution container, recorded per-task by the agent wrapper as
/// `runtime/outputs/<task_id>/determinism-env.json`) is merged in at
/// finalize via [`merge_container_env`].
pub fn serialize_active_settings() -> DeterminismShimSidecar {
    // Union the applied-policy floor (what the harness injects into every
    // per-task container) with any determinism var also present on the
    // compiler host. A `BTreeSet` keeps the result deduped + sorted so the
    // capture is byte-stable regardless of host env ordering.
    //
    // The floor spans BOTH applied-policy classes. The seed/locale vars are
    // unconditional per D3; the thread-budget vars are unconditional per
    // `harness::executor::hardware_envelope::apply_blas_thread_envelope`, which
    // runs on every dispatch before any per-stage policy lookup. Declaring the
    // thread budget is what stops a replay on a differently-sized host from
    // silently changing BLAS reduction order (see `THREAD_BUDGET_ENV_VARS`).
    let mut captured: std::collections::BTreeSet<String> = APPLIED_POLICY_ENV_VARS
        .iter()
        .chain(THREAD_BUDGET_ENV_VARS.iter())
        .map(|k| (*k).to_string())
        .collect();
    captured.extend(
        CAPTURED_ENV_VARS
            .iter()
            .filter(|k| env::var(k).is_ok())
            .map(|k| (*k).to_string()),
    );
    DeterminismShimSidecar {
        schema_version: "1".into(),
        env_capture: EnvCapture {
            captured_env_vars: captured.into_iter().collect(),
            redacted_env_vars: REDACTED_ENV_VARS
                .iter()
                .filter(|k| env::var(k).is_ok())
                .map(|k| (*k).to_string())
                .collect(),
        },
        seed_policy: SeedPolicy {
            random_seed: env::var("SOURCE_DATE_EPOCH")
                .ok()
                .and_then(|v| v.parse().ok()),
            // The harness injects SOURCE_DATE_EPOCH (the run epoch) plus
            // PYTHONHASHSEED=0 into every task, so the applied seed policy is
            // deterministic even when the compiler host itself carries no
            // SOURCE_DATE_EPOCH. Record that applied policy instead of a bare
            // "process-default" that would contradict the per-task
            // determinism-env.json evidence.
            seed_source: if env::var("SOURCE_DATE_EPOCH").is_ok() {
                "SOURCE_DATE_EPOCH".into()
            } else {
                "harness-injected (SOURCE_DATE_EPOCH + PYTHONHASHSEED=0)".into()
            },
        },
        temp_path_policy: TempPathPolicy {
            strategy: "stable-by-task-id".into(),
            // DETERMINISTIC by design: the harness scratch root is the
            // package-relative `runtime/scratch/<task_id>`, never the
            // host `$TMPDIR`. Capturing the literal `$TMPDIR`
            // (e.g. `/tmp/claude-1000`) would bake host state into the
            // sidecar and break byte-reproducibility, so we record the
            // symbolic package-relative root instead.
            root: "runtime/scratch".into(),
        },
        // Fall back to the harness-applied locale (LANG=C.UTF-8) rather than a
        // bare "C" when the compiler host sets neither LC_ALL nor LANG, so the
        // declared locale matches what per-task determinism-env.json records.
        locale: env::var("LC_ALL")
            .or_else(|_| env::var("LANG"))
            .unwrap_or_else(|_| "C.UTF-8".into()),
        timezone: env::var("TZ").unwrap_or_else(|_| "UTC".into()),
        ablation_engaged: AblationFlag::ReexecutionClass.is_active(),
        // The compiler-host snapshot never declares acks — they are projected
        // from atom `non_determinism` declarations at emit time by a separate
        // pass. Empty here keeps the host snapshot byte-stable.
        non_deterministic_artifacts: Vec::new(),
    }
}

impl DeterminismShimSidecar {
    /// Assign the non-determinism acknowledgments, keeping them sorted +
    /// deduplicated for byte-stable serialization. The emit-time projection
    /// (atom `non_determinism` decl → this field) calls this so the sidecar is
    /// reproducible regardless of the order atoms were visited.
    pub fn set_non_deterministic_artifacts(&mut self, mut acks: Vec<NonDetAck>) {
        acks.sort();
        acks.dedup();
        self.non_deterministic_artifacts = acks;
    }
}

/// Return the acknowledgment that covers `artifact_path`, if the shim declares
/// one. Matching is exact on the declared `artifact` string, then by trailing
/// path segment, then by file name — so a shim that declares a bare
/// `de_results.tsv` still matches a re-execution outcome id of
/// `runtime/outputs/differential_expression/de_results.tsv`.
///
/// This is the shared lookup both the comparator and the equivalence-failure
/// invariant use, so the comparator's `AcknowledgedNonDeterminism` bucket and
/// the invariant's satisfied verdict are driven by the SAME declaration.
pub fn ack_for<'a>(shim: &'a DeterminismShimSidecar, artifact_path: &str) -> Option<&'a NonDetAck> {
    shim.non_deterministic_artifacts
        .iter()
        .find(|a| artifact_ack_matches(&a.artifact, artifact_path))
}

/// True when a shim-declared `artifact` string identifies the same artifact as
/// a re-execution outcome id (`artifact_path`).
fn artifact_ack_matches(declared: &str, artifact_path: &str) -> bool {
    if declared == artifact_path {
        return true;
    }
    // Trailing path-segment match: artifact_path == ".../<declared>".
    if let Some(prefix_len) = artifact_path.len().checked_sub(declared.len()) {
        if artifact_path.ends_with(declared) && artifact_path[..prefix_len].ends_with('/') {
            return true;
        }
    }
    // File-name match: a bare filename declared against a full relative path.
    let file_name = |p: &str| {
        std::path::Path::new(p)
            .file_name()
            .and_then(|s| s.to_str())
            .map(str::to_owned)
    };
    match (file_name(declared), file_name(artifact_path)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Fold a per-task container's captured determinism env keys into the
/// package-level (compiler-host) shim's `captured_env_vars`, as a
/// deterministic `BTreeSet` union (deduped + sorted). The host shim
/// stays a pure emit-time snapshot; this is the seam where
/// harness-written runtime evidence
/// (`runtime/outputs/<task_id>/determinism-env.json`, D4) augments it
/// at finalize. Byte-stable: identical inputs yield identical output
/// ordering.
///
/// The union is over NAMES only and is idempotent, so the applied-policy floor
/// (seed/locale + [`THREAD_BUDGET_ENV_VARS`]) can never conflict with a later
/// container fold: a name the container also reports simply collapses. That is
/// why the thread-budget vars belong in the floor rather than in the
/// compiler-host presence scan — the compiler host does not normally carry
/// `OMP_NUM_THREADS`, so a presence scan would record them approximately never,
/// while the harness injects them on every dispatch.
///
/// Note the asymmetry this leaves: the agent wrapper's per-task
/// `determinism-env.json` writes `captured_env_vars` over the five-var
/// seed/locale allowlist only, so per-task evidence under-reports the thread
/// budget relative to this floor. That is a gap in the RUNTIME capture, not a
/// contradiction in the declaration — the harness demonstrably does inject all
/// of [`THREAD_BUDGET_ENV_VARS`]. Closing it means teaching the wrapper to
/// record those keys WITH their values, which is also what
/// [`crate::replay::env_provision::apply_recorded_thread_budget`] needs in order
/// to re-inject the original budget instead of inheriting the replay host's.
pub fn merge_container_env(host: &mut DeterminismShimSidecar, container_keys: &[String]) {
    let mut set: std::collections::BTreeSet<String> =
        host.env_capture.captured_env_vars.iter().cloned().collect();
    set.extend(container_keys.iter().cloned());
    host.env_capture.captured_env_vars = set.into_iter().collect();
}

/// Package-relative path of the shim sidecar.
const SHIM_REL_PATH: &str = "runtime/determinism-shim.json";

/// Key of the emit-time declaration block that sits ALONGSIDE — never inside —
/// the load-bearing [`DeterminismShimSidecar::non_deterministic_artifacts`]
/// mask. Written by `conversation::emit::sidecars::write_determinism_shim`.
const DECLARED_BLOCK_KEY: &str = "declared_non_determinism";

/// One empirical-Bayes / adaptive-shrinkage engine, paired with the call sites
/// that prove it was USED rather than merely installed.
struct ShrinkageEngine {
    /// Upstream package identity, as [`normalized_package_name`] renders a
    /// recorded `language_packages_installed[].name`. Matching post-normalization
    /// means a conda feedstock (`bioconductor-apeglm`) or a channel-qualified
    /// spec (`bioconda::apeglm`) hits the same entry.
    package: &'static str,
    /// Entry points whose CALL-SHAPED occurrence in the stage's retained
    /// scripts is direct evidence the engine ran. Data-driven per engine, not a
    /// hardcoded R idiom: a future engine in another language declares its own
    /// tokens here (e.g. a Python estimator's `fit_shrinkage`) and the scanner
    /// needs no change.
    entry_points: &'static [&'static str],
}

/// Engines that can satisfy an [`NonDetKind::AdaptiveShrinkage`] declaration.
///
/// `lfcShrink` is the shared DESeq2 front door for both engines (selected via
/// its `type=` argument); the bare estimator names cover a script that calls
/// the engine directly (`apeglm(...)`, `ashr::ash(...)`).
const SHRINKAGE_ENGINES: &[ShrinkageEngine] = &[
    ShrinkageEngine {
        package: "apeglm",
        entry_points: &["lfcShrink", "apeglm"],
    },
    ShrinkageEngine {
        package: "ashr",
        entry_points: &["lfcShrink", "ash"],
    },
];

/// Directory, inside a stage's output dir, where the agent retains every script
/// it authored for that task (the emitter's `scripts/` contract). This — not a
/// log, not the narrative — is where invocation evidence is read from.
const RETAINED_SCRIPTS_DIR: &str = "scripts";

/// Extension of files inside `scripts/` that are TRANSCRIPTS, not code. A log
/// can echo a command line that errored out or was never reached, so it is
/// never treated as evidence the call ran.
const TRANSCRIPT_EXT: &str = "log";

/// Depth bound on the walk under `scripts/`. Retained scripts are flat in
/// practice; the bound keeps a pathological tree from costing an unbounded
/// walk while still finding a script an agent filed one level down.
const MAX_SCRIPT_WALK_DEPTH: usize = 3;

/// Outcome of reconciling ONE declaration against its stage's run evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclarationVerdict {
    /// The run evidence positively exhibits the declared mechanism. The
    /// declaration is promoted into the mask — the exemption is EARNED.
    Confirmed,
    /// The evidence was readable and does NOT exhibit the declared mechanism.
    /// No mask; the entry stays as a record that execution contradicted the
    /// static claim.
    Refuted,
    /// No verdict was possible — evidence missing/unreadable, or no positive
    /// predicate exists for the declared kind. Fail-closed: no mask.
    Declared,
}

impl DeclarationVerdict {
    /// Wire value written back into the declaration's `status` field.
    fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Refuted => "refuted",
            Self::Declared => "declared",
        }
    }
}

/// Reconcile the emit-time `declared_non_determinism` block in
/// `runtime/determinism-shim.json` against each stage's recorded run evidence,
/// promoting only the declarations the run actually confirms into the
/// authoritative [`DeterminismShimSidecar::non_deterministic_artifacts`] mask.
/// Returns the number of CONFIRMED declarations.
///
/// # Why this exists
///
/// The declarations are projected at EMIT from an atom's static
/// `non_determinism` YAML — before any task ran. The mask, by contrast, is what
/// [`ack_for`] serves to [`crate::reexecution::classify_reexecution`] (which
/// downgrades a real divergence to `AcknowledgedNonDeterminism`) and to
/// `crate::audit_proof::invariants::equivalence_failure` (which treats the
/// divergence as satisfied). Writing a static declaration straight into the
/// mask therefore exempts an artifact from equivalence checking on no evidence
/// — even when the executed script never used the declared mechanism. This
/// function is the seam that turns a claim into an observation.
///
/// # Confirmation predicate: TWO signals, both required
///
/// Modality-agnostic and evidence-based. A declaration is `confirmed` only when
/// the stage's own recorded run evidence carries BOTH:
///
/// **(a) Availability** — the mechanism's engine appears in the stage
/// `result.json`'s `language_packages_installed[].name` list, named by the
/// declaration's own `confirmation_evidence` pointer. Names are normalized
/// (lowercased, channel prefix `<channel>::` dropped, conda-feedstock prefixes
/// `bioconductor-`/`r-`/`python-` stripped) before matching.
///
/// **(b) Use** — a call-shaped occurrence of one of that engine's entry points
/// (see [`SHRINKAGE_ENGINES`]) in the stage's own retained scripts, i.e. under
/// `<stage output dir>/scripts/`, the same directory the evidence pointer
/// resolves into.
///
/// ## Why availability alone is insufficient
///
/// `language_packages_installed` proves the engine was AVAILABLE to the stage,
/// never that the stage USED it. A base image that happens to bundle `apeglm`,
/// or an install step that pulls it in as a transitive dependency of something
/// else, would make signal (a) unconditionally true — and every DE stage would
/// then silently earn an exemption from re-execution equivalence checking on a
/// mechanism it never invoked. Since the mask this function writes is exactly
/// what suppresses a real divergence, an install-only predicate re-opens the
/// hole this seam exists to close. Requiring direct evidence of the CALL keeps
/// the exemption tied to an observation of the run.
///
/// Signal (b) is read only from `scripts/`, never from `*.log`: a transcript
/// can echo a command that errored out or was never reached. Occurrences inside
/// a comment (`#`, the marker in R/Python/shell) are skipped, so a
/// commented-out call is not evidence.
///
/// | [`NonDetKind`] | Verdict rule |
/// |---|---|
/// | `AdaptiveShrinkage` | `confirmed` iff (a) a shrinkage engine (`apeglm` or `ashr`) is in the installed set AND (b) one of its entry points is called in the retained scripts; (a) without (b) — available but unused — is `refuted`, as is (a) failing |
/// | `MultithreadedReduction`, `UnseededRng`, `FloatingPointAssociativity`, `Other` | no positive two-signal predicate exists → left `declared`, NO mask |
/// | any future variant (`NonDetKind` is `#[non_exhaustive]`) | left `declared`, NO mask |
///
/// Fail-closed in every ambiguous case: a missing/unreadable/non-JSON evidence
/// file, an absent or unparseable `kind`, an evidence pointer that escapes the
/// package root, or a declaration with no `artifact` all leave the entry
/// `declared` and grant NO mask. A kind is never confirmed by default.
///
/// # Effects
///
/// Rewrites the sidecar atomically (tmp + rename via
/// [`crate::fs_helpers::atomic_write_bytes_sync`]), preserving every unrelated
/// JSON field, setting each declaration's `status`, and flipping the block's
/// own `status` to `reconciled`. Idempotent — a second call over the same
/// evidence produces byte-identical output. A package with no sidecar, a
/// non-object sidecar, or no declaration block is a no-op returning `Ok(0)`.
///
/// `runtime/determinism-shim.json` is EXCLUDED from the BagIt payload manifest
/// (`emitter::bagit`), so this rewrite needs no manifest reseal.
pub fn reconcile_declared_non_determinism(package_root: &Path) -> std::io::Result<usize> {
    let path = package_root.join(SHIM_REL_PATH);
    // A package with no shim (legacy or pre-emit) has nothing to reconcile.
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(0);
    };
    let mut root: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
    let Some(obj) = root.as_object_mut() else {
        return Ok(0);
    };
    let Some(mut block) = obj.remove(DECLARED_BLOCK_KEY) else {
        return Ok(0);
    };
    if !block.is_object() {
        // Not the shape we write; put it back untouched rather than dropping a
        // field we do not own.
        obj.insert(DECLARED_BLOCK_KEY.to_string(), block);
        return Ok(0);
    }

    let mut confirmed: Vec<NonDetAck> = Vec::new();
    if let Some(block_obj) = block.as_object_mut() {
        if let Some(declarations) = block_obj
            .get_mut("declarations")
            .and_then(serde_json::Value::as_array_mut)
        {
            for entry in declarations.iter_mut() {
                let Some(entry_obj) = entry.as_object_mut() else {
                    continue;
                };
                let (verdict, ack) = reconcile_one(package_root, entry_obj);
                if let Some(ack) = ack {
                    confirmed.push(ack);
                }
                entry_obj.insert(
                    "status".to_string(),
                    serde_json::Value::String(verdict.as_str().to_string()),
                );
            }
        }
        block_obj.insert(
            "status".to_string(),
            serde_json::Value::String("reconciled".to_string()),
        );
    }

    // Promote through the canonical setter so the mask stays sorted + deduped
    // (byte-stability contract), and so re-running the reconciliation over an
    // already-promoted shim is a no-op rather than a duplicate.
    let mut shim: DeterminismShimSidecar =
        serde_json::from_value(serde_json::Value::Object(obj.clone()))
            .map_err(std::io::Error::other)?;
    let mut acks = std::mem::take(&mut shim.non_deterministic_artifacts);
    acks.extend(confirmed.iter().cloned());
    shim.set_non_deterministic_artifacts(acks);

    let shim_value = serde_json::to_value(&shim).map_err(std::io::Error::other)?;
    // `non_deterministic_artifacts` is `skip_serializing_if = Vec::is_empty`,
    // so an empty mask omits the key entirely. Drop any prior copy first: a
    // merge alone could otherwise leave an UNEARNED mask behind.
    obj.remove("non_deterministic_artifacts");
    if let Some(fields) = shim_value.as_object() {
        for (key, value) in fields {
            obj.insert(key.clone(), value.clone());
        }
    }
    obj.insert(DECLARED_BLOCK_KEY.to_string(), block);

    let body = serde_json::to_vec_pretty(&root).map_err(std::io::Error::other)?;
    crate::fs_helpers::atomic_write_bytes_sync(&path, &body)?;
    Ok(confirmed.len())
}

/// Reconcile a single declaration entry. Returns its verdict plus the mask
/// entry it earned (`Some` only on [`DeclarationVerdict::Confirmed`]).
fn reconcile_one(
    package_root: &Path,
    entry: &serde_json::Map<String, serde_json::Value>,
) -> (DeclarationVerdict, Option<NonDetAck>) {
    // An absent or unparseable kind (including a variant this build does not
    // know) is not confirmable.
    let Some(kind) = entry
        .get("kind")
        .and_then(|k| serde_json::from_value::<NonDetKind>(k.clone()).ok())
    else {
        return (DeclarationVerdict::Declared, None);
    };
    let Some(evidence_rel) = entry
        .get("confirmation_evidence")
        .and_then(serde_json::Value::as_str)
    else {
        return (DeclarationVerdict::Declared, None);
    };
    // The pointer is data read out of a file: refuse anything that could
    // resolve outside the package root.
    let rel = Path::new(evidence_rel);
    if evidence_rel.is_empty()
        || rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return (DeclarationVerdict::Declared, None);
    }
    let evidence_path = package_root.join(rel);
    let Ok(text) = std::fs::read_to_string(&evidence_path) else {
        return (DeclarationVerdict::Declared, None);
    };
    let Ok(evidence) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (DeclarationVerdict::Declared, None);
    };

    match kind {
        NonDetKind::AdaptiveShrinkage => {
            // INVOCATION IS THE PRIMARY SIGNAL. The install set is only
            // corroborating, because it is unreliable in BOTH directions:
            //   * available-but-unused would grant an unearned exemption
            //     (a base image that bundles apeglm makes it unconditional);
            //   * used-but-not-recorded is real — an observed run called
            //     `lfcShrink(type="apeglm")` and recorded the choice while
            //     `language_packages_installed` listed only DESeq2, because
            //     the engine came from the image rather than a stage install.
            //     `lfcShrink(type="normal")` is DESeq2's own shrinkage and can
            //     never appear in an install set at all.
            // So: a call-shaped, non-comment invocation in the stage's own
            // retained scripts confirms. The scripts dir is derived from the
            // (already path-jailed) evidence pointer's own directory, so both
            // signals are read from the same stage and no second untrusted
            // path string enters the lookup.
            let scripts_dir = evidence_path
                .parent()
                .unwrap_or(package_root)
                .join(RETAINED_SCRIPTS_DIR);
            let all_entry_points: Vec<&str> = SHRINKAGE_ENGINES
                .iter()
                .flat_map(|e| e.entry_points.iter().copied())
                .collect();
            if !scripts_invoke_any(&scripts_dir, &all_entry_points) {
                // No invocation in the stage's own scripts: whether or not an
                // engine was installed, nothing shrank. An unearned exemption
                // is exactly the failure mode this predicate exists to
                // prevent, so refute.
                return (DeclarationVerdict::Refuted, None);
            }
            // Corroboration only, and deliberately non-blocking: record when
            // the install set agrees, but never let its absence override a
            // real observed call (see the `type="normal"` case above).
            let installed = installed_package_names(&evidence);
            let corroborated = SHRINKAGE_ENGINES
                .iter()
                .any(|e| installed.iter().any(|name| name.as_str() == e.package));
            if !corroborated {
                tracing::debug!(
                    target: "determinism-shim",
                    "shrinkage invocation confirmed from retained scripts; no engine in \
                     language_packages_installed (image-provided engine or DESeq2-native \
                     shrinkage)"
                );
            }
            match ack_from_declaration(entry, kind) {
                Some(ack) => (DeclarationVerdict::Confirmed, Some(ack)),
                // A declaration naming no artifact cannot scope a mask entry.
                None => (DeclarationVerdict::Declared, None),
            }
        }
        // No positive, evidence-based two-signal predicate exists for the
        // remaining kinds — and `NonDetKind` is `#[non_exhaustive]`, so future
        // variants land here too. Fail closed rather than granting a mask on a
        // kind whose confirmation rule has not been written.
        _ => (DeclarationVerdict::Declared, None),
    }
}

/// Build the mask entry a confirmed declaration earns. `None` when the entry
/// names no artifact (nothing to scope the exemption to).
fn ack_from_declaration(
    entry: &serde_json::Map<String, serde_json::Value>,
    kind: NonDetKind,
) -> Option<NonDetAck> {
    let artifact = entry
        .get("artifact")
        .and_then(serde_json::Value::as_str)
        .filter(|a| !a.is_empty())?;
    let columns = entry
        .get("columns")
        .and_then(serde_json::Value::as_array)
        .map(|cols| {
            cols.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<String>>()
        });
    let reason = entry
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    Some(NonDetAck {
        artifact: artifact.to_string(),
        columns,
        kind,
        reason,
    })
}

/// Normalized `language_packages_installed[].name` values from a stage
/// `result.json`. Empty when the stage recorded no install set — which is
/// itself a fail-closed input (nothing can be confirmed against it).
fn installed_package_names(evidence: &serde_json::Value) -> Vec<String> {
    let Some(list) = evidence
        .get("language_packages_installed")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|pkg| pkg.get("name").and_then(serde_json::Value::as_str))
        .map(normalized_package_name)
        .collect()
}

/// Reduce a recorded package name to its upstream identity so the predicate
/// matches whichever packaging surface the run happened to install from:
/// lowercase, drop a `<channel>::` qualifier, strip a conda-feedstock prefix.
fn normalized_package_name(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    let bare = match lower.rsplit_once("::") {
        Some((_, name)) => name,
        None => lower.as_str(),
    };
    for prefix in ["bioconductor-", "r-", "python-"] {
        if let Some(rest) = bare.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    bare.to_string()
}

/// True when any script retained under `scripts_dir` contains a call-shaped,
/// non-commented occurrence of one of `entry_points` — the "was it actually
/// USED" half of the confirmation predicate.
///
/// An unreadable or absent `scripts/` directory yields `false`: no evidence is
/// not evidence of use.
fn scripts_invoke_any(scripts_dir: &Path, entry_points: &[&str]) -> bool {
    retained_script_paths(scripts_dir)
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .any(|source| source_calls_any(&source, entry_points))
}

/// Every code file retained under `dir`, sorted (a `BTreeSet` — `read_dir`
/// order is filesystem-dependent, and this crate's contract is deterministic
/// behaviour). Transcripts (`*.log`) are excluded.
fn retained_script_paths(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut found = std::collections::BTreeSet::new();
    collect_script_paths(dir, 0, &mut found);
    found.into_iter().collect()
}

/// Depth-bounded walk feeding [`retained_script_paths`].
fn collect_script_paths(
    dir: &Path,
    depth: usize,
    found: &mut std::collections::BTreeSet<std::path::PathBuf>,
) {
    if depth > MAX_SCRIPT_WALK_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        // `file_type()` does NOT follow symlinks, so a symlinked directory is
        // neither descended nor read — the walk cannot loop, and the scan
        // cannot be steered outside the package by a planted link.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_script_paths(&path, depth + 1, found);
        } else if file_type.is_file() && !is_transcript(&path) {
            found.insert(path);
        }
    }
}

/// True for a run transcript rather than a script.
fn is_transcript(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(TRANSCRIPT_EXT))
}

/// True when `source` calls any of `entry_points` outside a comment.
fn source_calls_any(source: &str, entry_points: &[&str]) -> bool {
    source.lines().any(|line| {
        let code = strip_comment(line);
        entry_points.iter().any(|token| line_calls(code, token))
    })
}

/// The code portion of one line: everything before the first `#` that is not
/// inside a quoted string. `#` is the comment marker in R, Python and shell —
/// the languages retained scripts are written in — so a commented-out call is
/// dropped before the call scan sees it. Quote tracking keeps a `#` inside a
/// string literal (a hex colour, a header line) from truncating real code.
///
/// Deliberately stateless across lines — this is a comment filter, not a
/// parser. A string literal continued across a newline is the one shape it
/// mis-reads, and it mis-reads it toward truncation, i.e. toward NOT finding
/// an invocation: the fail-closed direction, which withholds the exemption
/// rather than granting an unearned one.
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    for (i, byte) in line.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            Some(open) => {
                if byte == b'\\' {
                    escaped = true;
                } else if byte == open {
                    quote = None;
                }
            }
            // `#` and the quote bytes are ASCII, so `i` is always a char
            // boundary and the slice below cannot split a UTF-8 sequence.
            None => match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'#' => return &line[..i],
                _ => {}
            },
        }
    }
    line
}

/// True when `code` contains `token` in CALL shape: preceded by a
/// non-identifier byte (so `hash(` is not a call to `ash`) and immediately
/// followed by `(` (so a bare mention in prose or a `type = "apeglm"` argument
/// is not read as an invocation).
fn line_calls(code: &str, token: &str) -> bool {
    let bytes = code.as_bytes();
    let mut from = 0usize;
    while let Some(offset) = code[from..].find(token) {
        let start = from + offset;
        let end = start + token.len();
        let boundary_before = start == 0 || !is_identifier_byte(bytes[start - 1]);
        let call_after = bytes.get(end) == Some(&b'(');
        if boundary_before && call_after {
            return true;
        }
        // Tokens are ASCII, so `start + 1` is always a char boundary.
        from = start + 1;
    }
    false
}

/// Bytes that may appear INSIDE an identifier. `.` is included because R
/// permits it in names: `my.ash(` must not read as a call to `ash`.
fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shim_has_schema_v1() {
        let s = serialize_active_settings();
        assert_eq!(s.schema_version, "1");
    }

    #[test]
    fn temp_path_policy_is_stable_by_task_id() {
        let s = serialize_active_settings();
        assert_eq!(s.temp_path_policy.strategy, "stable-by-task-id");
    }

    #[test]
    #[serial_test::serial]
    fn temp_path_root_is_deterministic_and_does_not_leak_host_tmpdir() {
        // det-07 — the temp-path root must be a deterministic,
        // package-relative symbolic value, never the host `$TMPDIR`.
        // Set a bogus TMPDIR and assert the sidecar root ignores it.
        // `serial_test::serial` keeps this off the other env-mutating
        // suites; restore the prior value before returning.
        let prev = env::var("TMPDIR").ok();
        env::set_var("TMPDIR", "/tmp/claude-99999");
        let s = serialize_active_settings();
        assert_eq!(
            s.temp_path_policy.root, "runtime/scratch",
            "temp_path_policy.root must be the deterministic package-relative \
             scratch root, not the host $TMPDIR"
        );
        assert!(
            !s.temp_path_policy.root.contains("/tmp/"),
            "host $TMPDIR leaked into determinism-shim: {}",
            s.temp_path_policy.root
        );
        match prev {
            Some(v) => env::set_var("TMPDIR", v),
            None => env::remove_var("TMPDIR"),
        }
    }

    #[test]
    fn merge_container_env_unions_keys_deterministically() {
        let mut host = serialize_active_settings();
        host.env_capture.captured_env_vars = vec!["TZ".to_string()];
        // Container reported these set inside the container (out of order).
        let container = vec![
            "LC_ALL".to_string(),
            "PYTHONHASHSEED".to_string(),
            "TZ".to_string(), // duplicate must not double
        ];
        merge_container_env(&mut host, &container);
        assert_eq!(
            host.env_capture.captured_env_vars,
            vec![
                "LC_ALL".to_string(),
                "PYTHONHASHSEED".to_string(),
                "TZ".to_string()
            ],
            "merge must dedupe + sort (BTreeSet union) for byte-stability"
        );
    }

    #[test]
    fn host_snapshot_has_empty_acks() {
        let s = serialize_active_settings();
        assert!(
            s.non_deterministic_artifacts.is_empty(),
            "the compiler-host snapshot must never declare acks"
        );
    }

    #[test]
    fn old_shim_without_field_deserializes_to_empty_acks() {
        // A shim written before the field existed must load with an empty ack
        // set via serde default, not fail to parse.
        let json = "{\"schema_version\":\"1\",\"env_capture\":{\"captured_env_vars\":[],\
            \"redacted_env_vars\":[]},\"seed_policy\":{\"random_seed\":null,\
            \"seed_source\":\"process-default\"},\"temp_path_policy\":{\
            \"strategy\":\"stable-by-task-id\",\"root\":\"runtime/scratch\"},\
            \"locale\":\"C\",\"timezone\":\"UTC\",\"ablation_engaged\":false}";
        let shim: DeterminismShimSidecar =
            serde_json::from_str(json).expect("old shim must deserialize");
        assert!(shim.non_deterministic_artifacts.is_empty());
    }

    #[test]
    fn set_acks_sorts_and_dedups() {
        let mut shim = serialize_active_settings();
        shim.set_non_deterministic_artifacts(vec![
            NonDetAck {
                artifact: "z.tsv".into(),
                columns: None,
                kind: NonDetKind::UnseededRng,
                reason: "b".into(),
            },
            NonDetAck {
                artifact: "a.tsv".into(),
                columns: Some(vec!["c1".into()]),
                kind: NonDetKind::AdaptiveShrinkage,
                reason: "a".into(),
            },
            NonDetAck {
                artifact: "a.tsv".into(),
                columns: Some(vec!["c1".into()]),
                kind: NonDetKind::AdaptiveShrinkage,
                reason: "a".into(),
            },
        ]);
        // Sorted by artifact, duplicate collapsed.
        assert_eq!(shim.non_deterministic_artifacts.len(), 2);
        assert_eq!(shim.non_deterministic_artifacts[0].artifact, "a.tsv");
        assert_eq!(shim.non_deterministic_artifacts[1].artifact, "z.tsv");
    }

    #[test]
    fn ack_for_matches_by_filename_path_suffix_and_exact() {
        let mut shim = serialize_active_settings();
        shim.set_non_deterministic_artifacts(vec![NonDetAck {
            artifact: "de_results.tsv".into(),
            columns: Some(vec!["log2FoldChange".into()]),
            kind: NonDetKind::AdaptiveShrinkage,
            reason: "shrinkage".into(),
        }]);
        // File-name match against a full relative path.
        assert!(
            ack_for(
                &shim,
                "runtime/outputs/differential_expression/de_results.tsv"
            )
            .is_some(),
            "bare filename ack must match a full rel path by file name"
        );
        // Exact match.
        assert!(ack_for(&shim, "de_results.tsv").is_some());
        // Non-match.
        assert!(ack_for(&shim, "other.tsv").is_none());
    }

    #[test]
    fn ack_for_exact_path_match() {
        let mut shim = serialize_active_settings();
        shim.set_non_deterministic_artifacts(vec![NonDetAck {
            artifact: "results/tables/de.tsv".into(),
            columns: None,
            kind: NonDetKind::Other,
            reason: "whole artifact".into(),
        }]);
        assert!(ack_for(&shim, "results/tables/de.tsv").is_some());
        // A different directory with the same basename still matches by name.
        assert!(ack_for(&shim, "elsewhere/de.tsv").is_some());
    }

    #[test]
    fn seed_source_reflects_harness_injected_policy_when_source_date_epoch_unset() {
        // Pre-existing SOURCE_DATE_EPOCH would invalidate the
        // assertion — skip in that case rather than mutating env in a
        // non-serial test.
        if env::var("SOURCE_DATE_EPOCH").is_ok() {
            return;
        }
        let s = serialize_active_settings();
        // When the compiler host carries no SOURCE_DATE_EPOCH, the shim
        // declares the harness-injected applied policy rather than a bare
        // process default — so it stays consistent with per-task evidence.
        assert_eq!(
            s.seed_policy.seed_source,
            "harness-injected (SOURCE_DATE_EPOCH + PYTHONHASHSEED=0)"
        );
        assert!(s.seed_policy.random_seed.is_none());
    }

    #[test]
    fn captured_env_declares_applied_policy_floor() {
        // The applied-policy env vars the harness injects into every task
        // are always declared, regardless of the (possibly bare) compiler
        // host, so the package-level shim never contradicts per-task
        // determinism-env.json.
        let s = serialize_active_settings();
        for k in ["LANG", "PYTHONHASHSEED", "SOURCE_DATE_EPOCH", "TZ"] {
            assert!(
                s.env_capture.captured_env_vars.iter().any(|v| v == k),
                "applied-policy env var {k} must be declared in the shim capture"
            );
        }
        // Byte-stable: sorted + deduped.
        let mut sorted = s.env_capture.captured_env_vars.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, s.env_capture.captured_env_vars);
    }

    #[test]
    fn captured_env_declares_the_thread_budget() {
        // The harness sets all 13 numerical-library thread vars on every task
        // dispatch, and thread count fixes multithreaded reduction ORDER. An
        // undeclared budget is an unrecorded re-execution variable, so every
        // key must appear in the applied-policy floor — not only when the
        // compiler host happens to export it.
        let s = serialize_active_settings();
        for k in THREAD_BUDGET_ENV_VARS {
            assert!(
                s.env_capture.captured_env_vars.iter().any(|v| v == k),
                "thread-budget env var {k} must be declared in the shim capture"
            );
        }
        assert_eq!(
            THREAD_BUDGET_ENV_VARS.len(),
            13,
            "the canonical thread-budget list is 13 keys; a change here must be \
             mirrored in scripts/_agent-blas-bootstrap.sh (both the apply loop \
             and the container forward allowlist)"
        );
    }

    #[test]
    fn captured_env_is_sorted_and_deduped_after_adding_the_thread_budget() {
        // The capture is a `BTreeSet` union of two applied-policy classes plus a
        // host presence scan whose members OVERLAP the first class. Byte-stability
        // of `determinism-shim.json` depends on the result staying sorted with no
        // repeats regardless of host env ordering.
        let s = serialize_active_settings();
        let mut expected = s.env_capture.captured_env_vars.clone();
        expected.sort();
        expected.dedup();
        assert_eq!(
            expected, s.env_capture.captured_env_vars,
            "captured_env_vars must be sorted + deduped for byte-stability"
        );
        // Union semantics: at least both applied-policy classes, and no name
        // appears twice even though the host scan can re-supply LANG/TZ/etc.
        assert!(
            s.env_capture.captured_env_vars.len()
                >= APPLIED_POLICY_ENV_VARS.len() + THREAD_BUDGET_ENV_VARS.len(),
            "the floor must contain both applied-policy classes, got {:?}",
            s.env_capture.captured_env_vars
        );
    }

    #[test]
    fn thread_budget_list_has_no_duplicates() {
        let set: std::collections::BTreeSet<&&str> = THREAD_BUDGET_ENV_VARS.iter().collect();
        assert_eq!(
            set.len(),
            THREAD_BUDGET_ENV_VARS.len(),
            "THREAD_BUDGET_ENV_VARS must be deduplicated: {THREAD_BUDGET_ENV_VARS:?}"
        );
    }

    #[test]
    fn thread_budget_and_seed_policy_classes_are_disjoint() {
        // Two separate consts unioned into one floor; an accidental overlap
        // would be a sign the classes were confused (a seed var is not a
        // thread var and vice versa).
        for k in THREAD_BUDGET_ENV_VARS {
            assert!(
                !APPLIED_POLICY_ENV_VARS.contains(k),
                "{k} appears in both applied-policy classes"
            );
        }
    }

    #[test]
    #[serial_test::serial]
    fn locale_defaults_to_c_utf8_when_lang_and_lc_all_unset() {
        // With neither LC_ALL nor LANG set on the host, the shim declares the
        // harness-applied locale (C.UTF-8), matching per-task evidence — not a
        // bare "C". `serial` keeps this off the other env-mutating suites.
        let prev_lang = env::var("LANG").ok();
        let prev_lc = env::var("LC_ALL").ok();
        env::remove_var("LANG");
        env::remove_var("LC_ALL");
        let s = serialize_active_settings();
        assert_eq!(s.locale, "C.UTF-8");
        match prev_lang {
            Some(v) => env::set_var("LANG", v),
            None => env::remove_var("LANG"),
        }
        match prev_lc {
            Some(v) => env::set_var("LC_ALL", v),
            None => env::remove_var("LC_ALL"),
        }
    }

    // ---------------------------------------------------------------------
    // reconcile_declared_non_determinism
    // ---------------------------------------------------------------------

    const DE_TASK: &str = "differential_expression";
    const DE_ARTIFACT: &str = "runtime/outputs/differential_expression/de_results.tsv";
    const DE_EVIDENCE: &str = "runtime/outputs/differential_expression/result.json";

    /// Write a shim carrying exactly one emit-projected declaration, in the
    /// shape `conversation::emit::sidecars::write_determinism_shim` produces.
    fn seed_shim(root: &std::path::Path, kind: &str, confirmation_evidence: &str) {
        let runtime = root.join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        let shim = serde_json::json!({
            "schema_version": "1",
            "env_capture": { "captured_env_vars": ["TZ"], "redacted_env_vars": [] },
            "seed_policy": { "random_seed": null, "seed_source": "process-default" },
            "temp_path_policy": { "strategy": "stable-by-task-id", "root": "runtime/scratch" },
            "locale": "C.UTF-8",
            "timezone": "UTC",
            "ablation_engaged": false,
            "declared_non_determinism": {
                "status": "declared_pending_run_confirmation",
                "note": "Projected from static atom declarations at emit.",
                "declarations": [{
                    "task_id": DE_TASK,
                    "artifact": DE_ARTIFACT,
                    "columns": ["log2FC", "lfcSE"],
                    "kind": kind,
                    "reason": "Empirical-Bayes adaptive shrinkage of effect sizes.",
                    "status": "declared",
                    "confirmation_evidence": confirmation_evidence
                }]
            }
        });
        std::fs::write(
            runtime.join("determinism-shim.json"),
            serde_json::to_vec_pretty(&shim).unwrap(),
        )
        .unwrap();
    }

    /// Write the stage `result.json` the declaration points at.
    fn seed_result_json(root: &std::path::Path, task: &str, installed: &[(&str, &str)]) {
        let dir = root.join("runtime/outputs").join(task);
        std::fs::create_dir_all(&dir).unwrap();
        let packages: Vec<serde_json::Value> = installed
            .iter()
            .map(|(name, version)| {
                serde_json::json!({ "name": name, "version": version, "channel": "bioconda" })
            })
            .collect();
        let result = serde_json::json!({
            "task_id": task,
            "status": "completed",
            "language_packages_installed": packages
        });
        std::fs::write(
            dir.join("result.json"),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
    }

    /// Write one retained script into the stage's `scripts/` dir — the
    /// directory the "was it actually invoked" signal is read from.
    fn seed_script(root: &std::path::Path, task: &str, file_name: &str, body: &str) {
        let dir = root
            .join("runtime/outputs")
            .join(task)
            .join(RETAINED_SCRIPTS_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file_name), body).unwrap();
    }

    /// A retained R script that really calls the DESeq2 shrinkage front door.
    const INVOKING_R_SCRIPT: &str = "\
library(DESeq2)\n\
dds <- DESeq(dds)\n\
res_shrunk <- lfcShrink(dds, coef = coef_name, type = \"apeglm\", quiet = TRUE)\n\
write.table(as.data.frame(res_shrunk), \"de_results.tsv\", sep = \"\\t\")\n";

    /// A retained R script that fits the model but never shrinks — the engine
    /// may be installed, yet nothing here invokes it.
    const NON_INVOKING_R_SCRIPT: &str = "\
library(DESeq2)\n\
dds <- DESeq(dds)\n\
res <- results(dds, alpha = 0.05)\n\
write.table(as.data.frame(res), \"de_results.tsv\", sep = \"\\t\")\n";

    fn read_shim_json(root: &std::path::Path) -> serde_json::Value {
        let body = std::fs::read(root.join("runtime/determinism-shim.json")).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// The one declaration in the reconciled block.
    fn only_declaration(shim: &serde_json::Value) -> &serde_json::Value {
        &shim["declared_non_determinism"]["declarations"][0]
    }

    /// An observed run called `lfcShrink(type="apeglm")` and recorded the
    /// choice, yet `language_packages_installed` listed only DESeq2 — the
    /// engine came from the base image, not a stage install. Ranking the
    /// install record above the invocation would FALSE-REFUTE a stage that
    /// genuinely shrank, and `lfcShrink(type="normal")` (DESeq2's own
    /// shrinkage) can never appear in an install set at all. Invocation
    /// decides; the install record only corroborates.
    #[test]
    fn invocation_confirms_even_when_the_engine_was_never_separately_installed() {
        let tmp = tempfile::tempdir().unwrap();
        seed_shim(tmp.path(), "adaptive_shrinkage", DE_EVIDENCE);
        seed_result_json(tmp.path(), DE_TASK, &[("DESeq2", "1.50.2")]);
        seed_script(tmp.path(), DE_TASK, "01_deseq2_de.R", INVOKING_R_SCRIPT);

        let confirmed = reconcile_declared_non_determinism(tmp.path()).unwrap();
        assert_eq!(
            confirmed, 1,
            "a real invocation must confirm without an install record"
        );
        let shim = read_shim_json(tmp.path());
        assert_eq!(only_declaration(&shim)["status"], "confirmed");
    }

    #[test]
    fn reconcile_confirms_when_evidence_names_a_shrinkage_engine() {
        let tmp = tempfile::tempdir().unwrap();
        seed_shim(tmp.path(), "adaptive_shrinkage", DE_EVIDENCE);
        seed_result_json(
            tmp.path(),
            DE_TASK,
            &[("apeglm", "1.32.0"), ("DESeq2", "1.50.2")],
        );
        // Confirmation turns on the INVOCATION in a retained script; the
        // install record merely corroborates it.
        seed_script(tmp.path(), DE_TASK, "01_deseq2_de.R", INVOKING_R_SCRIPT);

        let confirmed = reconcile_declared_non_determinism(tmp.path()).unwrap();
        assert_eq!(
            confirmed, 1,
            "one declaration was confirmed by run evidence"
        );

        let shim = read_shim_json(tmp.path());
        assert_eq!(
            shim["declared_non_determinism"]["status"], "reconciled",
            "the block must record that a reconciliation pass ran"
        );
        assert_eq!(
            only_declaration(&shim)["status"],
            "confirmed",
            "the declaration must be marked confirmed"
        );

        // The mask is EARNED: the artifact + its columns now appear in
        // `non_deterministic_artifacts`, so `ack_for` covers it.
        let parsed: DeterminismShimSidecar =
            serde_json::from_value(shim.clone()).expect("reconciled shim still parses");
        assert_eq!(
            parsed.non_deterministic_artifacts.len(),
            1,
            "confirmation must grant exactly one mask entry"
        );
        let ack = ack_for(&parsed, DE_ARTIFACT).expect("confirmed artifact is masked");
        assert_eq!(
            ack.kind,
            NonDetKind::AdaptiveShrinkage,
            "the mask must carry the declared kind"
        );
        assert_eq!(
            ack.columns,
            Some(vec!["log2FC".to_string(), "lfcSE".to_string()]),
            "the mask must inherit the declaration's column scope, not widen it"
        );

        // Idempotent: re-running over the same evidence changes nothing.
        let before = std::fs::read(tmp.path().join("runtime/determinism-shim.json")).unwrap();
        let again = reconcile_declared_non_determinism(tmp.path()).unwrap();
        let after = std::fs::read(tmp.path().join("runtime/determinism-shim.json")).unwrap();
        assert_eq!(again, 1, "re-reconciliation reports the same confirmation");
        assert_eq!(before, after, "re-reconciliation must be byte-idempotent");
    }

    #[test]
    fn confirmation_turns_on_invocation_not_on_the_install_record() {
        // INVOCATION decides. `language_packages_installed` proves only that
        // the engine was reachable, never that the stage ran it — and it is
        // unreliable in the other direction too (an image-provided engine, or
        // DESeq2-native `lfcShrink(type="normal")`, leaves no install record
        // for a run that genuinely shrank). So the invoked column alone
        // determines the verdict, and the installed column must not change it.
        let cases = [
            (true, true, 1usize, "confirmed"),
            (false, true, 1, "confirmed"),
            (true, false, 0, "refuted"),
            (false, false, 0, "refuted"),
        ];
        for (installed, invoked, expect_confirmed, expect_status) in cases {
            let tmp = tempfile::tempdir().unwrap();
            seed_shim(tmp.path(), "adaptive_shrinkage", DE_EVIDENCE);
            let packages: &[(&str, &str)] = if installed {
                &[("DESeq2", "1.50.2"), ("apeglm", "1.32.0")]
            } else {
                &[("DESeq2", "1.50.2")]
            };
            seed_result_json(tmp.path(), DE_TASK, packages);
            seed_script(
                tmp.path(),
                DE_TASK,
                "01_deseq2_de.R",
                if invoked {
                    INVOKING_R_SCRIPT
                } else {
                    NON_INVOKING_R_SCRIPT
                },
            );

            assert_eq!(
                reconcile_declared_non_determinism(tmp.path()).unwrap(),
                expect_confirmed,
                "installed={installed} invoked={invoked} must yield \
                 {expect_confirmed} confirmation(s)"
            );
            let shim = read_shim_json(tmp.path());
            assert_eq!(
                only_declaration(&shim)["status"],
                expect_status,
                "installed={installed} invoked={invoked} must be recorded {expect_status}"
            );
            let parsed: DeterminismShimSidecar = serde_json::from_value(shim).unwrap();
            assert_eq!(
                parsed.non_deterministic_artifacts.len(),
                expect_confirmed,
                "installed={installed} invoked={invoked} must grant \
                 {expect_confirmed} mask entr(y/ies)"
            );
        }
    }

    #[test]
    fn engine_installed_but_never_invoked_is_refuted() {
        let tmp = tempfile::tempdir().unwrap();
        seed_shim(tmp.path(), "adaptive_shrinkage", DE_EVIDENCE);
        // The image (or a transitive dependency) made apeglm AVAILABLE, so the
        // install-set signal alone holds...
        seed_result_json(
            tmp.path(),
            DE_TASK,
            &[("DESeq2", "1.50.2"), ("apeglm", "1.32.0")],
        );
        // ...but the script the stage actually ran never shrinks anything.
        seed_script(tmp.path(), DE_TASK, "01_deseq2_de.R", NON_INVOKING_R_SCRIPT);
        // A transcript can echo a call that errored out or was never reached,
        // so `*.log` is excluded from the scan and must not flip the verdict.
        seed_script(
            tmp.path(),
            DE_TASK,
            "01_deseq2_de.log",
            "R> res_shrunk <- lfcShrink(dds, coef = \"cond\", type = \"apeglm\")\n\
             Error: object 'dds' not found\n",
        );

        assert_eq!(
            reconcile_declared_non_determinism(tmp.path()).unwrap(),
            0,
            "availability without invocation must confirm nothing"
        );

        let shim = read_shim_json(tmp.path());
        assert_eq!(
            only_declaration(&shim)["status"],
            "refuted",
            "an installed-but-unused engine is a refutation, not an open question"
        );
        // NO mask entry is created at all — not an empty-columns one, not a
        // whole-artifact one.
        assert!(
            shim.get("non_deterministic_artifacts").is_none(),
            "an unused engine must not write the mask key at all, got: {shim}"
        );
        let parsed: DeterminismShimSidecar = serde_json::from_value(shim).unwrap();
        assert!(
            parsed.non_deterministic_artifacts.is_empty(),
            "availability alone must never earn an exemption"
        );
        assert!(
            ack_for(&parsed, DE_ARTIFACT).is_none(),
            "the comparator must still equivalence-check this artifact"
        );
    }

    #[test]
    fn commented_out_invocation_is_not_evidence() {
        let tmp = tempfile::tempdir().unwrap();
        seed_shim(tmp.path(), "adaptive_shrinkage", DE_EVIDENCE);
        seed_result_json(
            tmp.path(),
            DE_TASK,
            &[("DESeq2", "1.50.2"), ("apeglm", "1.32.0")],
        );
        // Every occurrence of an entry point sits behind a `#`: a full-line
        // comment, a trailing comment, and a direct-call mention. None ran.
        seed_script(
            tmp.path(),
            DE_TASK,
            "01_deseq2_de.R",
            "library(DESeq2)\n\
             # res_shrunk <- lfcShrink(dds, coef = coef_name, type = \"apeglm\") # disabled\n\
             res <- results(dds, alpha = 0.05)  # could also use lfcShrink(dds)\n\
             #   apeglm(x, y) direct call also disabled\n\
             write.table(as.data.frame(res), \"de_results.tsv\", sep = \"\\t\")\n",
        );

        assert_eq!(
            reconcile_declared_non_determinism(tmp.path()).unwrap(),
            0,
            "a commented-out call is not evidence the method ran"
        );
        let shim = read_shim_json(tmp.path());
        assert_eq!(
            only_declaration(&shim)["status"],
            "refuted",
            "the engine was available and demonstrably not invoked"
        );
        let parsed: DeterminismShimSidecar = serde_json::from_value(shim).unwrap();
        assert!(
            parsed.non_deterministic_artifacts.is_empty(),
            "commented-out code must grant no exemption"
        );
    }

    #[test]
    fn call_scan_ignores_mentions_and_respects_identifier_boundaries() {
        // Unit-level guard on the call-shape rule the two-signal predicate
        // rests on, so a regression is localized rather than surfacing only
        // through a whole reconciliation.
        assert!(
            source_calls_any("res <- lfcShrink(dds, type = \"apeglm\")\n", &["lfcShrink"]),
            "a plain call must be recognized"
        );
        assert!(
            source_calls_any("res <- DESeq2::lfcShrink(dds)\n", &["lfcShrink"]),
            "a namespace-qualified call must be recognized (`::` is a boundary)"
        );
        assert!(
            !source_calls_any(
                "cat(\"Using lfcShrink coef:\", coef_name)\n",
                &["lfcShrink"]
            ),
            "a bare mention with no `(` is not a call"
        );
        assert!(
            !source_calls_any("res <- lfcShrink(dds)\n", &["apeglm"]),
            "the `type=` argument value is not itself an invocation"
        );
        assert!(
            !source_calls_any("h <- hash(x)\ny <- my.ash(z)\n", &["ash"]),
            "an identifier that merely ENDS in the token is not a call to it"
        );
        assert!(
            source_calls_any("fit <- ashr::ash(betahat, sebetahat)\n", &["ash"]),
            "the qualified estimator call must be recognized"
        );
        // A `#` inside a string literal must not truncate real code.
        assert!(
            source_calls_any(
                "plot(col = \"#FF0000\"); res <- lfcShrink(dds)\n",
                &["lfcShrink"]
            ),
            "a `#` inside a quoted string is not a comment marker"
        );
    }

    #[test]
    fn reconcile_refutes_when_evidence_lacks_the_engine() {
        let tmp = tempfile::tempdir().unwrap();
        seed_shim(tmp.path(), "adaptive_shrinkage", DE_EVIDENCE);
        // The run installed DESeq2 only AND retained no script that calls a
        // shrinkage entry point, so the static claim that this stage shrank
        // effect sizes is contradicted on the signal that actually decides it
        // (invocation) as well as on the corroborating install record.
        seed_result_json(tmp.path(), DE_TASK, &[("DESeq2", "1.50.2")]);

        let confirmed = reconcile_declared_non_determinism(tmp.path()).unwrap();
        assert_eq!(confirmed, 0, "no declaration may be confirmed");

        let shim = read_shim_json(tmp.path());
        assert_eq!(
            only_declaration(&shim)["status"],
            "refuted",
            "execution contradicted the static claim — keep it as a record"
        );
        assert_eq!(
            shim["declared_non_determinism"]["status"], "reconciled",
            "the block must record that a reconciliation pass ran"
        );

        // NO mask is granted: the artifact stays subject to equivalence checking.
        assert!(
            shim.get("non_deterministic_artifacts").is_none(),
            "a refuted declaration must not write the mask key at all, got: {shim}"
        );
        let parsed: DeterminismShimSidecar = serde_json::from_value(shim).unwrap();
        assert!(
            parsed.non_deterministic_artifacts.is_empty(),
            "a refuted declaration must grant no exemption"
        );
        assert!(
            ack_for(&parsed, DE_ARTIFACT).is_none(),
            "the comparator must still see this artifact as un-acked"
        );
    }

    #[test]
    fn reconcile_is_fail_closed_when_evidence_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        seed_shim(tmp.path(), "adaptive_shrinkage", DE_EVIDENCE);
        // No result.json written: the stage never produced the evidence the
        // declaration names.

        let confirmed = reconcile_declared_non_determinism(tmp.path()).unwrap();
        assert_eq!(confirmed, 0, "unreadable evidence can confirm nothing");

        let shim = read_shim_json(tmp.path());
        assert_eq!(
            only_declaration(&shim)["status"],
            "declared",
            "missing evidence is not a refutation — the entry stays unresolved"
        );
        assert_eq!(
            shim["declared_non_determinism"]["status"], "reconciled",
            "the pass still records that it ran"
        );
        let parsed: DeterminismShimSidecar = serde_json::from_value(shim).unwrap();
        assert!(
            parsed.non_deterministic_artifacts.is_empty(),
            "fail-closed: no evidence, no mask"
        );
    }

    #[test]
    fn reconcile_leaves_kinds_without_a_predicate_declared() {
        let tmp = tempfile::tempdir().unwrap();
        // `other` has no positive two-signal predicate; readable evidence must
        // still not confirm it, and must not be misread as a refutation.
        seed_shim(tmp.path(), "other", DE_EVIDENCE);
        seed_result_json(tmp.path(), DE_TASK, &[("apeglm", "1.32.0")]);

        let confirmed = reconcile_declared_non_determinism(tmp.path()).unwrap();
        assert_eq!(confirmed, 0, "a kind with no predicate is never confirmed");

        let shim = read_shim_json(tmp.path());
        assert_eq!(
            only_declaration(&shim)["status"],
            "declared",
            "no predicate exists, so no verdict is possible"
        );
        let parsed: DeterminismShimSidecar = serde_json::from_value(shim).unwrap();
        assert!(
            parsed.non_deterministic_artifacts.is_empty(),
            "no mask without a confirmation rule"
        );
    }

    #[test]
    fn reconcile_normalizes_channel_and_feedstock_prefixed_engine_names() {
        // Real runs record the engine under whichever packaging surface
        // installed it: `apeglm`, `bioconductor-apeglm`, or `bioconda::ashr`.
        for recorded in ["apeglm", "bioconductor-apeglm", "bioconda::ashr", "r-ashr"] {
            let tmp = tempfile::tempdir().unwrap();
            seed_shim(tmp.path(), "adaptive_shrinkage", DE_EVIDENCE);
            seed_result_json(tmp.path(), DE_TASK, &[(recorded, "1.0.0")]);
            seed_script(tmp.path(), DE_TASK, "01_deseq2_de.R", INVOKING_R_SCRIPT);
            assert_eq!(
                reconcile_declared_non_determinism(tmp.path()).unwrap(),
                1,
                "recorded engine name {recorded} must confirm the declaration"
            );
        }
    }

    #[test]
    fn reconcile_is_a_noop_without_a_declaration_block() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        let body = serde_json::to_vec_pretty(&serialize_active_settings()).unwrap();
        std::fs::write(runtime.join("determinism-shim.json"), &body).unwrap();

        assert_eq!(
            reconcile_declared_non_determinism(tmp.path()).unwrap(),
            0,
            "no declarations means nothing to confirm"
        );
        assert_eq!(
            std::fs::read(runtime.join("determinism-shim.json")).unwrap(),
            body,
            "a shim with no declaration block must be left byte-identical"
        );
        // A package with no shim at all is also a no-op.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            reconcile_declared_non_determinism(empty.path()).unwrap(),
            0,
            "a package with no shim must not error"
        );
    }
}
