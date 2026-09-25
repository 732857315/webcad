# Put the portable Rust toolchain on PATH for the current PowerShell session.
#
#   . .\tools\env.ps1
#
# No drive letters are hard-coded. The toolchain is looked up in order:
#   1. $env:PORTABLE_ROOT\Rust   (set by MoveCoding\Scripts\Use-PortableEnvironment.ps1)
#   2. <ancestor>\MoveCoding\Rust for the nearest ancestor directory of this repo
#   3. whatever cargo is already on PATH (CI runners, other machines)

$webcadRepo = Split-Path -Parent $PSScriptRoot
$webcadPortable = $null

if ($env:PORTABLE_ROOT -and (Test-Path -LiteralPath (Join-Path $env:PORTABLE_ROOT 'Rust\cargo\bin') -PathType Container)) {
    $webcadPortable = $env:PORTABLE_ROOT
}

if (-not $webcadPortable) {
    $dir = Get-Item -LiteralPath $webcadRepo
    while ($dir) {
        $candidate = Join-Path $dir.FullName 'MoveCoding'
        if (Test-Path -LiteralPath (Join-Path $candidate 'Rust\cargo\bin') -PathType Container) {
            $webcadPortable = $candidate
            break
        }
        $dir = $dir.Parent
    }
}

if ($webcadPortable) {
    $env:RUSTUP_HOME = Join-Path $webcadPortable 'Rust\rustup'
    $env:CARGO_HOME = Join-Path $webcadPortable 'Rust\cargo'
    $cargoBin = Join-Path $env:CARGO_HOME 'bin'
    if (($env:Path -split ';') -notcontains $cargoBin) { $env:Path = "$cargoBin;$env:Path" }
    # GNU binutils (dlltool + as) from the portable MSYS2. The windows-gnu target needs them for crates
    # linked via raw-dylib (windows-link, getrandom). Appended so they never shadow other tools.
    $binutils = Join-Path $webcadPortable 'MSYS2\ucrt64\bin'
    if ((Test-Path -LiteralPath (Join-Path $binutils 'dlltool.exe') -PathType Leaf) -and
        (($env:Path -split ';') -notcontains $binutils)) {
        $env:Path = "$env:Path;$binutils"
    }
}

if (-not $env:CARGO_NET_RETRY) { $env:CARGO_NET_RETRY = '10' }
# schannel's certificate revocation lookup fails intermittently on this network
# (CRYPT_E_NO_REVOCATION_CHECK). Chain validation stays on; only the revocation lookup is skipped.
if (-not $env:CARGO_HTTP_CHECK_REVOKE) { $env:CARGO_HTTP_CHECK_REVOKE = 'false' }

if (-not (Get-Command cargo -CommandType Application -ErrorAction SilentlyContinue)) {
    Write-Warning 'webcad: cargo not found. Install Rust into MoveCoding\Rust or put cargo on PATH.'
}

Remove-Variable webcadRepo, webcadPortable -ErrorAction SilentlyContinue
