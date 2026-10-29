# Third-party notices

## CodexBar

TokenBubble's Codex and Claude credential handling, usage requests, and response mapping
are adapted from CodexBar at commit `d554d44931807e568471c5ff0d7f786b9c33053b`
(<https://github.com/steipete/CodexBar>). Codex test fixtures under
`crates/tokenbubble-core/tests/fixtures/codex/` are adapted from CodexBar's test payloads;
Claude fixtures are synthetic and follow its documented response shapes.

```text
MIT License

Copyright (c) 2026 Peter Steinberger

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Provider artwork

The embedded `crates/tokenbubble-gpui/assets/codex.png` and `claude.svg` are unmodified
`Brand-ProviderIcon-codex.png` and `Brand-ProviderIcon-claude.svg` from the same pinned
CodexBar commit. Their provenance and primary brand sources are recorded in
[CodexBar's provider artwork notes](https://github.com/steipete/CodexBar/blob/d554d44931807e568471c5ff0d7f786b9c33053b/docs/provider-brand-icons.md).
The Codex image is OpenAI's product artwork; the Claude mark uses Anthropic's documented
orange (`#D97757`). Product names and marks remain the property of their respective owners.

The Windows bubbles emoji is rendered from the user's installed Segoe UI Emoji font.
TokenBubble does not redistribute the font or an extracted emoji asset.
