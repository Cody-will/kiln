# Installs the latest Kiln release on Windows.
#   irm https://raw.githubusercontent.com/Cody-will/kiln/main/install.ps1 | iex
# Environment: KILN_PREFIX (default %LOCALAPPDATA%\Programs\kiln),
# KILN_VERSION (a tag such as v0.2.0; default: latest),
# KILN_DOWNLOAD_URL (a base URL holding the archive, e.g. a local mirror).
# Needs clang (LLVM) on PATH to build programs: winget install LLVM.LLVM
$ErrorActionPreference = 'Stop'
$repo = 'Cody-will/kiln'
$prefix = if ($env:KILN_PREFIX) { $env:KILN_PREFIX } else { Join-Path $env:LOCALAPPDATA 'Programs\kiln' }
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'aarch64' } else { 'x86_64' }
$asset = "kiln-$arch-windows.zip"
$base = if ($env:KILN_DOWNLOAD_URL) { $env:KILN_DOWNLOAD_URL.TrimEnd('/') }
        elseif ($env:KILN_VERSION) { "https://github.com/$repo/releases/download/$($env:KILN_VERSION)" }
        else { "https://github.com/$repo/releases/latest/download" }
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("kiln-" + [Guid]::NewGuid())
New-Item -ItemType Directory $tmp | Out-Null
try {
    $zip = Join-Path $tmp $asset
    Write-Host "Downloading $base/$asset"
    Invoke-WebRequest -UseBasicParsing "$base/$asset" -OutFile $zip
    try {
        Invoke-WebRequest -UseBasicParsing "$base/$asset.sha256" -OutFile "$zip.sha256"
        $want = ((Get-Content "$zip.sha256" -Raw).Trim() -split '\s+')[0].ToLower()
        $got = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
        if ($want -ne $got) { throw "checksum mismatch for $asset (want $want, got $got)" }
    } catch [System.Net.WebException] {
        Write-Warning 'no checksum file; skipping verification'
    }
    Expand-Archive -Path $zip -DestinationPath (Join-Path $tmp 'x')
    $src = Get-ChildItem (Join-Path $tmp 'x') -Directory | Select-Object -First 1
    if (Test-Path $prefix) {
        Remove-Item -Recurse -Force (Join-Path $prefix 'bin'), (Join-Path $prefix 'lib') -ErrorAction SilentlyContinue
    }
    New-Item -ItemType Directory -Force $prefix | Out-Null
    Copy-Item -Recurse -Force (Join-Path $src.FullName '*') $prefix
    $bin = Join-Path $prefix 'bin'
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not (($userPath -split ';') -contains $bin)) {
        [Environment]::SetEnvironmentVariable('Path', ($userPath.TrimEnd(';') + ";$bin").TrimStart(';'), 'User')
        Write-Host "Added $bin to your user PATH (open a new terminal)."
    }
    & (Join-Path $bin 'kiln.exe') --version
    if (-not (Get-Command clang -ErrorAction SilentlyContinue)) {
        Write-Warning 'clang not found; install LLVM (winget install LLVM.LLVM) to build programs.'
    }
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
