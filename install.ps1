# Installs `nan`, the NaN toolchain of the latest release or of the tag in NAN_VERSION, into
# NAN_INSTALL_DIR (by default %LOCALAPPDATA%\Programs\nan), after checking the SHA-256 the release
# publishes, and adds that directory to the user's PATH. It throws rather than exits, so that
# running it through Invoke-Expression leaves the calling shell open.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$repository = 'https://github.com/nan-language/nan'
$dir = if ($env:NAN_INSTALL_DIR) { $env:NAN_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\nan' }

$target = switch ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture) {
    'X64' { 'x86_64-windows' }
    'Arm64' { 'aarch64-windows' }
    default { throw 'nan: releases are built for Windows on x86_64 and AArch64 only' }
}

$version = if ($env:NAN_VERSION) { $env:NAN_VERSION } else {
    (Invoke-RestMethod 'https://api.github.com/repos/nan-language/nan/releases/latest').tag_name
}
if ($version -notmatch '^v[0-9]') { throw 'nan: no release found' }

$asset = "nan-$version-$target.exe"
$work = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
New-Item -ItemType Directory $work | Out-Null
try {
    $image = Join-Path $work $asset
    Invoke-WebRequest "$repository/releases/download/$version/$asset" -OutFile $image -UseBasicParsing
    Invoke-WebRequest "$repository/releases/download/$version/$asset.sha256" -OutFile "$image.sha256" -UseBasicParsing
    $expected = ((Get-Content -Raw "$image.sha256").Trim() -split '\s+')[0]
    if ((Get-FileHash $image -Algorithm SHA256).Hash -ne $expected) { throw "nan: $asset does not match its SHA-256" }
    New-Item -ItemType Directory -Force $dir | Out-Null
    Copy-Item $image (Join-Path $dir 'nan.exe') -Force
} finally {
    Remove-Item -Recurse -Force $work
}

Write-Output "installed nan $version as $(Join-Path $dir 'nan.exe')"
$entries = @(([Environment]::GetEnvironmentVariable('Path', 'User') -split ';') | Where-Object { $_ })
if ($entries -notcontains $dir) {
    [Environment]::SetEnvironmentVariable('Path', (($entries + $dir) -join ';'), 'User')
    Write-Output "added $dir to your PATH; open a new terminal to run nan"
}
