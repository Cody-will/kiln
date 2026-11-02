# Builds the Windows release archive for this machine:
#   target\dist\kiln-x86_64-windows.zip (+ .sha256)
# with the same layout as tools/release.sh. Needs cargo and clang (LLVM)
# on PATH; GitHub's windows-latest runners have both.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'aarch64' } else { 'x86_64' }
$dist = Join-Path $root 'target\dist'
$name = "kiln-$version"
$stage = Join-Path $dist $name
$env:CARGO_TARGET_DIR = Join-Path $root 'target\dist-cargo'
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force (Join-Path $stage 'bin') | Out-Null

# The C runtime uses GNU C extensions (__int128), so build it with
# clang-cl rather than MSVC's cl.exe.
if (-not $env:CC) { $env:CC = 'clang-cl' }
Write-Host "== kiln $version ($arch-windows), without LLVM"
cargo build --release --locked -p kiln_cli
if ($LASTEXITCODE) { throw 'cargo build failed' }
$kiln = Join-Path $stage 'bin\kiln.exe'
Copy-Item (Join-Path $env:CARGO_TARGET_DIR 'release\kiln.exe') $kiln
& $kiln __install-lib (Join-Path $stage 'lib\kiln')
if ($LASTEXITCODE) { throw 'kiln __install-lib failed' }

# kilnc drives clang through POSIX shell scripts, so it is not shipped on
# Windows yet: `kiln` uses its built-in code generator there (tier 2, see
# docs/STABILITY.md).

Write-Host '== editors'
New-Item -ItemType Directory -Force (Join-Path $stage 'lib\kiln\editors') | Out-Null
Copy-Item -Recurse editors\nvim (Join-Path $stage 'lib\kiln\editors\nvim')
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue (Join-Path $stage 'lib\kiln\editors\nvim\test')
Copy-Item LICENSE-MIT, LICENSE-APACHE, README.md $stage

Write-Host '== smoke test'
$t = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid())
New-Item -ItemType Directory $t | Out-Null
Push-Location $t
& $kiln new demo | Out-Null
Set-Location demo
$out = & $kiln run
$code = $LASTEXITCODE
Pop-Location
if ($code -or -not $out) { throw 'smoke test failed' }

$asset = "kiln-$arch-windows.zip"
$zip = Join-Path $dist $asset
if (Test-Path $zip) { Remove-Item $zip }
Compress-Archive -Path $stage -DestinationPath $zip
$hash = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
Set-Content -NoNewline -Path "$zip.sha256" -Value "$hash  $asset`n"
Write-Host "built $zip"
