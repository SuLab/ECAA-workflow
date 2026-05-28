# Packer AMI template for `scripps-agent`

This directory builds the base AMI that `AwsExecutor::provision`
launches for each remote task. The AMI bakes in:

- `scripts/agent-claude.sh` + `scripts/agent-claude-aws.sh`
- Node.js 20 + AWS CLI v2 + `jq` + the SSM agent
- `scripts/run-task-on-instance.sh` — the SSM RunCommand wrapper

## Why a custom AMI at all

Every remote task launches on a fresh instance — if the AMI is a
stock Ubuntu image the task spends 2-5 minutes on apt-get before
doing any real work. A baked AMI cuts cold-start to the time it
takes EC2 to boot the instance (~30-60 s) and makes tasks
deterministic: the harness knows exactly what's installed.

## Building

```bash
# One-time: install Packer (https://www.packer.io/downloads)
# then init the plugin dependencies
packer init packer/

# Build the AMI. Set workspace_sha to the current HEAD short SHA so
# the AMI carries the code it was built against.
packer build \
  -var "workspace_sha=$(git rev-parse --short HEAD)" \
  packer/scripps-agent.pkr.hcl
```

Packer writes the resulting AMI id to `manifest.json` in the working
directory. Record it as `ECAA_AWS_AMI_ID` in the operator's
`~/.scripps/config.env` (or pass via the harness's `--ami-id` flag).

## GPU variant

When building for GPU-backed stages (DeepVariant, AlphaFold) swap
the `source_ami_filter_name` for the NVIDIA GPU-optimized Deep
Learning AMI and bump `instance_type` to a G-family shape — the
provisioner steps unconditionally install Node + AWS CLI on top, so
the same template works for both.

## AMI tag contract

The AMI inherits three tags the harness reads:

| Tag | Meaning |
|---|---|
| `Name` | `scripps-agent-<workspace-sha>` — for human-readable EC2 console |
| `WorkspaceSha` | The `git rev-parse --short HEAD` value at build time |
| `BuiltBy` | Always `packer` — lets IAM scope launch permissions |

The `AwsExecutor` refuses to launch an AMI whose `WorkspaceSha`
doesn't match the running harness binary's SHA (embedded via `build.rs`
at cargo build time). This prevents version skew: a harness talking
to an older AMI would pass it a prompt the baked scripts don't
understand.

## Related components

- `AwsExecutor::provision` — consumes `ECAA_AWS_AMI_ID`
- `scripts/run-task-on-instance.sh` — SSM RunCommand wrapper baked into this AMI
- spot launch + `CapacityRebalance` — consumes `ECAA_AWS_SPOT` via `executor::spot_policy`
- multi-AZ failover — consumes `ECAA_AWS_SUBNET_IDS`
