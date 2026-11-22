# The local gate: everything that has to be green before a commit or a release.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

function Invoke-Cargo {
    & cargo @args
    if ($LASTEXITCODE -ne 0) { throw "cargo $($args -join ' ') failed" }
}

Push-Location $root
try {
    Invoke-Cargo fmt --check
    Invoke-Cargo clippy --release --locked --all-targets '--' -D warnings
    Invoke-Cargo clippy --release --locked --features demo '--' -D warnings
    Invoke-Cargo test --release --locked
    Invoke-Cargo build --release --locked
    Push-Location (Join-Path $root 'setup')
    try {
        Invoke-Cargo fmt --check
        Invoke-Cargo clippy --release --locked --all-targets '--' -D warnings
        Invoke-Cargo test --release --locked
    }
    finally { Pop-Location }
    'All checks passed.'
}
finally { Pop-Location }
