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

Write-Host "== kiln $version ($arch-windows), without LLVM"
cargo build --release --locked -p kiln_cli
if ($LASTEXITCODE) { throw 'cargo build failed' }
$kiln = Join-Path $stage 'bin\kiln.exe'
Copy-Item (Join-Path $env:CARGO_TARGET_DIR 'release\kiln.exe') $kiln
& $kiln __install-lib (Join-Path $stage 'lib\kiln')
if ($LASTEXITCODE) { throw 'kiln __install-lib failed' }

Write-Host '== kilnc (stage1 by the Rust compiler, stage2 by stage1)'
$boot = Join-Path $dist 'boot'
if (Test-Path $boot) { Remove-Item -Recurse -Force $boot }
New-Item -ItemType Directory -Force $boot | Out-Null
Push-Location compiler
$env:KILN_COMPILER = 'rust'; $env:KILN_RUN_FLAGS = ''
& $kiln build --release -o (Join-Path $boot 'stage1.exe')
$code = $LASTEXITCODE
Remove-Item Env:KILN_COMPILER
Pop-Location
if ($code) { throw 'building kilnc stage1 failed' }
$env:KILN_RT = Join-Path $stage 'lib\kiln\runtime\kiln_rt.lib'
if (-not (Test-Path $env:KILN_RT)) { $env:KILN_RT = Join-Path $stage 'lib\kiln\runtime\libkiln_rt.a' }
$env:KILNC_RESOLVE_DIR = Join-Path $root 'crates\kiln_resolve'
& (Join-Path $boot 'stage1.exe') --build --release --c-source compiler/native/arena.c compiler/src/main.kn -o (Join-Path $boot 'kilnc.exe')
if ($LASTEXITCODE) { throw 'building kilnc stage2 failed' }
Copy-Item (Join-Path $boot 'kilnc.exe') (Join-Path $stage 'lib\kiln\kilnc.exe')

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
