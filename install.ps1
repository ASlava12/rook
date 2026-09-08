# Install Rook from a published release.
#
#   irm https://raw.githubusercontent.com/ASlava12/rook/main/install.ps1 | iex
#
# What it does, in the order it does it: works out which build this machine
# wants, downloads that archive and the release's checksums, refuses to go on if
# they disagree, and copies two binaries and the built-in skills into a
# directory under your profile. It writes nowhere else and asks for no
# privileges. It does add its directory to your user PATH, because on Windows
# there is no profile file everybody agrees on — and it says so when it does.

$ErrorActionPreference = 'Stop'

$repo = if ($env:ROOK_REPO) { $env:ROOK_REPO } else { 'ASlava12/rook' }
# The layout the binaries look for: bin\rook.exe finds its skills at
# ..\share\rook\skills.
$prefix = if ($env:ROOK_PREFIX) { $env:ROOK_PREFIX } else { "$env:LOCALAPPDATA\rook" }
$version = if ($env:ROOK_VERSION) { $env:ROOK_VERSION } else { 'latest' }

$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
    'AMD64' { 'x86_64' }
    'ARM64' { 'aarch64' }
    default { throw "no published build for $env:PROCESSOR_ARCHITECTURE; build from source with cargo" }
}
$target = "$arch-pc-windows-msvc"

$base = if ($version -eq 'latest') {
    "https://github.com/$repo/releases/latest/download"
} else {
    "https://github.com/$repo/releases/download/$version"
}

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("rook-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Write-Host "rook: fetching $target from $repo ($version)"
    # The checksums first: the archive's name carries a version that `latest`
    # does not know yet, so the name is read out of them.
    try {
        Invoke-WebRequest -Uri "$base/SHA256SUMS" -OutFile "$tmp\SHA256SUMS" -UseBasicParsing
    } catch {
        throw "no published release to install from yet — build from a clone with ``cargo xtask dist``"
    }

    $sums = Get-Content "$tmp\SHA256SUMS" | ForEach-Object { , ($_ -split '\s+', 2) }
    $line = $sums | Where-Object { $_[1] -like "*$target*" } | Select-Object -First 1
    if (-not $line) { throw "the release has no build for $target" }
    $archive = $line[1].Trim()
    $expected = $line[0].Trim()

    Invoke-WebRequest -Uri "$base/$archive" -OutFile "$tmp\$archive" -UseBasicParsing

    # Verified before anything is unpacked. A download nobody checked is a
    # download somebody else can replace.
    $got = (Get-FileHash "$tmp\$archive" -Algorithm SHA256).Hash.ToLower()
    if ($got -ne $expected.ToLower()) {
        throw "the checksum does not match: expected $expected, got $got — nothing was installed"
    }
    Write-Host 'rook: checksum ok'

    Expand-Archive -Path "$tmp\$archive" -DestinationPath $tmp -Force
    $unpacked = Join-Path $tmp ([System.IO.Path]::GetFileNameWithoutExtension($archive))
    if (-not (Test-Path "$unpacked\bin")) { throw "the archive is not shaped as expected: no bin\ in $archive" }

    New-Item -ItemType Directory -Force -Path "$prefix\bin", "$prefix\share\rook" | Out-Null
    # A running binary cannot be overwritten on Windows, so a daemon that is up
    # is stopped first rather than being the reason an install half-fails.
    if (Get-Command rook -ErrorAction SilentlyContinue) {
        & rook daemon stop 2>$null | Out-Null
    }
    Copy-Item "$unpacked\bin\*" "$prefix\bin\" -Force
    if (Test-Path "$prefix\share\rook\skills") { Remove-Item "$prefix\share\rook\skills" -Recurse -Force }
    Copy-Item "$unpacked\share\rook\skills" "$prefix\share\rook\skills" -Recurse -Force

    Write-Host "rook: installed to $prefix\bin"
    & "$prefix\bin\rook.exe" --version

    # There is no `~/.profile` everybody agrees on here, so the user's own PATH
    # is edited — and said out loud, because a script that changes your
    # environment silently is one you find out about later.
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($userPath -notlike "*$prefix\bin*") {
        [Environment]::SetEnvironmentVariable('Path', "$prefix\bin;$userPath", 'User')
        Write-Host ''
        Write-Host "Added $prefix\bin to your user PATH. Open a new terminal for it to take effect."
    }

    Write-Host ''
    Write-Host 'Next: rook init, then rook.'
} finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
