# GeWe callback notes

This project currently treats V1 as the preferred callback mode for complete group state observation.

## V1 strengths

- Captures `ModContacts` chatroom snapshots.
- Captures SYSTEM XML for invite, remove, and group-name changes.
- Can normalize events into a V2-style schema.

## V2 strengths

- Cleaner message shape.
- Better selected-message filtering.

## Known design choice

Use V1 for ingest completeness, then normalize to the shared `gewe-skill-types` schema.
