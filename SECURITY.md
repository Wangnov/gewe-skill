# Security Policy

`gewe-skill` handles private WeChat data. Treat all callback payloads, tokens, attachments, and message databases as sensitive.

## Rules

- Never commit `.dev.vars`, `.env`, SQLite databases, raw callback files, downloaded attachments, or bearer tokens.
- Use separate read and write tokens for `gewe-skill-memory`.
- Expose write endpoints only to trusted infrastructure such as `gewe-skill-edge`.
- Keep Agent-facing usage read-only by default.
- Report security issues privately instead of opening public issues with payload samples.
