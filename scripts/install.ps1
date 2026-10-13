# po installer for Windows x86_64.
#
#   irm https://raw.githubusercontent.com/sumofbytes/po/HEAD/scripts/install.ps1 | iex
#   $env:PO_VERSION = "0.2.3"; irm ... | iex           # pin a version
#
# Env:
#   PO_VERSION       version to install ("latest" or e.g. "0.2.3" / "v0.2.3")
#   PO_INSTALL_DIR   install directory (default: %LOCALAPPDATA%\Programs\po)
#   PO_BASE_URL      base for /releases lookups (default: https://github.com/<repo>)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Repo = if ($env:PO_REPO) { $env:PO_REPO } else { "sumofbytes/po" }
$BaseUrl = if ($env:PO_BASE_URL) { $env:PO_BASE_URL.TrimEnd("/") } else { "https://github.com/$Repo" }
$InstallDir = if ($env:PO_INSTALL_DIR) { $env:PO_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\po" }
$Version = if ($env:PO_VERSION) { $env:PO_VERSION } else { "latest" }
$Target = "x86_64-pc-windows-msvc"

function Fail($message) { throw "po-install: $message" }

if ($env:PROCESSOR_ARCHITECTURE -ne "AMD64") {
    Fail "no prebuilt binary for $env:PROCESSOR_ARCHITECTURE. Build from source: cargo install --git https://github.com/$Repo"
}

if ($Version -eq "latest") {
    # Follow the /releases/latest redirect; no API call, so no rate limits.
    try {
        $response = Invoke-WebRequest -Uri "$BaseUrl/releases/latest" -UseBasicParsing -MaximumRedirection 5
        $final = if ($response.BaseResponse.ResponseUri) { $response.BaseResponse.ResponseUri.AbsoluteUri } else { $response.BaseResponse.RequestMessage.RequestUri.AbsoluteUri }
    } catch {
        Fail "cannot determine latest release (none published yet?)"
    }
    $Version = ($final.TrimEnd("/") -split "/")[-1]
}
if ($Version -match '^[0-9]') { $Version = "v$Version" }
if ($Version -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$') { Fail "invalid version '$Version' (expected e.g. 0.2.3 or v0.2.3)" }

$Base = "$BaseUrl/releases/download/$Version"
$Name = "po-$Version-$Target"
$Asset = "$Name.zip"
$Tmp = Join-Path ([IO.Path]::GetTempPath()) ("po-install-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $Tmp | Out-Null

try {
    Write-Host "Downloading po $Version for $Target ..."
    Invoke-WebRequest -Uri "$Base/$Asset" -OutFile (Join-Path $Tmp $Asset) -UseBasicParsing
    $sums = (Invoke-WebRequest -Uri "$Base/SHA256SUMS" -UseBasicParsing).Content

    $expected = $null
    foreach ($line in $sums -split "`n") {
        $fields = $line.Trim() -split "\s+"
        if ($fields.Count -ge 2 -and $fields[1].TrimStart("*") -eq $Asset) { $expected = $fields[0]; break }
    }
    if (-not $expected) { Fail "no checksum entry for $Asset" }
    $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $Tmp $Asset)).Hash
    if ($actual -ne $expected) { Fail "checksum mismatch for $Asset (got $actual, want $expected)" }

    Expand-Archive -Path (Join-Path $Tmp $Asset) -DestinationPath $Tmp
    $binary = Join-Path $Tmp "$Name\po.exe"
    if (-not (Test-Path $binary)) { Fail "archive did not contain a po.exe binary" }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    $dest = Join-Path $InstallDir "po.exe"
    if (Test-Path $dest) {
        # A running po.exe can be renamed but not overwritten.
        Move-Item -Force $dest "$dest.old"
    }
    Move-Item -Force $binary $dest
    Remove-Item -Force "$dest.old" -ErrorAction SilentlyContinue
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (($userPath -split ";") -notcontains $InstallDir) {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$InstallDir".TrimStart(";"), "User")
    Write-Host "note: added $InstallDir to your user PATH; open a new terminal to pick it up."
}

Write-Host "Installed po $Version to $dest"
Write-Host ""
Write-Host 'Next: install an ACP agent (e.g. opencode), then run `po` in a project.'
