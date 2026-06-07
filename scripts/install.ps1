<#
.SYNOPSIS
    doiget installer for Windows — downloads the prebuilt, checksum-verified
    binary from the latest (or a pinned) GitHub Release. No Rust toolchain or
    compilation required.

.DESCRIPTION
    Usage:
      irm https://raw.githubusercontent.com/sotashimozono/doiget/main/scripts/install.ps1 | iex

    Environment overrides:
      $env:DOIGET_VERSION      version WITHOUT the leading 'v' (default: latest stable)
      $env:DOIGET_INSTALL_DIR  install directory (default: %LOCALAPPDATA%\Programs\doiget)

    The published `.sha256` sidecar is verified before install; a mismatch
    aborts. cosign bundles are also published — see the README for optional
    keyless signature verification.
#>
#Requires -Version 5
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repo = 'sotashimozono/doiget'
$version = if ($env:DOIGET_VERSION) { $env:DOIGET_VERSION } else { 'latest' }
$installDir = if ($env:DOIGET_INSTALL_DIR) { $env:DOIGET_INSTALL_DIR } else { "$env:LOCALAPPDATA\Programs\doiget" }

if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw "doiget-install: unsupported architecture '$env:PROCESSOR_ARCHITECTURE' — only x86_64/AMD64 is published for Windows (target tracked in #247)"
}
$asset = 'doiget-windows-x86_64.exe'

if ($version -eq 'latest') {
    $base = "https://github.com/$repo/releases/latest/download"
} else {
    $base = "https://github.com/$repo/releases/download/v$version"
}

$tmpDir = (New-Item -ItemType Directory -Path (Join-Path $env:TEMP ("doiget-install-" + [Guid]::NewGuid().ToString('N')))).FullName
try {
    $binPath = Join-Path $tmpDir $asset
    $shaPath = "$binPath.sha256"

    Write-Host "doiget-install: downloading $asset ($version)"
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $binPath
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset.sha256" -OutFile $shaPath

    # The sidecar is `openssl dgst -sha256 -r` output: "<hex>  *<filename>".
    $expected = (((Get-Content $shaPath -Raw).Trim() -split '\s+')[0]).ToLower()
    $actual = (Get-FileHash -Algorithm SHA256 -Path $binPath).Hash.ToLower()
    if ([string]::IsNullOrWhiteSpace($expected)) { throw "doiget-install: empty expected checksum in $asset.sha256" }
    if ($expected -ne $actual) { throw "doiget-install: checksum mismatch: expected $expected, got $actual" }
    Write-Host "doiget-install: checksum OK ($actual)"

    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    $dest = Join-Path $installDir 'doiget.exe'
    Move-Item -Force -Path $binPath -Destination $dest
    Write-Host "doiget-install: installed to $dest"

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not $userPath -or ($userPath -split ';') -notcontains $installDir) {
        Write-Host "doiget-install: note: $installDir is not on your PATH. Add it (new shells) with:"
        Write-Host "  [Environment]::SetEnvironmentVariable('Path', `"$installDir;`" + [Environment]::GetEnvironmentVariable('Path','User'), 'User')"
    }
    Write-Host "doiget-install: done - run: doiget --version"
} finally {
    Remove-Item -Recurse -Force -Path $tmpDir -ErrorAction SilentlyContinue
}
