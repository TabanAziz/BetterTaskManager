# Builds the release files in dist\: setup, portable zip and checksums.
# Refuses to finish if a binary would reveal the builder's user name or profile path.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$version = ([regex]'(?m)^version = "([^"]+)"').Match((Get-Content (Join-Path $root 'Cargo.toml') -Raw)).Groups[1].Value
$setupVersion = ([regex]'(?m)^version = "([^"]+)"').Match((Get-Content (Join-Path $root 'setup\Cargo.toml') -Raw)).Groups[1].Value
if (-not $version -or $version -ne $setupVersion) { throw "Cargo.toml versions differ: '$version' and '$setupVersion'" }

& (Join-Path $PSScriptRoot 'check.ps1')

$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$flags = @(
    '-Ctarget-feature=+crt-static',
    "--remap-path-prefix=$env:USERPROFILE=~",
    "--remap-path-prefix=$cargoHome=cargo",
    "--remap-path-prefix=$root=btm"
)
$previousFlags = $env:CARGO_ENCODED_RUSTFLAGS
$env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]0x1f
try {
    foreach ($project in @($root, (Join-Path $root 'setup'))) {
        Push-Location $project
        try {
            & cargo build --release --locked
            if ($LASTEXITCODE -ne 0) { throw "build failed in $project" }
        }
        finally { Pop-Location }
    }
}
finally { $env:CARGO_ENCODED_RUSTFLAGS = $previousFlags }

$app = Join-Path $root 'target\release\BTM.exe'
$setup = Join-Path $root 'setup\target\release\BTM-Setup.exe'

$latin = [Text.Encoding]::GetEncoding(28591)
foreach ($binary in @($app, $setup)) {
    $bytes = [IO.File]::ReadAllBytes($binary)
    $text = $latin.GetString($bytes)
    foreach ($private in @($env:USERPROFILE, "\$env:USERNAME\", "/$env:USERNAME/")) {
        $utf16 = $latin.GetString([Text.Encoding]::Unicode.GetBytes($private))
        foreach ($needle in @($private, $utf16)) {
            if ($text.IndexOf($needle, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
                throw "$binary contains '$private'"
            }
        }
    }
}

$check = Start-Process -FilePath $setup -ArgumentList 'self-check' -Wait -PassThru
if ($check.ExitCode -ne 0) { throw 'setup self-check failed' }

$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force -Path $dist | Out-Null
$setupFile = Join-Path $dist "BTM-Setup-$version.exe"
$zipFile = Join-Path $dist "BTM-$version-portable.zip"
Copy-Item $setup $setupFile -Force

Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$stream = New-Object IO.FileStream($zipFile, [IO.FileMode]::Create)
$zip = New-Object IO.Compression.ZipArchive($stream, [IO.Compression.ZipArchiveMode]::Create)
try {
    $parts = [ordered]@{
        'BTM.exe'                  = $app
        'LICENSE.txt'              = (Join-Path $root 'LICENSE')
        'THIRD-PARTY-LICENSES.txt' = (Join-Path $root 'THIRD-PARTY-LICENSES')
    }
    foreach ($name in $parts.Keys) {
        [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $parts[$name], $name) | Out-Null
    }
}
finally {
    $zip.Dispose()
    $stream.Dispose()
}

$sums = foreach ($file in @($setupFile, $zipFile)) {
    '{0}  {1}' -f (Get-FileHash $file -Algorithm SHA256).Hash.ToLower(), (Split-Path -Leaf $file)
}
[IO.File]::WriteAllText((Join-Path $dist 'SHA256SUMS.txt'), (($sums -join "`n") + "`n"))
"Release $version is in $dist"
$sums
