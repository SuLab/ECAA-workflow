# Shared-server deployment (rootless Podman + Quadlet + Caddy)

1. `cp deploy/quadlet/ecaa-workflow.container ~/.config/containers/systemd/`
2. `mkdir -p ~/.config/ecaa && printf 'ECAA_SERVER_AUTH_TOKEN=%s\n' "$(openssl rand -hex 32)" > ~/.config/ecaa/ecaa.env`
   (add `ECAA_ANTHROPIC_API_KEY=...` for LLM chat)
3. `loginctl enable-linger $USER`   # rootless units start at boot only with linger
4. `systemctl --user daemon-reload && systemctl --user start ecaa-workflow`
5. Front with Caddy: set `ECAA_SERVER_AUTH_TOKEN` in Caddy's env and run `caddy run --config deploy/Caddyfile`.
6. Auto-update: `systemctl --user enable --now podman-auto-update.timer`.

Fallback (no systemd): `compose.yaml` with `restart: unless-stopped` behind Caddy.
