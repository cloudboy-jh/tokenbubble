# TokenBubble implementation plan

**Goal:** Build TokenBubble, a CodexBar-style account-wide subscription meter in Rust/GPUI, embedded in Compi and runnable independently from a Windows tray or macOS menu-bar icon.

**Architecture:** One UI-independent collection library and one shared GPUI panel. Compi consumes them as optional client-side functionality; a separate executable hosts the same panel and its own native tray/menu-bar lifecycle. No usage data or credentials enter the terminal daemon or process protocol.

**Tech stack:** Rust 2024; match Compi's pinned GPUI packages (`gpui-pre` / `gpui-pre-platform` 0.3.8, `gpui-component` 0.7.1). Select an HTTP client and native tray integration after the bounded verification steps below, not by assumption.

**Build order:** Standalone Windows app first, Compi integration second, native macOS qualification afterward. Get one real subscription working end to end before expanding providers.

## Current version (2026-10-29)

- Standalone Windows tray app with a shared GPUI panel, settings, manual/background refresh, and selectable Codex/Claude providers. The selection is saved; switching clears the old provider's display, ignores outdated requests, and preserves separate rate-limit cooldowns.
- The Windows tray icon is the installed Segoe UI Emoji bubbles glyph, rendered in native color at the taskbar's icon size. Usage remains in the panel and tooltip instead of changing the icon into bars. Windows may place it under Show Hidden Icons.
- Codex and Claude have embedded provider artwork in the panel header and selector, plus the settings source section. Claude uses its documented orange accent (`#D97757`); Codex keeps the existing accent and its blue/purple product artwork.
- Run `cargo dev` from this repository in Windows PowerShell to build, launch, and rebuild on saved changes. Click the notification-area icon to open the panel; `q` then Enter stops the development runner. For a single launch, run `cargo run --locked -p tokenbubble-gpui --bin tokenbubble`.
- Collection is read-only and uses the native CLI sign-in: Codex honors `CODEX_HOME` (default `~/.codex`); Claude honors `CLAUDE_CONFIG_DIR` (default `~/.claude`, `.credentials.json`). A Windows launch does not read WSL credentials. Neither collector refreshes tokens or changes CLI files.
- One-shot commands: `cargo run --locked -p tokenbubble-core --bin tokenbubble-collect -- codex` or the same command ending in `claude`. Output contains usage or a classified error, never tokens.
- Claude reports session, weekly, and supplied model/feature limits. Extra usage remains separate, with cents converted to major currency units. Missing identity or quota values are not invented.
- Verified native Codex and Claude readings, notification icon publication, second-launch reveal, Escape dismissal without quitting, reopen, click-away, provider selection persistence, and explicit quit. Native screenshots also verified the bubbles emoji, both provider icons, orange Claude styling, and the settings source section. The expired-sign-in state was verified before the stored Claude login was renewed.
- Native macOS Keychain behavior, multi-monitor/DPI changes, WSL source selection, and Compi integration remain unverified or unimplemented.
- Windows workspace tests and Clippy pass. The original planning sections below describe the intended broader product, not completed work.

## Installing and releasing Windows

- The Windows prerelease version is `0.1.0-beta.1`. Build its ZIP with `powershell -ExecutionPolicy Bypass -File tools/package-windows.ps1` from the repository root using native Windows Rust.
- Packages are written under `target/release-packages/`: a versioned Windows x64 ZIP, `SHA256SUMS.txt`, and release notes. The ZIP contains only `tokenbubble.exe`, `INSTALL.txt`, and `THIRD_PARTY_NOTICES.md`; provider images are embedded in the executable.
- Keep an installed copy outside the checkout, for example `%LOCALAPPDATA%\Programs\TokenBubble`. Launch that executable directly. Update by quitting the tray app, replacing the executable with a verified release, and relaunching. Settings remain in `%APPDATA%\TokenBubble\settings.toml`; CLI credentials remain owned by their CLIs.
- Autostart remains opt-in. Enable it from the installed copy, or toggle it off/on there if an older development path was registered. Release builds have no development console and do not need Rust or `cargo dev` to run.
- `.github/workflows/release-windows.yml` builds tags matching `v*` on Windows with Rust 1.99.0. It requires the tag to match the workspace version, checks formatting/tests/Clippy, runs the same packaging script, and publishes the ZIP and checksum. Tags containing `-` are published as prereleases.
- To publish the next version, update `[workspace.package].version` and the workspace package entries in `Cargo.lock`, commit and push, then push the matching `v<version>` tag. Verify the workflow and download the release package before replacing an installed copy.
- An existing unpublished tag can also be built explicitly with `gh workflow run release-windows.yml --ref main -f tag=v0.1.0-beta.1`. This leaves the tag unchanged and uses that tag's source.
- These builds are unsigned; Windows may show a SmartScreen warning. No installer or automatic updater is included.

## Inspection and limits

- Compi upstream and WSL HEAD: `919942a0e2e066f45f774464be52f31d0b53a5c3`.
- Inspected working source: `/home/thatmfer/proj/compi`, including substantial uncommitted changes and an untracked `crates/compi-protocol/src/program_status.rs`. Implementation must reread touched files and preserve those changes. Do not replace the tree with upstream source.
- CodexBar reference: `d554d44931807e568471c5ff0d7f786b9c33053b`.
- The original planning pass was source inspection only; subsequent runtime evidence is recorded above.
- Current Compi roadmap still puts OSC 7501 next. This is an independent feature plan, not authorization to reorder that work.
- Codex and Claude are the proposed first collectors, not a claim about the user's complete subscription list. Add other requested providers after inspecting their actual source and auth paths.

## Scope and boundaries

First delivery: account/plan identity where known, separate session/weekly/model quota windows, usage bars, reset countdowns, freshness/error states, provider selection, manual refresh, background polling, provider/source settings.

Credits and extra spending are separate monetary fields, never folded into subscription quota. Show only values actually supplied by the selected collector. Do not add API billing collectors, charts, browser-cookie import, app-owned OAuth login, automated CLI refresh commands, quota redemption, or multi-account management as prerequisites.

Compi remains terminal- and harness-agnostic. The meter is account-wide, not inferred from pane output or process names. Selecting a remote workspace must not silently switch the meter's credential environment.

`docs/dev/Spec.md:1013` excludes embedded model providers and credential management. Add a narrow clarification if implementation is authorized: optional read-only subscription telemetry is client-side, cannot invoke models or manage logins, and does not alter process-hosting scope.

## Proposed files

Product name: **TokenBubble**. Binary: `tokenbubble`. Two reusable crates: `tokenbubble-core` for collection and `tokenbubble-gpui` for the shared panel and standalone host.

Build these in the standalone TokenBubble workspace first. The following file layout is proposed, not existing implementation; the Compi adapter comes later:

```text
crates/tokenbubble-core/Cargo.toml
crates/tokenbubble-core/src/{lib,model,service,config,transport,source}.rs
crates/tokenbubble-core/src/providers/{mod,codex,claude}.rs
crates/tokenbubble-core/tests/{model,codex,claude,credentials,scheduler,transport}.rs
crates/tokenbubble-core/tests/fixtures/                  # sanitized, attributed source fixtures
crates/tokenbubble-gpui/Cargo.toml
crates/tokenbubble-gpui/src/{lib,panel,presentation,theme}.rs
crates/tokenbubble-gpui/src/bin/tokenbubble.rs
crates/tokenbubble-gpui/src/standalone/{mod,windows,macos}.rs
crates/tokenbubble-gpui/tests/presentation.rs
crates/compi-client/src/gui/workspace/usage.rs
```

Existing integration points:

- TokenBubble `Cargo.toml`: declare the two workspace members. Later, add reusable library dependencies to Compi without moving existing Compi crate code or changing its protocol/daemon/client split. Use local path dependencies during integration development; decide distribution of the shared libraries when needed.
- `crates/compi-client/Cargo.toml`: target-gated dependencies on the usage libraries.
- `crates/compi-client/src/gui.rs:135-228`: app-level service initialization; retain ordinary Compi close/quit behavior.
- `crates/compi-client/src/gui.rs:841`: `CompiApp` subscribes to shared usage state; no per-window fetch loops.
- `crates/compi-client/src/gui/workspace.rs:5267-5583`: header layout and entry-point rendering. Include corresponding tab drag hit-testing and narrow-window metrics/tests, not just a button insertion.
- `crates/compi-client/src/gui/workspace/settings.rs`: entry into the shared provider settings; no second provider settings implementation.
- `docs/dev/Spec.md`: the explicit scope clarification above, only during authorized implementation.

Do not change `compi-protocol`, `compi-daemon`, terminal snapshots, or wire fixtures for this feature. Do not reuse daemon-scoped `window_host.rs` ownership for the standalone application.

## Task 1: Prove collection before polishing UI

**Files:** new core manifest, `source.rs`, `transport.rs`, provider modules and tests.

1. Write credential parsing/request construction tests first using sanitized fixtures, never personal auth-file dumps.
2. Implement Codex and Claude read-only credential adapters and one-shot normalized collection.
3. Exercise each against an explicitly selected, consenting real source in the intended environment; output only normalized usage/errors. If authentication or endpoint access fails, report it rather than manufacturing success.
4. Identify which providers the user actually wants before expanding the collector list.

Windows host and WSL are different credential environments. Do not assume `%USERPROFILE%` represents WSL `$HOME`. Start with an explicit native/WSL source selection. For WSL, run a small collector helper in the chosen distro which loads credentials and performs HTTP there, returning only bounded normalized telemetry. Reuse the core collector in that helper rather than copying secrets to Windows or building a parallel parser. Do not inspect or run agents to discover credentials during planning.

Codex: honor `CODEX_HOME`; native `auth.json` access token and account scope. Usage endpoint follows scoped `chatgpt_base_url`, default `https://chatgpt.com/backend-api/wham/usage`. Match account headers and parsing semantics in the pinned fetcher. Do not redeem refresh tokens or rewrite shared auth files; expired credentials produce an actionable owner-login/refresh state.

Claude: read CLI-owned credentials with the platform-specific adapter; macOS may require Keychain access, while Linux/WSL uses a file. Preserve milliseconds for credential expiry. Verify required usage scope and pinned request headers. Usage endpoint in the inspected source is `https://api.anthropic.com/api/oauth/usage?cedar_ember=1`, with a documented-in-source compatibility retry. Do not mistake MCP credentials for subscription auth.

**Completion:** real normalized quota data from at least the intended first provider/source, plus passing parser/auth boundary tests. Runtime provider availability remains unverified until this step succeeds.

## Task 2: Shared model and refresh service

**Files:** core model/service/config modules and corresponding tests.

Use a snapshot keyed by provider, selected environment, and account scope. Include measurement time, quota windows with stable names/IDs, optional duration/reset, optional identity, separate credits/spend, and explicit fresh/stale/auth-required/unavailable/error states. Credentials never serialize into snapshots/config/cache/debug output.

Test unknown vs zero, missing/reset-only windows, swapped primary/secondary windows, over-quota readings, malformed optional extras, and account changes before implementing normalization. Keep raw percentages; clamp only the visual projection. Never infer that a window is session merely because it is first.

Start with a five-minute default and manual refresh, not the full adaptive power/activity machinery. Coalesce overlapping requests, enforce HTTP/body bounds and timeouts, honor rate-limit cooldowns, cancel on shutdown, and use bounded retry/backoff. Redraw countdowns locally only while visible; countdown ticks do not trigger network requests.

Transient failures retain same-account data with its original measurement timestamp. Logout, credential-source changes, or account changes invalidate the old display before collection begins. No automatic browser import, source fallback to another account, authentication redirects carrying secrets, or logging raw HTTP bodies.

**Completion:** fake-clock scheduler and isolated mock-transport tests pass. Unit tests use explicit fixtures and never load developer credentials.

## Task 3: Build the reusable GPUI panel

**Files:** GPUI library manifest/panel/presentation/theme and presentation tests.

Create one compact provider switcher and account/plan header; named quota bars with numerical labels/reset countdowns; a distinct optional credits/spend section; freshness/error row; horizontal refresh/settings actions. Preserve unavailable states rather than drawing empty healthy bars.

Pass a small presentation theme from Compi. Standalone supplies its own theme using the same component, not a dependency on the whole Compi client. Use real provider assets only after checking their source/redistribution terms. No invented account values in screenshots or acceptance claims.

Test view-model formatting, reset timezone handling, unknown readings, account-switch clearing and over-quota labels before implementing the rendering. Exercise keyboard navigation, Escape, focus return, scaling and narrow widths natively.

**Completion:** panel displays the verified collector result; mock state tests cover loading/stale/auth failures without claiming mock data as live.

## Task 4: Standalone Windows tray host

**Files:** standalone binary and Windows host module first; macOS host module after Compi integration.

First run a bounded native integration spike: create the system icon, receive click events through the GPUI event loop, show/anchor/dismiss a GPUI popup, and quit cleanly. Search of locally installed GPUI 0.3.8 source found no `TrayIcon`, `NSStatusBar`, or `Shell_NotifyIcon` implementation; this is not a proven built-in feature.

Choose an existing tray crate only after verifying compatibility with the GPUI event loop. Otherwise use a thin native shell (`Shell_NotifyIconW` / `NSStatusItem`) and keep all panel content GPUI. Do not introduce a second renderer or UI event loop just for a tray menu.

Standalone hides/closes its popup without quitting; explicit Quit stops collection and removes the icon. Add a per-user standalone instance guard so repeated launch reveals the existing popup. Do not start/connect the Compi daemon. Initially each host owns its own service instance; simultaneous Compi + standalone polling is an acknowledged tradeoff, not a reason to add a new daemon/IPC protocol now.

Autostart is opt-in. TokenBubble packaging is separate from the existing Compi installer; decide distribution once its native behavior works.

**Completion:** native Windows tray smoke checks, including multiple monitors/DPI, click-away, reopen, explicit quit, and no terminal daemon dependency. Once this works, integrate the same panel into Compi.

**After Compi integration:** implement and qualify the macOS menu-bar host using a real bundled app. Cross-compilation alone does not qualify macOS.

## Task 5: Embed in Compi

**Files:** the existing client integration points listed above and the new `workspace/usage.rs` adapter.

Attach one service per GPUI application, shared across Compi windows. Add one optional compact header indicator opening the shared panel; reserve its geometry consistently in header rendering, tab drop targets, hit testing, and narrow-window tests. Hide/collapse the indicator instead of squeezing tabs or window controls.

The panel is a small anchored popover, not a permanent pane or full-window settings overlay. Return focus to the previous terminal/editor on dismissal. Fetching must not block startup or input/render paths. Closing Compi stops its meter polling but does not affect shells, quota, or the independent standalone app.

**Completion:** multiple windows share readings and one fetch loop; missing credentials leave Compi fully usable; no wire/daemon change.

## Verification commands and acceptance

Run in the standalone TokenBubble workspace after the proposed crates exist:

```sh
cargo test -p tokenbubble-core
cargo test -p tokenbubble-gpui
cargo fmt --all -- --check
cargo clippy -p tokenbubble-core -p tokenbubble-gpui --all-targets -- -D warnings
cargo run -p tokenbubble-gpui --bin tokenbubble
```

After integration, run separately in the Compi checkout:

```sh
cargo test -p compi-client
cargo fmt --all -- --check
cargo clippy -p compi-client --all-targets -- -D warnings
```

Use native Windows/macOS for GPUI commands. Verify/build the core WSL helper separately with the real WSL toolchain; do not assume Rust is installed there. Existing unrelated baseline failures must be reported separately, never fixed by weakening tests.

Ship claims require: actual provider data; correct reset windows; stale/auth failure honesty; no secrets in persistent telemetry/logs; responsive Compi with unavailable credentials; matching panel behavior in both hosts; standalone running without Compi. This planning pass establishes none of those runtime claims.

## Reference source and attribution

Pinned source base:
`https://github.com/steipete/CodexBar/tree/d554d44931807e568471c5ff0d7f786b9c33053b`

Inspect these files again before implementing because private quota APIs and auth formats can change:

- `Sources/CodexBarCore/Providers/Codex/CodexOAuth/CodexOAuthCredentials.swift`
- `Sources/CodexBarCore/Providers/Codex/CodexOAuth/CodexOAuthUsageFetcher.swift`
- `Sources/CodexBarCore/Providers/Codex/CodexAuthenticatedHTTPTransport.swift`
- `Sources/CodexBarCore/Providers/Claude/ClaudeOAuth/ClaudeOAuthCredentials.swift`
- `Sources/CodexBarCore/Providers/Claude/ClaudeOAuth/ClaudeOAuthUsageFetcher.swift`
- `Sources/CodexBarCore/UsageFetcher.swift`
- `docs/codex-oauth.md`, `docs/claude.md`, `docs/refresh-loop.md`
- `Tests/CodexBarTests/CodexOAuthCredentialReadTests.swift`
- `Tests/CodexBarTests/CodexOAuthRequestTests.swift`
- `TestsLinux/ClaudeOAuthUsageRateLimitGateLinuxTests.swift`
- `Tests/CodexBarTests/ClaudeOAuthCredentialsStoreCLIStorageOwnershipTests.swift`

CodexBar's inspected license is MIT, copyright 2026 Peter Steinberger. Preserve its license/attribution for copied code, fixtures, or substantial adaptations. Its macOS-specific browser/Keychain/UI implementation is not a portable Rust component.

## Recommended order

One real collector → shared model/service → shared GPUI panel → working standalone Windows tray app → additional requested subscriptions → Compi integration → native macOS menu-bar qualification. Do not wait for every standalone setting before integrating the working panel. No provider-parity promise, authentication manager, extra background daemon, or roadmap rewrite.
