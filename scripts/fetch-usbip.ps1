# Downloads the pinned usbip-win2 installer for the given architecture from
# the official GitHub releases and verifies its SHA-256 before use.
#
# Usage: scripts/fetch-usbip.ps1 -Arch arm64 -Destination installer\usbip
param(
    [Parameter(Mandatory = $true)][ValidateSet('x64', 'arm64')][string]$Arch,
    [string]$Destination = ''
)
$ErrorActionPreference = 'Stop'
if (!$Destination) {
    $Destination = Join-Path (Split-Path $PSScriptRoot -Parent) 'installer\usbip'
}

# Pinned release. Bump the version AND the hash together; hashes come from
# the release page / GitHub API ("digest" field) and are verified locally.
$Version = '0.9.8.1'
$Hashes = @{
    'x64'   = '38CAD6D4432B52D5BB9409D9AD03B72FDFFC4ADA4CD3A48FBECA1A2752A8518A'
    'arm64' = 'CAB7FF97F79275EEB5C5B8BB2EB3111FF6B6C4EEAD07D9BEC9BE5BD1E3A35800'
}

$name = "USBip-$Version-$Arch.exe"
$url = "https://github.com/vadimgrn/usbip-win2/releases/download/v.$Version/$name"
New-Item -ItemType Directory -Force -Path $Destination | Out-Null
$file = Join-Path $Destination $name

if (!(Test-Path $file)) {
    Write-Host "Downloading $url"
    Invoke-WebRequest -Uri $url -OutFile $file
}

$actual = (Get-FileHash -Path $file -Algorithm SHA256).Hash
if ($actual -ne $Hashes[$Arch]) {
    Remove-Item $file -Force
    throw "SHA-256 mismatch for ${name}: got $actual, expected $($Hashes[$Arch])"
}
Write-Host "OK: $name ($($Hashes[$Arch].Substring(0, 12))...)"
# Pipeline output: the exact file name, for the installer's /DUsbipFile.
Write-Output $name
