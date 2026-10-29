[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:OS -ne 'Windows_NT') {
    throw 'Package TokenBubble with a native Windows Rust toolchain.'
}

$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    $metadata = & cargo metadata --locked --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) { throw 'Could not read the workspace version.' }
    $package = ($metadata | ConvertFrom-Json).packages | Where-Object name -eq 'tokenbubble-gpui'
    $version = $package.version
    $target = 'x86_64-pc-windows-msvc'
    & cargo build --release --locked --target $target -p tokenbubble-gpui --bin tokenbubble
    if ($LASTEXITCODE -ne 0) { throw 'The Windows release build failed.' }

    $name = "tokenbubble-v$version-windows-x86_64"
    $output = Join-Path $root 'target\release-packages'
    New-Item -ItemType Directory -Force $output | Out-Null
    $staging = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid().ToString())
    New-Item -ItemType Directory $staging | Out-Null
    try {
        Copy-Item (Join-Path $root "target\$target\release\tokenbubble.exe") $staging
        Copy-Item (Join-Path $root 'THIRD_PARTY_NOTICES.md') $staging
        @"
TokenBubble $version - Windows x64

Install:
1. Extract this ZIP to a permanent folder, such as:
   %LOCALAPPDATA%\Programs\TokenBubble
2. Run tokenbubble.exe. The bubbles emoji appears in the notification area,
   possibly under Show Hidden Icons. Click it to open the usage panel.
3. Select Codex or Claude. Sign in with the corresponding native Windows CLI.
   Windows and WSL credentials are separate. TokenBubble does not manage logins.

To start automatically, enable Settings > General > Start at sign-in from this
installed copy. If it was already enabled for a development copy, toggle it off
and on so Windows uses the permanent executable path.

Updates are manual: quit TokenBubble from its tray menu, replace tokenbubble.exe
with the new release, then launch it again. Settings stay in
%APPDATA%\TokenBubble\settings.toml. CLI credentials are not part of the package.

This build is unsigned; Windows may show a SmartScreen warning. Download only
from the project's GitHub releases and verify the ZIP against SHA256SUMS.txt:
  https://github.com/cloudboy-jh/tokenbubble/releases
"@ | Set-Content (Join-Path $staging 'INSTALL.txt') -Encoding UTF8
        $archive = Join-Path $output "$name.zip"
        Compress-Archive -Path (Join-Path $staging '*') -DestinationPath $archive -Force
        $hash = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $name.zip" | Set-Content (Join-Path $output 'SHA256SUMS.txt') -Encoding ASCII
        Write-Output "Package: $archive"
        Write-Output "SHA-256: $hash"
    }
    finally {
        Remove-Item $staging -Recurse -Force
    }
}
finally {
    Pop-Location
}
