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

The service listens on `127.0.0.1:8788` by default. Put Caddy or another reverse proxy in front of it for HTTPS.

Do not expose `/write/*` without a strong bearer token. Agent-facing `/api/*` should use a separate read token.
