# Architecture

```text
GeWe
  -> gewe-skill-edge
  -> Cloudflare D1/R2 short-term buffer
  -> gewe-skill-memory
  -> Agent read-only API
  -> gewe-skill
```

`gewe-skill-edge` is intentionally small. It receives callbacks, keeps a short-term buffer, downloads attachments, and forwards durable records to `gewe-skill-memory`.

`gewe-skill-memory` is the long-lived memory layer. It stores normalized messages, chatroom events, raw callback archives, and attachment metadata for Agent use.
