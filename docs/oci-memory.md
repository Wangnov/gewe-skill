# OCI memory deployment

Recommended target layout:

```text
/opt/gewe-skill-memory
├─ bin/gewe-skill-memory
├─ bin/gewe-skill
├─ config/gewe-skill-memory.env
└─ data/gewe-skill-memory.sqlite
```

Systemd unit template:

```text
crates/gewe-skill-memory/deploy/gewe-skill-memory.service
```

Recommended sync and maintenance timers:

```text
crates/gewe-skill-memory/deploy/gewe-skill-edge-sync.timer
crates/gewe-skill-memory/deploy/gewe-skill-edge-attachment-sync.timer
crates/gewe-skill-memory/deploy/gewe-skill-edge-chatroom-event-sync.timer
crates/gewe-skill-memory/deploy/gewe-skill-identity-refresh.timer
crates/gewe-skill-memory/deploy/gewe-skill-identity-event-backfill.timer
```

Install or update the service from GitHub Release assets with:

```bash
sudo GEWE_SKILL_VERSION=v0.1.18 scripts/install-oci-memory-release.sh
```

The installer preserves `config/gewe-skill-memory.env`, installs the memory service, and enables the recommended timers.

The service listens on `127.0.0.1:8788` by default. Put Caddy or another reverse proxy in front of it for HTTPS.

Do not expose `/write/*` without a strong bearer token. Agent-facing `/api/*` should use a separate read token.
