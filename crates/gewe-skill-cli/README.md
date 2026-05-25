# gewe-skill-cli

Operations CLI and universal Agent command surface for `gewe-skill`.

Examples:

```bash
gewe-skill health
gewe-skill recent --limit 20
gewe-skill conversations --limit 50
gewe-skill attachments --limit 20
gewe-skill attachment-download --sha256 '<sha256>' --output /tmp/gewe-attachment.bin
gewe-skill normalize --file callback.json --received-at 2026-05-26T00:00:00.000Z
```
