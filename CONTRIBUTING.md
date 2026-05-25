# Contributing

Thank you for helping improve `gewe-skill`.

## Development principles

- Keep callback normalization deterministic and evidence-preserving.
- Add new GeWe callback handling in `gewe-skill-core` first, then expose it through memory/client/CLI as needed.
- Do not add WeChat mutation APIs without an explicit safety design.
- Include redacted fixtures when documenting callback behavior.
- Keep commits atomic and use scoped Conventional Commit titles.
