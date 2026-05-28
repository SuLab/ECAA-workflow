# Packer template for the base AMI the AwsExecutor provisions for
# each remote task. The AMI bakes in:
#   - scripts/agent-claude.sh + scripts/agent-claude-aws.sh
#   - scripts/run-task-on-instance.sh (the SSM wrapper)
#   - the Node.js runtime Claude Code needs + the aws + jq binaries
#   - the Nvidia drivers when building against a GPU base AMI
#
# Build with: packer init packer && packer build packer/scripps-agent.pkr.hcl
#
# The output AMI is tagged with the workspace's `make build` short SHA
# so the AwsExecutor can refuse to launch against an AMI that doesn't
# match the in-tree scripts. This prevents version skew between the
# harness's expectations and the baked-in wrapper.

packer {
  required_plugins {
    amazon = {
      version = ">= 1.3.0"
      source  = "github.com/hashicorp/amazon"
    }
  }
}

variable "aws_region" {
  type    = string
  default = "us-west-2"
}

variable "source_ami_owner" {
  type    = string
  default = "099720109477" # Canonical (Ubuntu)
}

# Ubuntu 22.04 LTS amd64 — refresh the filter for new releases.
variable "source_ami_filter_name" {
  type    = string
  default = "ubuntu/images/hvm-ssd/ubuntu-jammy-22.04-amd64-server-*"
}

variable "instance_type" {
  type    = string
  default = "t3.medium"
}

variable "ami_name_prefix" {
  type    = string
  default = "scripps-agent"
}

# Set by CI from `git rev-parse --short HEAD` so the AMI carries the
# workspace SHA that baked its scripts.
variable "workspace_sha" {
  type    = string
  default = "unknown"
}

source "amazon-ebs" "scripps-agent" {
  region        = var.aws_region
  instance_type = var.instance_type
  ami_name      = "${var.ami_name_prefix}-${var.workspace_sha}-{{ timestamp }}"
  ssh_username  = "ubuntu"

  source_ami_filter {
    owners      = [var.source_ami_owner]
    most_recent = true
    filters = {
      name                = var.source_ami_filter_name
      virtualization-type = "hvm"
      root-device-type    = "ebs"
    }
  }

  tags = {
    Name          = "scripps-agent-${var.workspace_sha}"
    WorkspaceSha  = var.workspace_sha
    BuiltBy       = "packer"
    Purpose       = "ecaa-workflow-agent"
  }
}

# Plan §S15.11 — runtime versions baked alongside WORKSPACE_SHA so the
# AwsExecutor can refuse a dispatch when the AMI's container-runtime
# version is older than what the task's `preferred_container` declares
# (`ECAA_AMI_RUNTIME_STRICT=1`, default). Ranges are chosen to match
# the production targets the round-2 + round-4 research called out:
#   - Apptainer 1.4.4 (Mar 2025) — preferred per Round-2 §3.10
#   - Docker 25.0+ as the secondary container runtime
#   - Podman as the rootless fallback
# The `RUNTIME_VERSIONS.json` file is read by the runtime-version
# probe in `crates/harness/src/executor/aws/provisioning.rs` at the
# AMI-validation step (S15.11 / `BlockerKind::AmiRuntimeSkew`).
variable "apptainer_version" {
  type    = string
  default = "1.4.4"
}

variable "docker_version" {
  type    = string
  default = "25.0"
}

build {
  name = "scripps-agent"

  sources = ["source.amazon-ebs.scripps-agent"]

  # Stage the in-tree scripts the harness shells out to.
  provisioner "file" {
    source      = "../scripts/agent-claude.sh"
    destination = "/tmp/agent-claude.sh"
  }
  provisioner "file" {
    source      = "../scripts/agent-claude-aws.sh"
    destination = "/tmp/agent-claude-aws.sh"
  }
  provisioner "file" {
    source      = "../scripts/run-task-on-instance.sh"
    destination = "/tmp/run-task-on-instance.sh"
  }

  provisioner "shell" {
    inline = [
      "set -euo pipefail",
      "sudo DEBIAN_FRONTEND=noninteractive apt-get update",
      "sudo DEBIAN_FRONTEND=noninteractive apt-get install -y ca-certificates curl gnupg lsb-release jq unzip",
      # Node.js 20 (required by @anthropic-ai/claude-code)
      "curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -",
      "sudo DEBIAN_FRONTEND=noninteractive apt-get install -y nodejs",
      # AWS CLI v2
      "curl 'https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip' -o /tmp/awscliv2.zip",
      "cd /tmp && unzip -q awscliv2.zip && sudo ./aws/install",
      # SSM agent is already on the Ubuntu AMIs but make sure it's enabled.
      "sudo snap install amazon-ssm-agent --classic || true",
      "sudo systemctl enable --now snap.amazon-ssm-agent.amazon-ssm-agent.service || true",

      # Plan §S15.11 — install Apptainer (preferred container runtime
      # per Round-2 §3.10). Ubuntu's apt repo trails upstream so we
      # build from the released .deb to pin the version we declare in
      # RUNTIME_VERSIONS.json.
      "APPTAINER_VER='${var.apptainer_version}'",
      "curl -fsSL -o /tmp/apptainer.deb \"https://github.com/apptainer/apptainer/releases/download/v$${APPTAINER_VER}/apptainer_$${APPTAINER_VER}_amd64.deb\"",
      "sudo DEBIAN_FRONTEND=noninteractive apt-get install -y /tmp/apptainer.deb || true",

      # Docker 25+ as the secondary runtime. The harness's runtime
      # probe order is apptainer → docker → podman per S15.4; both
      # land here so the agent script's auto-detect chain has options.
      "curl -fsSL https://get.docker.com -o /tmp/get-docker.sh",
      "sudo sh /tmp/get-docker.sh",
      "sudo systemctl enable --now docker",
      "sudo usermod -aG docker ubuntu || true",

      # Podman as the rootless fallback (no daemon).
      "sudo DEBIAN_FRONTEND=noninteractive apt-get install -y podman",

      # Place the scripps scripts in /opt/scripps — the SSM RunCommand
      # wrapper reaches into this path.
      "sudo mkdir -p /opt/scripps/bin",
      "sudo install -m 755 /tmp/agent-claude.sh /opt/scripps/bin/agent-claude.sh",
      "sudo install -m 755 /tmp/agent-claude-aws.sh /opt/scripps/bin/agent-claude-aws.sh",
      "sudo install -m 755 /tmp/run-task-on-instance.sh /opt/scripps/bin/run-task-on-instance.sh",

      # Stamp the workspace SHA so the AwsExecutor can verify AMI ↔ code match.
      "echo '${var.workspace_sha}' | sudo tee /opt/scripps/WORKSPACE_SHA",

      # Plan §S15.11 — stamp the runtime versions baked into this AMI
      # so `AwsExecutor::select_instance_type` can refuse a dispatch
      # whose `preferred_container` requires a newer runtime than what
      # this AMI ships. The probe reads /opt/scripps/RUNTIME_VERSIONS.json
      # and compares against the workspace's minimum-runtime policy
      # (config/compute-profiles/runtime-policy.yaml — pending S15.11
      # follow-up). Mismatch surfaces `BlockerKind::AmiRuntimeSkew` when
      # `ECAA_AMI_RUNTIME_STRICT=1` (default); strict=0 downgrades to
      # a stderr warning.
      "APPTAINER_INSTALLED=$(apptainer --version 2>/dev/null | awk '{print $NF}' | head -n1 || true)",
      "DOCKER_INSTALLED=$(docker --version 2>/dev/null | awk '{print $3}' | tr -d ',' | head -n1 || true)",
      "PODMAN_INSTALLED=$(podman --version 2>/dev/null | awk '{print $NF}' | head -n1 || true)",
      "echo \"{\\\"apptainer\\\": \\\"$${APPTAINER_INSTALLED:-}\\\", \\\"docker\\\": \\\"$${DOCKER_INSTALLED:-}\\\", \\\"podman\\\": \\\"$${PODMAN_INSTALLED:-}\\\", \\\"baked_at\\\": \\\"$(date -Iseconds)\\\", \\\"workspace_sha\\\": \\\"${var.workspace_sha}\\\"}\" | sudo tee /opt/scripps/RUNTIME_VERSIONS.json",
    ]
  }
}
