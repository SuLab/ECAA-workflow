//! BagIt 1.0-style manifest emission for the package surface
//! (`manifest-sha512.txt` at the package root).
//!
//! Held to the deterministic surface contract documented in
//! CLAUDE.md §Deterministic output: the audit logs + affordance
//! sidecars are excluded because they're intentionally not part of
//! the byte-reproducibility baseline.
//!
//! The package is RFC 8493 BagIt-spec-compliant. Three tag files
//! sit alongside `manifest-sha512.txt`:
//! - `bagit.txt`  declares BagIt version + tag-file encoding,
//! - `bag-info.txt` carries Source-Organization, External-Description,
//!   Bagging-Date (from the `&dyn Clock` so emits stay byte-identical),
//!   and Payload-Oxum (`<octet-count>.<stream-count>` of the payload),
//! - `tagmanifest-sha512.txt` covers the three tag files above so a
//!   downstream verifier can detect tag-file tampering independently
//!   of the payload manifest.

use anyhow::{anyhow, Context, Result};
use rayon::prelude::*;

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
    let mut entries: Vec<std::path::PathBuf> = Vec::new();
    walk_for_manifest(dir, dir, &mut entries)?;
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

    // RFC 8493 §2.2.2 — `Bagging-Date` is a yyyy-mm-dd date. The
    // `&dyn Clock` keeps this byte-identical across two emits of the
    // same intake (FrozenClock derived from the intake hash).
    let bagging_date = clock.now().format("%Y-%m-%d").to_string();
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
}

/// Recursively collect every file under `current` (relative to `root`),
/// excluding the manifest itself and any path under `runtime/outputs/`
/// (those are agent-written artifacts; the harness emits a separate
/// `tag-manifest-sha512.txt` for them when execution completes).
fn walk_for_manifest(
    root: &std::path::Path,
    current: &std::path::Path,
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
        if rel.starts_with("runtime/outputs") || rel.starts_with("runtime/LOG.jsonl") {
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
        // Skip runtime audit/ECAA sidecars and affordance sidecars.
        // Core emits its placeholder ECAA sidecars after the BagIt
        // manifest, and the conversation emit path may overwrite them
        // with richer session-derived records after core emit_package
        // returns. Keeping all of them out of the payload manifest
        // prevents stale checksums on live emits while preserving the
        // byte-reproducibility baseline.
        if rel == std::path::Path::new("runtime/intake-conversation.jsonl")
            || rel == std::path::Path::new("runtime/decisions.jsonl")
            || rel == std::path::Path::new("runtime/proofs.jsonl")
            || rel == std::path::Path::new("runtime/claim-verification.json")
            || rel == std::path::Path::new("runtime/verifier-decisions.jsonl")
            || rel == std::path::Path::new("runtime/assumptions.jsonl")
            || rel == std::path::Path::new("runtime/validation-reports.jsonl")
            || rel == std::path::Path::new("runtime/determinism-shim.json")
            || rel == std::path::Path::new("runtime/security-policy.json")
            || rel == std::path::Path::new("runtime/audit-proof-report.json")
            || rel == std::path::Path::new("runtime/validation-summary.json")
            || rel == std::path::Path::new("runtime/policy-decisions.jsonl")
            || rel == std::path::Path::new("runtime/decisions.jsonl.mac")
            || rel == std::path::Path::new("runtime/plot_affordances.jsonl")
            || rel == std::path::Path::new("runtime/affordance_fallbacks.jsonl")
        {
            continue;
        }
        if path.is_dir() {
            walk_for_manifest(root, &path, out)?;
        } else if path.is_file() {
            out.push(rel);
        }
    }
    Ok(())
}
