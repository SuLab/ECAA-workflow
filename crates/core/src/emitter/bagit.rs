//! BagIt 1.0-style manifest emission for the package surface
//! (`manifest-sha512.txt` at the package root).
//!
//! Held to the deterministic surface contract documented in
//! CLAUDE.md §Deterministic output: the audit logs + affordance
//! sidecars are excluded because they're intentionally not part of
//! the byte-reproducibility baseline.
//!
//! The package follows the RFC 8493 BagIt tag-file CONVENTIONS but is
//! NOT a fully RFC 8493-conformant bag: the payload is manifested at the
//! package root rather than under a `data/` payload directory (RFC 8493
//! §2.1.2 requires the payload to live in `data/`). Relocating the
//! payload under `data/` would break every reader of the flat package
//! layout (`runtime/outputs/`, `inputs/`, `evidence/`), the RO-Crate
//! `@id` paths, the WRROC `mainEntity` paths, and the conformance
//! fixtures, so it is DEFERRED rather than faked. What IS provided to
//! spec: SHA-512 payload manifest with a correct Payload-Oxum, and the
//! three tag files below.
//!
//! Three tag files sit alongside `manifest-sha512.txt`:
//! - `bagit.txt`  declares BagIt version + tag-file encoding,
//! - `bag-info.txt` carries Source-Organization, External-Description,
//!   Bagging-Date (from the `&dyn Clock` so emits stay byte-identical),
//!   and Payload-Oxum (`<octet-count>.<stream-count>` of the payload),
//! - `tagmanifest-sha512.txt` covers the three tag files above so a
//!   downstream verifier can detect tag-file tampering independently
//!   of the payload manifest.

use anyhow::{anyhow, Context, Result};
use rayon::prelude::*;

/// Directory names that hold version-control internals or build/runtime
/// caches: their contents are transient (and, for `.git`, often thousands
/// of loose objects/refs) and must never be checksummed into the payload
/// manifest. A directory whose FINAL path component is one of these is
/// skipped at any depth, by both the Emit and Reseal walks. Skipping these
/// keeps the manifest a description of the package payload — not of a
/// checked-out VCS working copy that happens to sit under the package root.
const VCS_TRANSIENT_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".bzr",
    "CVS",
    "node_modules",
    "__pycache__",
    ".ipynb_checkpoints",
];

/// Whether the manifest is being written at emit time (skeleton — outputs
/// don't exist yet) or re-sealed after execution (outputs ARE part of the
/// at-rest audit surface and must be hashed). Emit keeps `runtime/outputs/`
/// out so the emit byte-reproducibility baseline is untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SealMode {  // pub(crate) = visible to emitter/mod.rs and ro_crate.rs
    Emit,
    Reseal,
}

/// Walk every file in `dir` recursively, compute SHA-512 of each, and
/// write `manifest-sha512.txt` at the package root in BagIt 1.0
/// Format: `<hex-sha512> <relative/path>` per line. Excludes the
/// manifest itself + the audit logs (which aren't yet on disk at
/// this call site).
///
/// Iteration order is sorted by relative path so the manifest itself
/// is byte-deterministic across runs.
pub(super) fn write_bagit_manifest(
    dir: &std::path::Path,
    clock: &dyn crate::clock::Clock,
) -> Result<()> {
    write_bagit_manifest_with_mode(dir, clock, SealMode::Emit)
}

pub(super) fn write_bagit_manifest_with_mode(
    dir: &std::path::Path,
    clock: &dyn crate::clock::Clock,
    mode: SealMode,
) -> Result<()> {
    let mut entries: Vec<std::path::PathBuf> = Vec::new();
    walk_for_manifest(dir, dir, mode, &mut entries)?;
    entries.sort();
    // Compute Payload-Oxum (sum of payload byte counts + entry count)
    // while we walk. Per RFC 8493 §2.2.2, Payload-Oxum is the octet
    // count + "." + stream count over the bag's payload — for our
    // bag-shape that's every file we hash into manifest-sha512.txt.
    // SHA-512 each payload file in parallel — entries are already
    // sorted, so the manifest assembly below walks `entries` + `hashes`
    // in lockstep and the output bytes stay byte-identical to the
    // serial version. `size` is `Option<u64>`: the original loop
    // skipped the Payload-Oxum count when metadata-fetch failed, and
    // we preserve that exactly via `None` (no octet/stream increment).
    let hashes: Vec<(String, Option<u64>)> = entries
        .par_iter()
        .map(|rel| {
            let abs = dir.join(rel);
            let hex = stream_sha512_hex(&abs)
                .with_context(|| format!("hashing {} for manifest", abs.display()))?;
            let size = std::fs::metadata(&abs).ok().map(|m| m.len());
            Ok::<_, anyhow::Error>((hex, size))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut payload_octets: u64 = 0;
    let mut payload_streams: u64 = 0;
    let mut out = String::new();
    for (rel, (hex, size)) in entries.iter().zip(hashes.iter()) {
        if let Some(s) = size {
            payload_octets = payload_octets.saturating_add(*s);
            payload_streams = payload_streams.saturating_add(1);
        }
        // BagIt 1.0 §2.1 — `<checksum><whitespace><filepath>`. Two
        // spaces is conventional; relative path uses POSIX-style
        // separators regardless of host OS.
        out.push_str(hex);
        out.push_str("  ");
        out.push_str(&rel.to_string_lossy().replace('\\', "/"));
        out.push('\n');
    }
    // Atomic write (.tmp + fsync + rename + parent fsync) — the
    // manifest is the byte-reproducibility anchor for the package, so
    // a crash mid-write must never leave a partial manifest behind.
    let manifest_path = dir.join("manifest-sha512.txt");
    crate::fs_helpers::atomic_write_bytes_sync(&manifest_path, out.as_bytes())
        .context("writing manifest-sha512.txt")?;

    // R4.17 — write the BagIt declaration + bag-info tag files. The
    // declaration is fixed-content per RFC 8493 §2.1.1.
    let bagit_txt = "BagIt-Version: 1.0\nTag-File-Character-Encoding: UTF-8\n";
    let bagit_path = dir.join("bagit.txt");
    std::fs::write(&bagit_path, bagit_txt).context("writing bagit.txt")?;

    // RFC 8493 §2.2.2 — `Bagging-Date` is human-meaningful.
    //
    // At EMIT we are writing the byte-reproducible skeleton (outputs don't
    // exist yet), so the date must be deterministic — NOT the wall clock and
    // NOT the opaque hash-derived `emit_clock` (which can map into the far
    // future, e.g. 2061). We anchor it to the genuine RUN epoch
    // (`SOURCE_DATE_EPOCH`, via `run_epoch_clock`), the same run-level value
    // `ro-crate-metadata.json::dateCreated` is anchored to, so the two are
    // CONSISTENT. When no run epoch is present `run_epoch_clock` falls back
    // to the stable `2026-01-01` base, so two emits of the same run stay
    // byte-identical and the prior baseline is preserved.
    //
    // At RESEAL (post-execution finalize) the package is NO LONGER part of
    // the byte-reproducibility baseline — it is the at-rest record of an
    // actual run — and the caller already threads the REAL run clock
    // (`finalize.rs` / conformance repair pass `WallClock`). C2: use that
    // real clock here so the finalized bag carries the genuine run date
    // rather than a frozen 2026-01-01 placeholder.
    let bagging_date = match mode {
        SealMode::Emit => {
            use crate::clock::Clock as _;
            crate::clock::run_epoch_clock()
                .now()
                .format("%Y-%m-%d")
                .to_string()
        }
        SealMode::Reseal => clock.now().format("%Y-%m-%d").to_string(),
    };
    let bag_info = format!(
        "Source-Organization: Scripps Research\n\
         External-Description: ecaa-workflow emitted RO-Crate package\n\
         Bagging-Date: {bagging_date}\n\
         Payload-Oxum: {payload_octets}.{payload_streams}\n",
    );
    let bag_info_path = dir.join("bag-info.txt");
    std::fs::write(&bag_info_path, &bag_info).context("writing bag-info.txt")?;

    // RFC 8493 §2.2.1 — tag manifest covers the tag files themselves so
    // downstream verifiers can detect tampering with bagit.txt /
    // bag-info.txt / manifest-sha512.txt independently of the payload
    // manifest. Order: same lexicographic sort as the payload manifest.
    let mut tag_entries: Vec<(&str, &std::path::Path)> = vec![
        ("bag-info.txt", bag_info_path.as_path()),
        ("bagit.txt", bagit_path.as_path()),
        ("manifest-sha512.txt", manifest_path.as_path()),
    ];
    tag_entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut tag_manifest = String::new();
    for (rel_name, abs) in &tag_entries {
        let hex = stream_sha512_hex(abs)
            .with_context(|| format!("hashing {} for tag manifest", abs.display()))?;
        tag_manifest.push_str(&hex);
        tag_manifest.push_str("  ");
        tag_manifest.push_str(rel_name);
        tag_manifest.push('\n');
    }
    std::fs::write(dir.join("tagmanifest-sha512.txt"), tag_manifest)
        .context("writing tagmanifest-sha512.txt")?;

    Ok(())
}

/// Stream-hash a file with SHA-512 over a `BufReader` in 64 KB
/// chunks. The streaming pattern bounds the hasher's working set by
/// the chunk buffer (64 KB) regardless of file size; a `fs::read` +
/// `hasher.update(&bytes)` pattern would allocate the entire file
/// into memory and balloon peak RSS on emit for large evidence
/// tables under `evidence/` or `inputs/`.
fn stream_sha512_hex(path: &std::path::Path) -> Result<String> {
    use sha2::{Digest, Sha512};
    use std::io::Read;
    const CHUNK_BYTES: usize = 64 * 1024;
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut reader = std::io::BufReader::with_capacity(CHUNK_BYTES, file);
    let mut hasher = Sha512::new();
    let mut buf = [0u8; CHUNK_BYTES];
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("reading {} chunk", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Verify a sealed BagIt payload manifest: recompute the SHA-512 of every file
/// listed in `manifest-sha512.txt` and confirm it matches the recorded hex. A
/// deposit is manifest-valid iff the manifest exists, every listed file is
/// present + readable, and every checksum matches. Returns `Ok(false)` (not an
/// error) on any integrity failure so callers can record a `bagit: fail`
/// attestation rather than aborting. Line format mirrors the writer:
/// `<hex-sha512><whitespace><relative/posix/path>`.
pub(crate) fn verify_manifest(dir: &std::path::Path) -> Result<bool> {
    let manifest_path = dir.join("manifest-sha512.txt");
    let Ok(manifest) = std::fs::read_to_string(&manifest_path) else {
        // No manifest at all → not a valid bag.
        return Ok(false);
    };
    for line in manifest.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((hex, rel)) = line.split_once(char::is_whitespace) else {
            return Ok(false);
        };
        let rel = rel.trim();
        let abs = dir.join(rel);
        match stream_sha512_hex(&abs) {
            Ok(actual) if actual.eq_ignore_ascii_case(hex.trim()) => {}
            // Missing/unreadable manifested file, or checksum mismatch.
            _ => return Ok(false),
        }
    }
    Ok(true)
}

/// Recursively collect every file under `current` (relative to `root`),
/// excluding the manifest itself and, depending on `mode`, paths under
/// `runtime/outputs/` (agent-written artifacts; excluded at emit because
/// outputs don't exist yet, included on reseal so produced DE tables and
/// figures are part of the at-rest audit surface).
fn walk_for_manifest(
    root: &std::path::Path,
    current: &std::path::Path,
    mode: SealMode,
    out: &mut Vec<std::path::PathBuf>,
) -> Result<()> {
    for entry in
        std::fs::read_dir(current).with_context(|| format!("read_dir {}", current.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .map_err(|e| anyhow!("strip_prefix failed for {}: {}", path.display(), e))?
            .to_path_buf();
        // Skip the manifest itself and ephemeral agent-write paths.
        if rel == std::path::Path::new("manifest-sha512.txt") {
            continue;
        }
        // R4.17 — BagIt tag files are covered by tagmanifest-sha512.txt,
        // not the payload manifest. Excluding them keeps the payload
        // manifest stable (writing tagmanifest mutates dir AFTER the
        // payload walk) and matches RFC 8493 §2.2.1.
        if rel == std::path::Path::new("bagit.txt")
            || rel == std::path::Path::new("bag-info.txt")
            || rel == std::path::Path::new("tagmanifest-sha512.txt")
        {
            continue;
        }
        // Emit: outputs don't exist yet → always excluded (byte-repro
        // baseline). Reseal: outputs ARE the at-rest evidence surface → hash
        // them so a reviewer can verify any reported number against hashed
        // data. LOG.jsonl stays excluded both ways (append-only run log,
        // not part of the integrity surface). task-spec.json under outputs
        // is deterministic and now covered on reseal.
        if rel.starts_with("runtime/LOG.jsonl") {
            continue;
        }
        if mode == SealMode::Emit && rel.starts_with("runtime/outputs") {
            continue;
        }
        // FACET-1 — three runtime evidence sidecars carry content whose REAL
        // value only exists AFTER a run and is non-deterministic BEFORE one, so
        // they are covered in the DEPOSIT (reseal) manifest but excluded from
        // the pre-run EMIT skeleton:
        //   - `verifier-decisions.jsonl`: the conversation emit pipeline
        //     overwrites the core-written empty file with a DESTRUCTIVE
        //     once-per-process drain of the compose-time substrate buffer, so an
        //     in-process re-emit of the same session drains ~1.5MB then finds it
        //     empty (0 bytes);
        //   - `validation-summary.json`: the conversation validator overwrites it
        //     with a real wall-clock `duration_ms` whose digit-width varies;
        //   - `audit-proof-report.json`: its verdicts range over the two files
        //     above, so it differs downstream.
        // Manifesting them at EMIT leaked that non-determinism into Payload-Oxum
        // + `manifest-sha512.txt`, breaking the emit byte-reproducibility
        // baseline. The correct invariant is "covered in the deposit; excluded
        // from the pre-run emit manifest" — RESEAL (finalize / export / the
        // deposit gate's re-verify input surface) still hashes all three. The
        // deterministic DR-4 evidence files (`claim-verification.json`,
        // `validation-reports.jsonl`, `reexecution.json`, `proofs.jsonl`,
        // `decisions.jsonl`, `assumptions.jsonl`, `security-policy.json`) stay
        // manifested at EMIT.
        if mode == SealMode::Emit
            && (rel == std::path::Path::new("runtime/verifier-decisions.jsonl")
                || rel == std::path::Path::new("runtime/validation-summary.json")
                || rel == std::path::Path::new("runtime/audit-proof-report.json"))
        {
            continue;
        }
        // P3-4 — per-task verification sidecars are written by the
        // conversation emit pipeline AFTER `emit_package` returns, and
        // are runtime-only artifacts consumed by the
        // `GET /task/:task_id/result` handler. Excluded from the
        // byte-reproducibility baseline alongside the audit logs.
        if rel.starts_with("runtime/verification-reports") {
            continue;
        }
        // Runtime audit/ECAA sidecars kept OFF the payload manifest.
        //
        // DR-4 — the deposit-integrity envelope MANIFESTS the substantive
        // DETERMINISTIC evidence sidecars instead of excluding them:
        // `decisions.jsonl`, `proofs.jsonl`, `assumptions.jsonl`,
        // `security-policy.json`, `claim-verification.json`,
        // `validation-reports.jsonl`, `reexecution.json`,
        // `coverage-statement.json`, `plot_affordances.jsonl`, and
        // `intake-conversation.jsonl` are covered at BOTH emit and reseal. A
        // deposit consumer needs these integrity-covered; they were the
        // present-on-disk-but-unmanifested evidence files in the `611cf5ee`
        // deposit. `crates/conversation/src/emit/mod.rs`'s emit pipeline calls
        // `emitter::reseal_emit_manifest` as its LAST step (after every
        // sidecar overwrite AND the final RO-Crate patch), so the manifest
        // entry for each always covers its truly-final bytes. Do NOT
        // re-exclude them to "fix" a staleness symptom — fix the seal-order
        // violation at its source instead.
        //
        // FACET-1 — the three NON-deterministic-at-emit evidence sidecars
        // (`verifier-decisions.jsonl`, `validation-summary.json`,
        // `audit-proof-report.json`) are handled by the `SealMode::Emit`-gated
        // block above: excluded from the pre-run emit skeleton, covered on
        // RESEAL so the deposit (and DR-4 re-verify input surface) still hashes
        // them.
        //
        // The remaining exclusions below are NOT integrity-bearing evidence:
        //   - `determinism-shim.json` — a HOST-VARYING diagnostic env capture
        //     (locale/timezone/seed policy + the applied-policy env-var names).
        //     Its bytes differ by compiler host and are refreshed at finalize
        //     by `merge_container_env`, so it is NOT a re-verify input and
        //     manifesting it would break cross-host byte-reproducibility. It is
        //     surfaced instead as an RO-Crate `@graph` CreativeWork, like
        //     `DEPOSIT-READINESS.json`.
        //   - `ed-cf-*` — re-emitted by the conversation path with the live
        //     `Tool::COUNT` (differs from the core baseline), informational.
        //   - `catalog-coverage-statement.json` / `policy-decisions.jsonl` /
        //     `affordance_fallbacks.jsonl` — post-manifest informational
        //     sidecars overwritten with no following reseal.
        //   - `decisions.jsonl.mac` — a keyed HMAC over `decisions.jsonl`;
        //     verified with the session secret, NOT by re-hashing into the
        //     payload manifest, and non-reproducible across sessions.
        if rel == std::path::Path::new("runtime/determinism-shim.json")
            || rel == std::path::Path::new("runtime/ed-cf-self-assessment.json")
            || rel == std::path::Path::new("runtime/ed-cf-delta.json")
            || rel == std::path::Path::new("runtime/catalog-coverage-statement.json")
            || rel == std::path::Path::new("runtime/policy-decisions.jsonl")
            || rel == std::path::Path::new("runtime/decisions.jsonl.mac")
            || rel == std::path::Path::new("runtime/affordance_fallbacks.jsonl")
            // Serialized package ABox from the conformance external-validator
            // path (project_package.py), only produced under
            // ECAA_CONFORMANCE_MODE; an external, non-deterministic artifact
            // kept out of the byte-reproducibility baseline like the other
            // post-manifest sidecars.
            || rel == std::path::Path::new("package.ttl")
            // Deposit-readiness attestation: written by `export` AFTER the final
            // reseal (Layer 1 self-validation) and updated post-export by the
            // re-execution check (Layer 2). Carries a wall-clock `verified_at` +
            // a verdict computed at export time, so it is intentionally a
            // mutable meta file off the byte-reproducibility manifest (it is
            // instead represented as an RO-Crate `@graph` entity — DR-11).
            || rel == std::path::Path::new("DEPOSIT-READINESS.json")
        {
            continue;
        }
        if path.is_dir() {
            // C1 — skip VCS internals + build/runtime caches at ANY depth.
            // Their contents are transient and (for `.git`) can be thousands
            // of loose objects, none of which belong in the payload manifest.
            // Match on the directory's FINAL component so a `.git` nested
            // arbitrarily deep is still excluded.
            let is_transient = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|name| VCS_TRANSIENT_DIRS.contains(&name))
                .unwrap_or(false);
            if is_transient {
                continue;
            }
            walk_for_manifest(root, &path, mode, out)?;
        } else if path.is_file() {
            out.push(rel);
        }
    }
    Ok(())
}

/// Reusable: rel-path -> (sha512_hex, size_bytes) for every manifest-eligible
/// payload file. Shares the walk + streaming hash used by the manifest writer.
pub(crate) fn payload_hashes(
    dir: &std::path::Path,
    mode: SealMode,
) -> std::io::Result<std::collections::BTreeMap<String, (String, u64)>> {
    let mut entries = Vec::new();
    walk_for_manifest(dir, dir, mode, &mut entries)
        .map_err(std::io::Error::other)?;
    entries.sort();
    let mut out = std::collections::BTreeMap::new();
    for rel in entries {
        let abs = dir.join(&rel);
        let hex = stream_sha512_hex(&abs).map_err(std::io::Error::other)?;
        let size = std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
        out.insert(rel.to_string_lossy().replace('\\', "/"), (hex, size));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha512};
    use std::io::Write;

    /// R2-N15 — stream hash must produce the exact same digest as the
    /// previous `fs::read` + `hasher.update(&bytes)` path so the
    /// manifest stays byte-reproducible. Test across three sizes that
    /// straddle the 64 KB chunk boundary: < chunk, == chunk - 1,
    /// > multiple chunks.
    #[test]
    fn stream_sha512_matches_in_memory_across_chunk_boundaries() {
        for size in [1024usize, 64 * 1024 - 1, 200 * 1024] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("blob-{size}.bin"));
            // Deterministic payload so a regression in the chunking
            // loop produces a stable diff.
            let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            {
                let mut f = std::fs::File::create(&path).unwrap();
                f.write_all(&payload).unwrap();
            }
            let expected = {
                let mut h = Sha512::new();
                h.update(&payload);
                format!("{:x}", h.finalize())
            };
            let actual = stream_sha512_hex(&path).unwrap();
            assert_eq!(actual, expected, "size={size}");
        }
    }

    /// Empty-file edge case — the streaming loop terminates on the
    /// first zero-length read, producing the empty-payload digest.
    #[test]
    fn stream_sha512_handles_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.bin");
        std::fs::File::create(&path).unwrap();
        let empty: &[u8] = &[];
        let expected = {
            let mut h = Sha512::new();
            h.update(empty);
            format!("{:x}", h.finalize())
        };
        let actual = stream_sha512_hex(&path).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn reseal_includes_runtime_outputs_emit_excludes() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("runtime/outputs/de")).unwrap();
        std::fs::write(tmp.path().join("runtime/outputs/de/de_results.tsv"), b"gene\tpadj\n").unwrap();
        std::fs::write(tmp.path().join("root.txt"), b"x").unwrap();
        let clk = crate::clock::FrozenClock::default();

        write_bagit_manifest_with_mode(tmp.path(), &clk, SealMode::Emit).unwrap();
        let emit_m = std::fs::read_to_string(tmp.path().join("manifest-sha512.txt")).unwrap();
        assert!(!emit_m.contains("runtime/outputs/de/de_results.tsv"), "emit must exclude outputs");

        write_bagit_manifest_with_mode(tmp.path(), &clk, SealMode::Reseal).unwrap();
        let reseal_m = std::fs::read_to_string(tmp.path().join("manifest-sha512.txt")).unwrap();
        assert!(reseal_m.contains("runtime/outputs/de/de_results.tsv"), "reseal must include outputs");
    }

    /// C1 twin — the manifest walk must EXCLUDE VCS internals
    /// (`.git/`) and build/runtime caches (`node_modules/`) at any
    /// depth while still hashing a real payload file, the Payload-Oxum
    /// must count only the real payload, and a tampered payload file
    /// must STILL break sha512 verification (the integrity guarantee is
    /// preserved, not weakened, by the exclusion).
    #[test]
    fn manifest_excludes_vcs_transient_dirs_but_hashes_payload() {
        let tmp = tempfile::tempdir().unwrap();
        // A real payload file that MUST be manifested.
        std::fs::write(tmp.path().join("payload.txt"), b"real payload\n").unwrap();
        // VCS internals at top level + nested deep — both must be skipped.
        std::fs::create_dir_all(tmp.path().join(".git/refs/heads")).unwrap();
        std::fs::write(tmp.path().join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
        std::fs::write(tmp.path().join(".git/refs/heads/main"), b"deadbeef\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("runtime/outputs/de/node_modules/pkg")).unwrap();
        std::fs::write(
            tmp.path().join("runtime/outputs/de/node_modules/pkg/index.js"),
            b"module.exports = {}\n",
        )
        .unwrap();
        // A nested __pycache__ to prove final-component matching at depth.
        std::fs::create_dir_all(tmp.path().join("runtime/outputs/de/__pycache__")).unwrap();
        std::fs::write(
            tmp.path().join("runtime/outputs/de/__pycache__/m.pyc"),
            b"\x00\x01",
        )
        .unwrap();
        // A real output file alongside the cache so reseal still hashes it.
        std::fs::write(
            tmp.path().join("runtime/outputs/de/de_results.tsv"),
            b"gene\tpadj\n",
        )
        .unwrap();
        let clk = crate::clock::FrozenClock::default();

        write_bagit_manifest_with_mode(tmp.path(), &clk, SealMode::Reseal).unwrap();
        let manifest =
            std::fs::read_to_string(tmp.path().join("manifest-sha512.txt")).unwrap();

        assert!(
            manifest.contains("payload.txt"),
            "real payload must be manifested:\n{manifest}"
        );
        assert!(
            manifest.contains("runtime/outputs/de/de_results.tsv"),
            "real output must be manifested on reseal:\n{manifest}"
        );
        assert!(
            !manifest.contains(".git/"),
            ".git internals must be excluded:\n{manifest}"
        );
        assert!(
            !manifest.contains("node_modules"),
            "node_modules must be excluded:\n{manifest}"
        );
        assert!(
            !manifest.contains("__pycache__"),
            "__pycache__ must be excluded:\n{manifest}"
        );

        // Payload-Oxum must count ONLY the two real payload files
        // (payload.txt = 13 bytes, de_results.tsv = 10 bytes), stream
        // count 2 — never the excluded VCS/cache files.
        let info = std::fs::read_to_string(tmp.path().join("bag-info.txt")).unwrap();
        assert!(
            info.contains("Payload-Oxum: 23.2"),
            "Payload-Oxum must count only real payload (23 octets / 2 streams):\n{info}"
        );

        // Integrity is PRESERVED: tampering with a manifested payload
        // file makes the recorded sha512 no longer match the file on disk.
        let before = {
            let line = manifest
                .lines()
                .find(|l| l.ends_with("payload.txt"))
                .expect("payload.txt line present");
            line.split_whitespace().next().unwrap().to_string()
        };
        std::fs::write(tmp.path().join("payload.txt"), b"TAMPERED\n").unwrap();
        let after = stream_sha512_hex(&tmp.path().join("payload.txt")).unwrap();
        assert_ne!(
            before, after,
            "tampering a manifested payload file must change its sha512 (verification still breaks)"
        );
    }

    /// At EMIT the bag is the byte-reproducible skeleton, so Bagging-Date
    /// must stay pinned to the EPOCH_2026 base and must NOT leak the
    /// hash-derived far-future emit clock.
    #[test]
    fn bagging_date_is_pinned_not_hash_derived() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), b"hi").unwrap();
        // A FrozenClock at a far-future instant simulates the hash-derived
        // emit clock that produced "2061-05-20".
        let future = crate::clock::FrozenClock {
            at: chrono::TimeZone::timestamp_opt(&chrono::Utc, 3_000_000_000, 0)
                .single()
                .unwrap(),
        };
        write_bagit_manifest(tmp.path(), &future).unwrap();
        let info = std::fs::read_to_string(tmp.path().join("bag-info.txt")).unwrap();
        assert!(
            info.contains("Bagging-Date: 2026-01-01"),
            "expected pinned date, got:\n{info}"
        );
    }

    #[test]
    fn payload_hashes_returns_sha512_and_size_for_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        let map = payload_hashes(dir.path(), SealMode::Reseal).unwrap();
        let (hex, size) = map.get("a.txt").expect("a.txt hashed");
        assert_eq!(*size, 5);
        assert_eq!(hex.len(), 128); // sha512 hex
    }

    /// C2 twin — at RESEAL (post-execution finalize) the package is the
    /// at-rest record of a real run, no longer on the byte-repro
    /// baseline, and the caller threads the REAL run clock. Bagging-Date
    /// must then be the genuine run date, NOT the frozen 2026-01-01
    /// placeholder. (The reseal callers — finalize.rs / conformance
    /// repair — pass `WallClock`; here we pass a fixed real-ish date so
    /// the assertion is deterministic.)
    #[test]
    fn reseal_bagging_date_is_real_run_date_not_pinned() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), b"hi").unwrap();
        // 2026-06-21T00:00:00Z — a plausible real run date distinct from
        // the EPOCH_2026 (2026-01-01) pin, so a regression that reverts to
        // the pin is caught.
        let run_clock = crate::clock::FrozenClock {
            at: chrono::TimeZone::timestamp_opt(&chrono::Utc, 1_782_000_000, 0)
                .single()
                .unwrap(),
        };
        write_bagit_manifest_with_mode(tmp.path(), &run_clock, SealMode::Reseal).unwrap();
        let info = std::fs::read_to_string(tmp.path().join("bag-info.txt")).unwrap();
        assert!(
            info.contains("Bagging-Date: 2026-06-21"),
            "reseal must stamp the real run date, got:\n{info}"
        );
        assert!(
            !info.contains("Bagging-Date: 2026-01-01"),
            "reseal must NOT reuse the EPOCH_2026 emit pin:\n{info}"
        );
    }
}
