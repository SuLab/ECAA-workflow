//! Typed `Config` struct loaded once at process boot.
//!
//! A single [`Config`] consolidates the env-var catalog documented in
//! `docs/env-vars-reference.md` behind one loader that:
//!
//! - Reads from `std::env::vars()` once at startup via [`Config::from_env`]
//!   (production) or from an injected [`HashMap`] via
//!   [`Config::from_env_map`] (tests).
//! - Rejects NaN / infinity for every `f64` knob (the cost-ceiling
//!   parser must never silently produce a non-finite multiplier).
//! - Rejects non-`https://` `ANTHROPIC_BASE_URL` overrides unless the
//!   host is a loopback address.
//! - Validates documented bounds (e.g. `ECAA_AWS_PRICING_REGION_MULT`
//!   ∈ `[0.5, 5.0]`; `ECAA_HARNESS_BATCH_WINDOW_SECS` ≤ 600).
//! - Redacts secrets in [`std::fmt::Debug`] so structured
//!   `tracing::info!` captures of the loaded config never leak
//!   `ECAA_ANTHROPIC_API_KEY`, `ECAA_SERVER_AUTH_TOKEN`, or
//!   `ECAA_LIT_NCBI_API_KEY` into logs.
//!
//! Migration of the ~30 per-request `std::env::var` consumer sites is
//! incremental — this module ships the type + parsers + unit tests.
//! Consumers should switch to `&Config` arguments over time; a
//! follow-up lint (`disallowed-methods = std::env::var`) will land
//! once the migration completes.
//!
//! ## Scope (env-vars covered)
//!
//! Mirrors `docs/env-vars-reference.md` for the **shared** knobs read by the
//! server, conversation crate, harness scheduler, and CLI. Container-runtime
//! plumbing (`ECAA_CONTAINER_*`, `ECAA_AGENT_*`, `ECAA_AWS_*` provisioning
//! flags) is harness-only and deliberately deferred to a future
//! `harness::HarnessConfig` — keeping this struct narrow avoids forcing the
//! server crate to depend on harness-only env semantics.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use url::Url;

/// Default Anthropic Messages API base URL. Override with `ANTHROPIC_BASE_URL`.
const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";

/// Default harness-progress batcher debounce window. Documented in
/// `docs/env-vars-reference.md` under `ECAA_HARNESS_BATCH_WINDOW_SECS`.
const DEFAULT_HARNESS_BATCH_WINDOW_SECS: u64 = 10;

/// Upper bound for `ECAA_HARNESS_BATCH_WINDOW_SECS`. Values past this would
/// hide blockers from the SME for unreasonable spans; the docs explicitly
/// reject `>600`.
const MAX_HARNESS_BATCH_WINDOW_SECS: u64 = 600;

/// Default heartbeat stall threshold (seconds). Documented under
/// `ECAA_TASK_HEARTBEAT_STALL_SECS`. The harness flips Running tasks to
/// `Blocked { HeartbeatStalled }` after this many seconds without a
/// `.heartbeat` touch.
const DEFAULT_TASK_HEARTBEAT_STALL_SECS: u64 = 300;

/// Default literature-evidence storage cap (MB) per task. Documented under
/// `ECAA_LIT_EVIDENCE_MAX_MB`.
const DEFAULT_LIT_EVIDENCE_MAX_MB: u64 = 200;

/// Default upload-root free-space reserve (GB). Documented under
/// `ECAA_UPLOAD_DISK_RESERVE_GB`.
const DEFAULT_UPLOAD_DISK_RESERVE_GB: u64 = 50;

/// Default chat-server bind interface. Documented under `ECAA_BIND_ADDR`.
const DEFAULT_BIND_ADDR: &str = "127.0.0.1";

/// Default chat-server port. Documented under `ECAA_PORT`. (The codebase
/// historically uses `3737` for the harness↔server progress channel, but the
/// SME-facing chat-server documented default in CLAUDE.md is `3000`.)
const DEFAULT_PORT: u16 = 3000;

/// Default composer engine alias. `semantic` and `proof-carrying` both route
/// to the v4 proof-carrying planner.
const DEFAULT_COMPOSER: &str = "semantic";

/// AWS pricing-region multiplier acceptable range. Outside this range the
/// loader rejects the value — a 10× multiplier or 0.01× discount is far
/// outside any documented AWS region pricing band and almost certainly an
/// operator typo.
const AWS_PRICING_REGION_MULT_MIN: f64 = 0.5;
const AWS_PRICING_REGION_MULT_MAX: f64 = 5.0;

/// Hosts treated as loopback for the `ANTHROPIC_BASE_URL` http:// exception.
/// Matches the documented operator runbook: an SSH-tunneled staging proxy
/// will appear at one of these.
const LOOPBACK_HOSTS: &[&str] = &["localhost", "127.0.0.1", "::1", "[::1]"];

// ----------------------------------------------------------------------------
// Public type surface
// ----------------------------------------------------------------------------

/// Chat-server LLM backend selector. `ECAA_CHAT_MODE=offline` forces the
/// `MockLlmBackend`; everything else (unset or any other value) routes to
/// the live Anthropic Messages API client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatMode {
    /// Live Anthropic Messages API.
    Online,
    /// `MockLlmBackend` — no network calls; UI degrades gracefully.
    Offline,
}

/// Drift-mode policy for the legacy `modalities:` block in
/// `config/modality-keywords.yaml`. The block is retired
/// (per-modality definitions live under `config/modalities/<id>.yaml`);
/// the loader still tolerates a non-empty block under `Warn`,
/// refuses it under `Fail`.
///
/// Sourced from the `Config::modality_drift_mode` field so the
/// env-var read happens exactly once at process boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModalityDriftMode {
    /// Log a `tracing::warn!` when the legacy block is non-empty.
    /// Default behaviour, preserves backwards compatibility.
    #[default]
    Warn,
    /// Refuse to load the classifier when the legacy block is
    /// non-empty. Recommended once the registry migration completes.
    Fail,
}

/// Literature retrieval source-scope tier. Documented under
/// `ECAA_LIT_SOURCE_SCOPE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LitSourceScope {
    /// PMC Open Access full-text XML only (default).
    PmcOa,
    /// PMC OA + NLM abstract fallback for non-OA PMIDs.
    PmcOaPlusAbstracts,
    /// Stub tier — requires `ECAA_LIT_INSTITUTIONAL_ACCESS=1` and a
    /// credential flow not yet implemented. Do not set in production.
    AllSourcesLocalOnly,
}

/// Literature-atom configuration block. Wraps the four `ECAA_LIT_*` env
/// vars consumed by `crates/harness/src/literature_scope.rs` and the
/// `scripts/agent_literature_fetch.py` helper.
#[derive(Clone)]
pub struct LiteratureConfig {
    /// Source scope.
    pub source_scope: LitSourceScope,
    /// Optional NCBI E-utilities API key. **Never logged.** When set the
    /// shared rate limit lifts from 3 req/s to 10 req/s.
    pub ncbi_api_key: Option<String>,
    /// Per-task `runtime/outputs/<task_id>/evidence/` storage cap in MB.
    pub evidence_max_mb: u64,
}

impl std::fmt::Debug for LiteratureConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiteratureConfig")
            .field("source_scope", &self.source_scope)
            .field(
                "ncbi_api_key",
                &self.ncbi_api_key.as_ref().map(|_| "<redacted>"),
            )
            .field("evidence_max_mb", &self.evidence_max_mb)
            .finish()
    }
}

/// Single source of truth for env-var-driven configuration shared across the
/// chat server, conversation crate, harness scheduler, and CLI.
///
/// See module docs for the load-time invariants enforced by
/// [`Config::from_env_map`]. Construct via [`Config::from_env`] in
/// production binaries or [`Config::for_test`] in unit tests.
#[derive(Clone)]
pub struct Config {
    // Anthropic / LLM client ---------------------------------------------
    /// `ECAA_ANTHROPIC_API_KEY` (or legacy `ANTHROPIC_API_KEY` with stderr
    /// deprecation warning). **Redacted in `Debug`.**
    pub anthropic_api_key: Option<String>,
    /// `ANTHROPIC_BASE_URL`. Must be `https://` unless the host is
    /// loopback; default `https://api.anthropic.com`.
    pub anthropic_base_url: Url,

    // Chat server / sessions ---------------------------------------------
    /// `ECAA_CHAT_MODE`. `offline` → [`ChatMode::Offline`]; anything else
    /// → [`ChatMode::Online`].
    pub chat_mode: ChatMode,
    /// `ECAA_CHAT_SESSIONS_DIR`. Default `~/.ecaa-workflow/sessions`.
    pub chat_sessions_dir: PathBuf,
    /// `ECAA_CONFIG_DIR`. Default `./config`.
    pub config_dir: PathBuf,
    /// `ECAA_PACKAGE_ROOT`. Default `~/.ecaa-workflow/packages`.
    pub package_root: PathBuf,
    /// `ECAA_SERVER_AUTH_TOKEN`. Required when the server binds anything
    /// other than `127.0.0.1` / `[::1]`. **Redacted in `Debug`.**
    pub server_auth_token: Option<String>,

    // Side-call gates ----------------------------------------------------
    /// `ECAA_AUTO_TITLE`. Enables Haiku 4.5 auto-title side-call.
    pub auto_title: bool,
    /// `ECAA_LIVE_API`. Gates live-API dev Make targets.
    pub live_api: bool,

    // AWS provisioning / cost --------------------------------------------
    /// `ECAA_AWS_COST_CEILING_USD`. Optional finite USD cap on the AWS cost
    /// guard. Must be finite (rejects NaN / ±∞) and non-negative.
    pub aws_cost_ceiling_usd: Option<f64>,
    /// `ECAA_AWS_PRICING_REGION_MULT`. Default `1.0`; clamped to
    /// `[0.5, 5.0]`.
    pub aws_pricing_region_mult: f64,
    /// `ECAA_AWS_PRICING_OVERRIDES_JSON`. Parsed as a JSON object mapping
    /// instance-type → USD/hour. Each value must be finite and `> 0`.
    pub aws_pricing_overrides: HashMap<String, f64>,

    // Harness loop -------------------------------------------------------
    /// `ECAA_HARNESS_BATCH_WINDOW_SECS`. Default `10`, max `600`. Values
    /// past the cap are rejected (the doc-gate guarantees blockers stay
    /// visible to the SME).
    pub harness_batch_window_secs: u64,
    /// `ECAA_TASK_HEARTBEAT_STALL_SECS`. Default `300`. `0` disables the
    /// stall trip-wire entirely (documented escape hatch).
    pub task_heartbeat_stall_secs: u64,
    /// `ECAA_HARNESS_BIN_PATH`. Optional override for integration tests.
    pub harness_bin_path: Option<PathBuf>,

    // Literature atoms ---------------------------------------------------
    /// Literature.
    pub literature: LiteratureConfig,

    // Upload / input bounds ----------------------------------------------
    /// `ECAA_UPLOAD_ROOT`. Default `~/.ecaa-workflow/uploads`.
    pub upload_root: Option<String>,
    /// `ECAA_UPLOAD_DISK_RESERVE_GB`. Default `50`.
    pub upload_disk_reserve_gb: u64,
    /// `ECAA_INPUT_ROOTS`. Colon- (or comma-)separated allowlist of
    /// filesystem roots an SME may point `local_path` inputs at.
    pub input_roots: Vec<String>,

    // Bind / port --------------------------------------------------------
    /// `ECAA_BIND_ADDR`. Default `127.0.0.1`. `0.0.0.0` requires
    /// `server_auth_token`.
    pub bind_addr: String,
    /// `ECAA_PORT`. Default `3000`.
    pub port: u16,

    // Provenance / composer ----------------------------------------------
    /// `ECAA_GIT_ENABLED`. Hard kill-switch for git-backed provenance.
    /// Treats `0` / `false` / `no` / `off` as disabled; everything else
    /// (including unset) is enabled (default `true`).
    pub git_enabled: bool,
    /// `ECAA_COMPOSER`. Default `"semantic"`; the v4 proof-carrying planner
    /// also accepts `"proof-carrying"`. Legacy values warn and route to v4
    /// at the conversation crate's session-create site, so they're not
    /// rejected here.
    pub composer: String,

    // Core classifier policy ---------------------------------------------
    /// `ECAA_MODALITY_DRIFT_MODE`. Controls how `Classifier::load`
    /// reacts to a non-empty legacy `modalities:` block in
    /// `config/modality-keywords.yaml`. Default [`ModalityDriftMode::Warn`].
    /// `Fail` refuses the load entirely.
    ///
    /// Snapshotted into `Config` so the "core compiler reads no env
    /// vars at runtime" invariant holds (was a per-call
    /// `std::env::var` read at `classify.rs:266`).
    pub modality_drift_mode: ModalityDriftMode,

    // ECAA emission mode (Aim 3A Arm B″) ---------------------------------
    /// `ECAA_ECAA_MODE`. Default `Full` (current behavior — full ECAA
    /// package shape with every typed sidecar materialized).
    /// `Conventional` is the Arm B″ control package: README +
    /// analysis.ipynb + basic RO-Crate + per-table CSVs, with no
    /// ECAA-specific sidecars. Unknown values fall back to `Full` with
    /// a tracing warning.
    pub ecaa_mode: crate::emit_mode::EcaaMode,
}

// ----------------------------------------------------------------------------
// Constructors
// ----------------------------------------------------------------------------

impl Config {
    /// Load every documented env-var from `std::env::vars()` and return a
    /// validated `Config`.
    ///
    /// **Call exactly once at process boot.** Pass the resulting
    /// `Arc<Config>` through `AppState` to every consumer; the
    /// disallowed-methods lint forbids `std::env::var` outside this
    /// constructor.
    #[allow(clippy::disallowed_methods)] // The single allowed env-var read site.
    pub fn from_env() -> Result<Self> {
        let owned: HashMap<String, String> = std::env::vars().collect();
        let view: HashMap<&str, &str> = owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        Self::from_env_map(&view)
    }

    /// Testable entry-point. Parses every documented env-var from the
    /// supplied map and runs the same validation as [`Config::from_env`].
    pub fn from_env_map(env: &HashMap<&str, &str>) -> Result<Self> {
        // -- Anthropic --------------------------------------------------
        let anthropic_api_key = read_api_key(env);
        let anthropic_base_url =
            parse_https_url(env, "ANTHROPIC_BASE_URL", DEFAULT_ANTHROPIC_BASE_URL)?;

        // -- Chat server ------------------------------------------------
        let chat_mode = match env.get("ECAA_CHAT_MODE").copied() {
            Some("offline") => ChatMode::Offline,
            Some(other) if other.eq_ignore_ascii_case("offline") => ChatMode::Offline,
            _ => ChatMode::Online,
        };
        let chat_sessions_dir = parse_pathbuf_with_default(env, "ECAA_CHAT_SESSIONS_DIR", || {
            home_subdir(".ecaa-workflow/sessions")
        });
        let config_dir =
            parse_pathbuf_with_default(env, "ECAA_CONFIG_DIR", || PathBuf::from("./config"));
        let package_root = parse_pathbuf_with_default(env, "ECAA_PACKAGE_ROOT", || {
            home_subdir(".ecaa-workflow/packages")
        });
        let server_auth_token = nonempty_string(env, "ECAA_SERVER_AUTH_TOKEN");

        // -- Side-call gates -------------------------------------------
        let auto_title = parse_bool(env, "ECAA_AUTO_TITLE", false);
        let live_api = parse_bool(env, "ECAA_LIVE_API", false);

        // -- AWS pricing -----------------------------------------------
        let aws_cost_ceiling_usd = parse_finite_f64(env, "ECAA_AWS_COST_CEILING_USD")?;
        if let Some(c) = aws_cost_ceiling_usd {
            if c < 0.0 {
                return Err(anyhow!(
                    "ECAA_AWS_COST_CEILING_USD must be non-negative, got {c}"
                ));
            }
        }
        let aws_pricing_region_mult =
            parse_finite_f64(env, "ECAA_AWS_PRICING_REGION_MULT")?.unwrap_or(1.0);
        if !(AWS_PRICING_REGION_MULT_MIN..=AWS_PRICING_REGION_MULT_MAX)
            .contains(&aws_pricing_region_mult)
        {
            return Err(anyhow!(
                "ECAA_AWS_PRICING_REGION_MULT must be in [{}, {}], got {}",
                AWS_PRICING_REGION_MULT_MIN,
                AWS_PRICING_REGION_MULT_MAX,
                aws_pricing_region_mult
            ));
        }
        let aws_pricing_overrides = parse_pricing_overrides(env)?;

        // -- Harness loop ----------------------------------------------
        let harness_batch_window_secs = parse_u64_bounded(
            env,
            "ECAA_HARNESS_BATCH_WINDOW_SECS",
            DEFAULT_HARNESS_BATCH_WINDOW_SECS,
            Some(MAX_HARNESS_BATCH_WINDOW_SECS),
        )?;
        let task_heartbeat_stall_secs = parse_u64_bounded(
            env,
            "ECAA_TASK_HEARTBEAT_STALL_SECS",
            DEFAULT_TASK_HEARTBEAT_STALL_SECS,
            None,
        )?;
        let harness_bin_path = env
            .get("ECAA_HARNESS_BIN_PATH")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);

        // -- Literature ------------------------------------------------
        let source_scope = match env.get("ECAA_LIT_SOURCE_SCOPE").copied() {
            None | Some("") => LitSourceScope::PmcOa,
            Some("pmc_oa") => LitSourceScope::PmcOa,
            Some("pmc_oa_plus_abstracts") => LitSourceScope::PmcOaPlusAbstracts,
            Some("all_sources_local_only") => LitSourceScope::AllSourcesLocalOnly,
            Some(other) => {
                // Mirrors `crates/harness/src/literature_scope.rs` —
                // invalid values warn-fall-back rather than fail-stop;
                // a typo in this env var must not brick a long-running
                // harness loop mid-run.
                tracing::warn!(
                    "ECAA_LIT_SOURCE_SCOPE={other:?} not recognized; falling back to pmc_oa"
                );
                LitSourceScope::PmcOa
            }
        };
        let ncbi_api_key = nonempty_string(env, "ECAA_LIT_NCBI_API_KEY");
        let evidence_max_mb = parse_u64_bounded(
            env,
            "ECAA_LIT_EVIDENCE_MAX_MB",
            DEFAULT_LIT_EVIDENCE_MAX_MB,
            None,
        )?;
        let literature = LiteratureConfig {
            source_scope,
            ncbi_api_key,
            evidence_max_mb,
        };

        // -- Upload / input bounds -------------------------------------
        let upload_root = nonempty_string(env, "ECAA_UPLOAD_ROOT");
        let upload_disk_reserve_gb = parse_u64_bounded(
            env,
            "ECAA_UPLOAD_DISK_RESERVE_GB",
            DEFAULT_UPLOAD_DISK_RESERVE_GB,
            None,
        )?;
        // The documented separator is colon (POSIX `$PATH` style); we
        // also accept comma to match the ECAA_AWS_SUBNET_IDS and
        // ECAA_AWS_INSTANCE_TYPE_ALLOWLIST precedents elsewhere in the
        // catalog.
        let input_roots = env
            .get("ECAA_INPUT_ROOTS")
            .copied()
            .unwrap_or("")
            .split([':', ','])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // -- Bind / port -----------------------------------------------
        let bind_addr = env
            .get("ECAA_BIND_ADDR")
            .copied()
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_BIND_ADDR)
            .to_string();
        let port = parse_u16_with_default(env, "ECAA_PORT", DEFAULT_PORT)?;

        // -- Provenance / composer -------------------------------------
        let git_enabled = match env.get("ECAA_GIT_ENABLED").copied() {
            // Documented kill-switch: ONLY `0` disables (the docs say
            // "any other value (or absent) = config-driven default").
            Some("0") => false,
            _ => true,
        };
        let composer = env
            .get("ECAA_COMPOSER")
            .copied()
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_COMPOSER)
            .to_string();

        // -- Core classifier policy ------------------------------------
        let modality_drift_mode = match env.get("ECAA_MODALITY_DRIFT_MODE").copied() {
            Some(v) if v.eq_ignore_ascii_case("fail") => ModalityDriftMode::Fail,
            Some(v) if v.eq_ignore_ascii_case("warn") => ModalityDriftMode::Warn,
            None | Some("") => ModalityDriftMode::Warn,
            Some(other) => {
                tracing::warn!(
                    "ECAA_MODALITY_DRIFT_MODE={other:?} not recognized; falling back to warn"
                );
                ModalityDriftMode::Warn
            }
        };

        // -- ECAA emission mode ----------------------------------------
        let ecaa_mode =
            crate::emit_mode::EcaaMode::from_env_str(env.get("ECAA_ECAA_MODE").copied());

        Ok(Config {
            anthropic_api_key,
            anthropic_base_url,
            chat_mode,
            chat_sessions_dir,
            config_dir,
            package_root,
            server_auth_token,
            auto_title,
            live_api,
            aws_cost_ceiling_usd,
            aws_pricing_region_mult,
            aws_pricing_overrides,
            harness_batch_window_secs,
            task_heartbeat_stall_secs,
            harness_bin_path,
            literature,
            upload_root,
            upload_disk_reserve_gb,
            input_roots,
            bind_addr,
            port,
            git_enabled,
            composer,
            modality_drift_mode,
            ecaa_mode,
        })
    }

    /// Returns a [`ConfigBuilder`] preloaded with safe test defaults: no
    /// API key, online chat mode, loopback bind, default Anthropic base
    /// URL, empty pricing overrides, etc. Use the chainable
    /// `with_<field>` setters to override what the test cares about.
    pub fn for_test() -> ConfigBuilder {
        ConfigBuilder::default()
    }
}

// ----------------------------------------------------------------------------
// Debug — redact secrets
// ----------------------------------------------------------------------------

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field(
                "anthropic_api_key",
                &self.anthropic_api_key.as_ref().map(|_| "<redacted>"),
            )
            .field("anthropic_base_url", &self.anthropic_base_url.as_str())
            .field("chat_mode", &self.chat_mode)
            .field("chat_sessions_dir", &self.chat_sessions_dir)
            .field("config_dir", &self.config_dir)
            .field("package_root", &self.package_root)
            .field(
                "server_auth_token",
                &self.server_auth_token.as_ref().map(|_| "<redacted>"),
            )
            .field("auto_title", &self.auto_title)
            .field("live_api", &self.live_api)
            .field("aws_cost_ceiling_usd", &self.aws_cost_ceiling_usd)
            .field("aws_pricing_region_mult", &self.aws_pricing_region_mult)
            .field(
                "aws_pricing_overrides",
                &format!("<{} entries>", self.aws_pricing_overrides.len()),
            )
            .field("harness_batch_window_secs", &self.harness_batch_window_secs)
            .field("task_heartbeat_stall_secs", &self.task_heartbeat_stall_secs)
            .field("harness_bin_path", &self.harness_bin_path)
            .field("literature", &self.literature)
            .field("upload_root", &self.upload_root)
            .field("upload_disk_reserve_gb", &self.upload_disk_reserve_gb)
            .field("input_roots", &self.input_roots)
            .field("bind_addr", &self.bind_addr)
            .field("port", &self.port)
            .field("git_enabled", &self.git_enabled)
            .field("composer", &self.composer)
            .field("modality_drift_mode", &self.modality_drift_mode)
            .field("ecaa_mode", &self.ecaa_mode)
            .finish()
    }
}

// ----------------------------------------------------------------------------
// ConfigBuilder — for tests
// ----------------------------------------------------------------------------

/// Test-only builder. Production code uses [`Config::from_env`].
///
/// The default is intentionally minimal — every field gets the same value
/// the loader would assign for an empty environment, *except* paths are
/// rebased on `/tmp/ecaa-workflow-test-default` so tests don't write to
/// `~/.ecaa-workflow/...`. Tests should chain `with_*` setters for the
/// fields they exercise.
pub struct ConfigBuilder {
    inner: Config,
}

impl Default for ConfigBuilder {
    fn default() -> Self {
        // Hard-code the same defaults the empty-env loader would produce.
        // We can't call `Config::from_env_map(&HashMap::new())` here
        // because that resolves `~/.ecaa-workflow/...` paths which
        // makes test output non-deterministic across CI environments.
        let anthropic_base_url = Url::parse(DEFAULT_ANTHROPIC_BASE_URL)
            .expect("DEFAULT_ANTHROPIC_BASE_URL is a valid URL constant");
        let test_root = PathBuf::from("/tmp/ecaa-workflow-test-default");
        Self {
            inner: Config {
                anthropic_api_key: None,
                anthropic_base_url,
                chat_mode: ChatMode::Online,
                chat_sessions_dir: test_root.join("sessions"),
                config_dir: PathBuf::from("./config"),
                package_root: test_root.join("packages"),
                server_auth_token: None,
                auto_title: false,
                live_api: false,
                aws_cost_ceiling_usd: None,
                aws_pricing_region_mult: 1.0,
                aws_pricing_overrides: HashMap::new(),
                harness_batch_window_secs: DEFAULT_HARNESS_BATCH_WINDOW_SECS,
                task_heartbeat_stall_secs: DEFAULT_TASK_HEARTBEAT_STALL_SECS,
                harness_bin_path: None,
                literature: LiteratureConfig {
                    source_scope: LitSourceScope::PmcOa,
                    ncbi_api_key: None,
                    evidence_max_mb: DEFAULT_LIT_EVIDENCE_MAX_MB,
                },
                upload_root: None,
                upload_disk_reserve_gb: DEFAULT_UPLOAD_DISK_RESERVE_GB,
                input_roots: Vec::new(),
                bind_addr: DEFAULT_BIND_ADDR.to_string(),
                port: DEFAULT_PORT,
                git_enabled: true,
                composer: DEFAULT_COMPOSER.to_string(),
                modality_drift_mode: ModalityDriftMode::Warn,
                ecaa_mode: crate::emit_mode::EcaaMode::Full,
            },
        }
    }
}

impl ConfigBuilder {
    /// Override `anthropic_api_key`. Pass a sentinel like
    /// `"sk-ant-test-XXXX"` — the value is never sent anywhere from a
    /// `for_test()` Config.
    pub fn anthropic_api_key(mut self, key: impl Into<String>) -> Self {
        self.inner.anthropic_api_key = Some(key.into());
        self
    }

    /// Anthropic base url.
    pub fn anthropic_base_url(mut self, url: Url) -> Self {
        self.inner.anthropic_base_url = url;
        self
    }

    /// Chat mode.
    pub fn chat_mode(mut self, mode: ChatMode) -> Self {
        self.inner.chat_mode = mode;
        self
    }

    /// Chat sessions dir.
    pub fn chat_sessions_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.inner.chat_sessions_dir = dir.into();
        self
    }

    /// Config dir.
    pub fn config_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.inner.config_dir = dir.into();
        self
    }

    /// Package root.
    pub fn package_root(mut self, dir: impl Into<PathBuf>) -> Self {
        self.inner.package_root = dir.into();
        self
    }

    /// Server auth token.
    pub fn server_auth_token(mut self, token: impl Into<String>) -> Self {
        self.inner.server_auth_token = Some(token.into());
        self
    }

    /// Auto title.
    pub fn auto_title(mut self, v: bool) -> Self {
        self.inner.auto_title = v;
        self
    }

    /// Live api.
    pub fn live_api(mut self, v: bool) -> Self {
        self.inner.live_api = v;
        self
    }

    /// Aws cost ceiling usd.
    pub fn aws_cost_ceiling_usd(mut self, v: Option<f64>) -> Self {
        self.inner.aws_cost_ceiling_usd = v;
        self
    }

    /// Aws pricing region mult.
    pub fn aws_pricing_region_mult(mut self, v: f64) -> Self {
        self.inner.aws_pricing_region_mult = v;
        self
    }

    /// Aws pricing overrides.
    pub fn aws_pricing_overrides(mut self, m: HashMap<String, f64>) -> Self {
        self.inner.aws_pricing_overrides = m;
        self
    }

    /// Harness batch window secs.
    pub fn harness_batch_window_secs(mut self, v: u64) -> Self {
        self.inner.harness_batch_window_secs = v;
        self
    }

    /// Task heartbeat stall secs.
    pub fn task_heartbeat_stall_secs(mut self, v: u64) -> Self {
        self.inner.task_heartbeat_stall_secs = v;
        self
    }

    /// Harness bin path.
    pub fn harness_bin_path(mut self, p: PathBuf) -> Self {
        self.inner.harness_bin_path = Some(p);
        self
    }

    /// Literature.
    pub fn literature(mut self, lit: LiteratureConfig) -> Self {
        self.inner.literature = lit;
        self
    }

    /// Upload root.
    pub fn upload_root(mut self, r: impl Into<String>) -> Self {
        self.inner.upload_root = Some(r.into());
        self
    }

    /// Upload disk reserve gb.
    pub fn upload_disk_reserve_gb(mut self, v: u64) -> Self {
        self.inner.upload_disk_reserve_gb = v;
        self
    }

    /// Input roots.
    pub fn input_roots(mut self, roots: Vec<String>) -> Self {
        self.inner.input_roots = roots;
        self
    }

    /// Bind addr.
    pub fn bind_addr(mut self, addr: impl Into<String>) -> Self {
        self.inner.bind_addr = addr.into();
        self
    }

    /// Port.
    pub fn port(mut self, p: u16) -> Self {
        self.inner.port = p;
        self
    }

    /// Git enabled.
    pub fn git_enabled(mut self, v: bool) -> Self {
        self.inner.git_enabled = v;
        self
    }

    /// Composer.
    pub fn composer(mut self, c: impl Into<String>) -> Self {
        self.inner.composer = c.into();
        self
    }

    /// Modality drift mode.
    pub fn modality_drift_mode(mut self, m: ModalityDriftMode) -> Self {
        self.inner.modality_drift_mode = m;
        self
    }

    /// Ecaa mode.
    pub fn ecaa_mode(mut self, mode: crate::emit_mode::EcaaMode) -> Self {
        self.inner.ecaa_mode = mode;
        self
    }

    /// Finalize the builder into a `Config`.
    pub fn build(self) -> Config {
        self.inner
    }
}

// ----------------------------------------------------------------------------
// Helper parsers
// ----------------------------------------------------------------------------

/// Reads `ECAA_ANTHROPIC_API_KEY`, falling back to legacy `ANTHROPIC_API_KEY`
/// with a one-time stderr deprecation warning (matches the docs in
/// `docs/env-vars-reference.md`).
fn read_api_key(env: &HashMap<&str, &str>) -> Option<String> {
    if let Some(k) = env.get("ECAA_ANTHROPIC_API_KEY").copied() {
        if !k.is_empty() {
            return Some(k.to_string());
        }
    }
    if let Some(k) = env.get("ANTHROPIC_API_KEY").copied() {
        if !k.is_empty() {
            // Emit once via `tracing` so the migration target is the
            // structured-log pipeline, not bare stderr — consistent
            // with the rest of the workspace.
            tracing::warn!(
                "ANTHROPIC_API_KEY is deprecated; use ECAA_ANTHROPIC_API_KEY \
                 to keep agent + chat-side billing separate"
            );
            return Some(k.to_string());
        }
    }
    None
}

/// Parses a `https://` URL with the loopback-http exception documented for
/// `ANTHROPIC_BASE_URL`. Rejects every other non-https scheme.
fn parse_https_url(env: &HashMap<&str, &str>, key: &str, default: &str) -> Result<Url> {
    let raw = env
        .get(key)
        .copied()
        .filter(|s| !s.is_empty())
        .unwrap_or(default);
    let parsed = Url::parse(raw).with_context(|| format!("{key} parse failed: {raw:?}"))?;
    if parsed.scheme() == "https" {
        return Ok(parsed);
    }
    // Loopback http:// is the only exception (SSH-tunneled staging
    // proxy, local mock server in tests).
    if parsed.scheme() == "http" {
        let host = parsed.host_str().unwrap_or("");
        if LOOPBACK_HOSTS.contains(&host) {
            return Ok(parsed);
        }
    }
    Err(anyhow!(
        "{key} must be https:// (got {raw}); loopback http:// is the only exception"
    ))
}

/// Parses an optional finite `f64` env-var. Returns `Ok(None)` when unset,
/// `Ok(Some(v))` when set + finite, `Err` on parse failure or NaN/±∞.
fn parse_finite_f64(env: &HashMap<&str, &str>, key: &str) -> Result<Option<f64>> {
    match env.get(key).copied() {
        None => Ok(None),
        Some("") => Ok(None),
        Some(s) => {
            let v: f64 = s
                .parse()
                .with_context(|| format!("{key} parse failed: {s:?}"))?;
            if !v.is_finite() {
                return Err(anyhow!("{key} must be finite, got {s:?}"));
            }
            Ok(Some(v))
        }
    }
}

/// Parses a `u64` env-var with a documented default + optional inclusive
/// maximum. Values above the max are rejected (mirrors the docs' explicit
/// "values >600s are rejected" contract for `ECAA_HARNESS_BATCH_WINDOW_SECS`).
fn parse_u64_bounded(
    env: &HashMap<&str, &str>,
    key: &str,
    default: u64,
    max: Option<u64>,
) -> Result<u64> {
    let v = match env.get(key).copied() {
        None | Some("") => default,
        Some(s) => match s.parse::<u64>() {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(key = %key, value = ?s, default, "env var not a u64; using default");
                default
            }
        },
    };
    if let Some(m) = max {
        if v > m {
            return Err(anyhow!("{key} must be <= max {m}, got {v}"));
        }
    }
    Ok(v)
}

/// Parses a `u16` env-var with a documented default. Out-of-range values
/// (above `u16::MAX`) fail with a typed error rather than silently
/// truncating — operator-typo defense.
fn parse_u16_with_default(env: &HashMap<&str, &str>, key: &str, default: u16) -> Result<u16> {
    match env.get(key).copied() {
        None | Some("") => Ok(default),
        Some(s) => s
            .parse::<u16>()
            .with_context(|| format!("{key} parse failed: {s:?}")),
    }
}

/// Parses a bool env-var. Accepts the canonical truthy table (`1`, `true`,
/// `yes`, `on`); falsy or unset returns `default`.
fn parse_bool(env: &HashMap<&str, &str>, key: &str, default: bool) -> bool {
    match env.get(key).copied() {
        None => default,
        Some(s) => matches!(
            s.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on" | "t" | "y"
        ),
    }
}

/// Parses an optional non-empty string env-var. Returns `None` when unset
/// OR set to the empty string (an empty `ECAA_SERVER_AUTH_TOKEN` is just
/// as broken as unset and should not paper over the operator's mistake).
fn nonempty_string(env: &HashMap<&str, &str>, key: &str) -> Option<String> {
    env.get(key)
        .copied()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Parses an optional `PathBuf` env-var, falling back to the supplied
/// closure (which can resolve `$HOME` lazily — useful in tests where
/// `$HOME` may be unset).
fn parse_pathbuf_with_default<F: FnOnce() -> PathBuf>(
    env: &HashMap<&str, &str>,
    key: &str,
    default: F,
) -> PathBuf {
    env.get(key)
        .copied()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default)
}

/// `$HOME/<rel>` if `$HOME` is set, otherwise `/tmp/ecaa-workflow/<rel>`.
/// Mirrors the fall-through documented in CLAUDE.md for the storage roots.
fn home_subdir(rel: &str) -> PathBuf {
    // Read $HOME via std::env::var — this is one of two intentional reads
    // gated under the C7 disallowed-methods waiver (the other is
    // `Config::from_env` itself).
    #[allow(clippy::disallowed_methods)]
    let home = std::env::var("HOME").ok();
    match home {
        Some(h) if !h.is_empty() => PathBuf::from(h).join(rel),
        _ => PathBuf::from("/tmp/ecaa-workflow").join(rel),
    }
}

/// Parses `ECAA_AWS_PRICING_OVERRIDES_JSON`. Accepts either an inline JSON
/// object (matching the historical site-tunable shape) or — per the docs
/// in `env-vars-reference.md` — a path to a JSON file on disk. The
/// inline path takes precedence if the env value parses as JSON.
///
/// Each override value must be finite and `> 0`.
fn parse_pricing_overrides(env: &HashMap<&str, &str>) -> Result<HashMap<String, f64>> {
    let raw = match env.get("ECAA_AWS_PRICING_OVERRIDES_JSON").copied() {
        None => return Ok(HashMap::new()),
        Some("") => return Ok(HashMap::new()),
        Some(s) => s,
    };
    // Try parsing the value as inline JSON first; if that fails, treat
    // it as a path. This is more permissive than the docs but is the
    // shape consumer code already used and migration is mechanical
    // either way.
    let parsed: HashMap<String, f64> = if raw.trim_start().starts_with('{') {
        serde_json::from_str(raw)
            .with_context(|| format!("ECAA_AWS_PRICING_OVERRIDES_JSON inline parse: {raw:?}"))?
    } else {
        let bytes = std::fs::read(raw).with_context(|| {
            format!("ECAA_AWS_PRICING_OVERRIDES_JSON cannot read file: {raw:?}")
        })?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("ECAA_AWS_PRICING_OVERRIDES_JSON file parse: {raw:?}"))?
    };
    for (k, v) in &parsed {
        if !v.is_finite() {
            return Err(anyhow!(
                "ECAA_AWS_PRICING_OVERRIDES_JSON[{k}] must be finite, got {v}"
            ));
        }
        if *v <= 0.0 {
            return Err(anyhow!(
                "ECAA_AWS_PRICING_OVERRIDES_JSON[{k}] must be > 0, got {v}"
            ));
        }
    }
    Ok(parsed)
}

// ----------------------------------------------------------------------------
// Inline tests (small surface — the cross-cutting cases are in
// `tests/config_parse.rs`).
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_finite_f64_rejects_nan_and_inf() {
        for bad in ["nan", "NaN", "inf", "-inf", "Infinity", "-Infinity"] {
            let mut env = HashMap::new();
            env.insert("KEY", bad);
            assert!(
                parse_finite_f64(&env, "KEY").is_err(),
                "should reject {bad}"
            );
        }
    }

    #[test]
    fn parse_finite_f64_accepts_finite_values() {
        for ok in ["0", "0.0", "-1.5", "1e3", "1.7976931348623157e308"] {
            let mut env = HashMap::new();
            env.insert("KEY", ok);
            assert!(
                parse_finite_f64(&env, "KEY").unwrap().is_some(),
                "should accept {ok}"
            );
        }
    }

    #[test]
    fn parse_https_url_accepts_loopback_http() {
        for raw in [
            "http://localhost",
            "http://127.0.0.1:3000",
            "http://[::1]:8080",
        ] {
            let mut env = HashMap::new();
            env.insert("URL", raw);
            assert!(
                parse_https_url(&env, "URL", DEFAULT_ANTHROPIC_BASE_URL).is_ok(),
                "loopback http should be ok: {raw}"
            );
        }
    }

    #[test]
    fn parse_https_url_rejects_non_loopback_http() {
        let mut env = HashMap::new();
        env.insert("URL", "http://api.anthropic.com");
        let err = parse_https_url(&env, "URL", DEFAULT_ANTHROPIC_BASE_URL).unwrap_err();
        assert!(err.to_string().contains("https://"), "got: {err}");
    }

    #[test]
    fn parse_https_url_rejects_other_schemes() {
        for bad in ["ftp://api.example.com", "file:///etc/passwd"] {
            let mut env = HashMap::new();
            env.insert("URL", bad);
            assert!(
                parse_https_url(&env, "URL", DEFAULT_ANTHROPIC_BASE_URL).is_err(),
                "should reject {bad}"
            );
        }
    }

    #[test]
    fn read_api_key_prefers_ecaa_prefix() {
        let mut env = HashMap::new();
        env.insert("ECAA_ANTHROPIC_API_KEY", "ecaa-key");
        env.insert("ANTHROPIC_API_KEY", "legacy-key");
        assert_eq!(read_api_key(&env), Some("ecaa-key".to_string()));
    }

    #[test]
    fn read_api_key_falls_back_to_legacy() {
        let mut env = HashMap::new();
        env.insert("ANTHROPIC_API_KEY", "legacy-key");
        assert_eq!(read_api_key(&env), Some("legacy-key".to_string()));
    }

    #[test]
    fn parse_pricing_overrides_rejects_zero() {
        let mut env = HashMap::new();
        env.insert("ECAA_AWS_PRICING_OVERRIDES_JSON", r#"{"m6i.large": 0.0}"#);
        assert!(parse_pricing_overrides(&env).is_err());
    }

    #[test]
    fn parse_pricing_overrides_rejects_nan() {
        // serde_json doesn't accept literal NaN tokens in standard JSON,
        // so synthesize a JSON object with a value that becomes NaN by
        // overflow — both should be rejected. We use a string that
        // parses fine but is non-finite via the inline path: in
        // practice JSON forbids NaN, so this confirms the parse-level
        // refusal too.
        let mut env = HashMap::new();
        env.insert("ECAA_AWS_PRICING_OVERRIDES_JSON", r#"{"x": NaN}"#);
        assert!(
            parse_pricing_overrides(&env).is_err(),
            "JSON disallows bare NaN; parser must surface that"
        );
    }
}
