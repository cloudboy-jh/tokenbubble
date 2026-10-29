![TokenBubble](docs/social-preview.png)

Codex and Claude subscription usage in your Windows tray.

[Download for Windows x64](https://github.com/cloudboy-jh/tokenbubble/releases)

## Install

1. Download the release ZIP and extract it to a permanent folder, such as `%LOCALAPPDATA%\Programs\TokenBubble`.
2. Run `tokenbubble.exe`. Click the bubbles icon near the clock, under **Show hidden icons** if needed.
3. Choose **Codex** or **Claude**. TokenBubble reads that CLI's existing Windows sign-in.

The prerelease is unsigned, so Windows may show a SmartScreen warning. Verify the ZIP against the release's `SHA256SUMS.txt`.

## Use

- Check session, weekly, and model limits with reset countdowns. **Refresh** fetches a new reading.
- **General:** choose the provider, refresh interval, and whether to start at sign-in.
- **Display:** show usage or remaining quota, choose reset formatting, and toggle monetary fields.
- **Source:** inspect the credential location, current plan, freshness, and errors.

TokenBubble does not log in, renew tokens, or copy credentials from WSL. Use the provider's native Windows CLI to sign in.

## Screenshots

Captured from the Windows app. Account details and local paths are hidden; usage values are real readings.

[Codex panel](docs/screenshots/codex.png) · [Claude panel](docs/screenshots/claude.png) · [1280×640 social card](docs/social-preview.png)

<details>
<summary>General settings</summary>

![General settings](docs/screenshots/settings-general.png)

</details>

<details>
<summary>Display settings</summary>

![Display settings](docs/screenshots/settings-display.png)

</details>

<details>
<summary>Source settings</summary>

![Source settings](docs/screenshots/settings-source.png)

</details>

<details>
<summary>About</summary>

![About TokenBubble](docs/screenshots/settings-about.png)

</details>

## Updates

Quit TokenBubble from its tray menu, replace the executable with the new release, and launch it again. Settings stay in `%APPDATA%\TokenBubble\settings.toml`. Updates are manual.

## Development

Windows PowerShell, with Rust installed:

```powershell
cargo dev
```

To build a release ZIP:

```powershell
powershell -ExecutionPolicy Bypass -File tools/package-windows.ps1
```
