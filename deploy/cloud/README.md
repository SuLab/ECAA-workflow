# Cloud deployment (AWS)

## Default — server image on plain EC2
Run the server image with compose/Quadlet on an EC2 instance that has a local Docker/Podman
runtime. This keeps the local `docker run bio-min` task path working. Put `$HOME/.ecaa-workflow`
on an EBS volume for persistent state. Front with Caddy for TLS (see shared-server README).

## Fallback — ECS/Fargate + delegated executor
Fargate provides NO Docker daemon and forbids privileged mode, so the server CANNOT launch task
containers locally there. Set `ECAA_EXECUTOR_MODE=aws` (or `slurm`) so task compute is delegated
to the existing executor. Mount EFS for state (many small SessionStore writes are latency-sensitive
on EFS).

## Task-executor AMI (retained, unchanged)
`packer/scripps-agent.pkr.hcl` builds the AWS TASK-EXECUTOR AMI (Node + apptainer/docker/podman +
agent-claude*.sh, stamped WORKSPACE_SHA) that AwsExecutor provisions. It is the compute substrate,
NOT the server. Keep it; the server is the OCI image above.
