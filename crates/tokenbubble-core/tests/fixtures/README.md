# Test fixtures

Synthetic, sanitized payloads only. Never add real auth files or captured responses containing
account identifiers.

`codex/usage_full.json` is adapted from the mock usage payload in CodexBar
`Tests/CodexBarTests/CodexOAuthRequestTests.swift` at commit
`d554d44931807e568471c5ff0d7f786b9c33053b` (MIT; see `THIRD_PARTY_NOTICES.md`), extended with
plan, credit, and spend-control fields documented in CodexBar's `CodexOAuthUsageFetcher.swift`.
