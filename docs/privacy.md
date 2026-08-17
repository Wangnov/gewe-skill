# Privacy

`gewe-skill` is designed for private personal or team infrastructure. It is not a hosted service.

## Data classes

- Raw callback JSON: source evidence, sensitive.
- Normalized messages: Agent-readable index, sensitive.
- Attachments: potentially large and highly sensitive.
- Chatroom events: derived metadata, still sensitive.

## Defaults

- Edge data should be short-lived.
- Memory data is the durable user-controlled store.
- Agent access should be read-only unless the user authorizes a specific mutation workflow.
- Raw callbacks are stored and synchronized verbatim, without field redaction or length
  truncation, so source evidence remains complete.
- D1, R2, admin export credentials, and the durable memory service must therefore remain inside
  the user's trusted security boundary.
