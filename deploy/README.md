# Deploy

| Tier | How | Docs |
|---|---|---|
| Workstation | `deploy/ecaa up` (compose; container-runtime-only) | repo README "Operate" |
| Shared server | rootless Podman Quadlet + Caddy | `deploy/README-shared-server.md` |
| Cloud (AWS) | server image on EC2, or Fargate + delegated executor | `deploy/cloud/README.md` |
| HPC | login-node orchestrator + Apptainer tasks | `deploy/apptainer/README.md` |

Security: the Docker socket is host-root-equivalent — prefer rootless Podman (the launcher
auto-detects it). `~/.claude` is mounted read-only; Anthropic credentials are always operator-supplied.

## Networking & auth (workstation)

The workstation `compose.yaml` uses `network_mode: host` with `ECAA_BIND_ADDR=127.0.0.1`.
The server binds the host's loopback directly, which it treats as trusted-local — so the full
`/api` works with **no auth token**, exactly like running the binary on the host. This is
deliberate: port-publishing would force a `0.0.0.0` bind inside the container, which the server
hard-requires a bearer token for, and the browser UI does not send one.

- **Linux:** works as-is; open `http://127.0.0.1:3000`.
- **macOS / Windows Docker Desktop:** host networking is limited. Either enable Docker Desktop's
  host-networking support, or run the shared-server posture (port-publish + `ECAA_SERVER_AUTH_TOKEN`
  behind Caddy, which injects the token). Plain port-publishing without a token will start but the
  auth middleware rejects every `/api` request.

The **shared-server** tier does the opposite on purpose: it binds off-loopback behind Caddy and
sets `ECAA_SERVER_AUTH_TOKEN` (Caddy injects it), so a multi-user host is authenticated.
