# aiSlang installer for Windows -- PLAN-90-days-v0_2.md Section Phase 1
# Week 13 Mon (b), Track A.5.
#
# PowerShell wrapper that bootstraps install.sh inside WSL2.  All
# the platform-detection / tarball-fetch / sha256-verify logic
# lives in install.sh -- this script only needs to:
#
#   1. Verify WSL2 is installed (and offer guidance if it isn't).
#   2. Optionally pick a non-default distro (via $env:AISLANG_WSL_DISTRO).
#   3. Forward $env:REPO / $env:VERSION / $env:PREFIX / $env:BASE_URL
#      into the WSL shell so they continue to work as overrides.
#   4. Invoke `curl -sSfL https://.../install.sh | sh` inside WSL.
#
# Why this shape (vs. a native Windows port of aislang): Docker
# Desktop on Windows runs through WSL2 anyway, so the Linux build
# of aislang is what every Windows user actually wants -- installed
# inside WSL where ELF binaries are first-class.  This script just
# saves users from opening a WSL shell manually.
#
# Strategic note (Decision 34): through v0.1 and v0.2 this script
# is distributed via authenticated `gh release download` rather
# than a public irm one-liner.  Public hosting goes live with v0.3.
#
# Supported invocation patterns:
#
#   # one-liner (after download via `gh release download`):
#   pwsh -ExecutionPolicy Bypass -File install.ps1
#
#   # one-liner (v0.3+, when the public URL goes live):
#   irm https://github.com/aislang/aislang/releases/latest/download/install.ps1 | iex
#
#   # with environment overrides (PowerShell syntax):
#   $env:VERSION = 'v0.2.0'; $env:PREFIX = '/usr/local/bin'
#   pwsh -ExecutionPolicy Bypass -File install.ps1
#
# Environment-variable overrides (mirror install.sh):
#   REPO                    -- github "<owner>/<repo>" (default: aislang/aislang)
#   VERSION                 -- tag to install            (default: latest)
#   PREFIX                  -- install dir inside WSL    (default: $HOME/.local/bin)
#   BASE_URL                -- release-asset base URL    (default: derived from REPO)
#   AISLANG_WSL_DISTRO      -- WSL distro to install into (default: WSL default distro)

# Strict-mode equivalent: fail on missing variables and unrecoverable errors.
# (Doesn't change the behaviour of native commands -- those still need
# $LASTEXITCODE checks.)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# -- tiny logging helpers ----------------------------------------
# Write-Host is deliberate here: this is an interactive installer
# where colored '==>' lines are the UX contract (matches install.sh's
# `printf '\033[1;34m==>\033[0m %s\n'`).  Write-Output would pollute
# the script's pipeline; Write-Information would be silenced under
# the default $InformationPreference.  PSScriptAnalyzer's
# PSAvoidUsingWriteHost warning is therefore explicitly suppressed
# per-function.
function Write-Info {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSAvoidUsingWriteHost', '')]
    param([string]$msg)
    # Match install.sh's `==>` prefix so users who pivot between
    # the two get the same visual cadence.
    Write-Host "==> $msg" -ForegroundColor Blue
}
function Write-Warn {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSAvoidUsingWriteHost', '')]
    param([string]$msg)
    Write-Host "==> $msg" -ForegroundColor Yellow
}
function Write-Die {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute(
        'PSAvoidUsingWriteHost', '')]
    param([string]$msg)
    Write-Host "==> $msg" -ForegroundColor Red
    exit 1
}

# -- ensure WSL is available -------------------------------------
function Test-Wsl {
    # `Get-Command` returns null + sets $? = false when missing;
    # we swallow the error and check the result directly.
    $wsl = Get-Command wsl.exe -ErrorAction SilentlyContinue
    if (-not $wsl) {
        Write-Die @'
WSL is not installed on this system.

aiSlang on Windows runs inside WSL2 (Docker Desktop on Windows
already uses WSL2 -- the Linux build of aislang is what you want).

One-time setup, from an *elevated* PowerShell prompt (Run as
Administrator):

    wsl --install

That installs WSL2 + the default Ubuntu distro.  A reboot is
required.  After reboot, re-run this installer.

If you already have WSL1, upgrade with:

    wsl --set-default-version 2
    wsl --update
'@
    }

    # `wsl --status` returns 0 when WSL is set up + a default
    # distro is registered.  Non-zero means WSL is installed but
    # no distro is registered (rare -- user manually removed it).
    #
    # `wsl.exe` outputs UTF-16; capture as raw bytes and ignore
    # text -- we only care about the exit code here.
    $null = & wsl.exe --status 2>$null
    if ($LASTEXITCODE -ne 0) {
        Write-Die @'
WSL is installed but no default distro is registered.

Install one and try again, e.g. from an elevated PowerShell:

    wsl --install -d Ubuntu

Or, if you already have a distro installed but no default is set:

    wsl --list --quiet
    wsl --set-default <distro-name>
'@
    }
}

# -- pick which distro to invoke into ----------------------------
# Returns either an empty string (use WSL default) or a `-d <name>`
# pair to splat into the wsl.exe argument list.
function Resolve-Distro {
    if ($env:AISLANG_WSL_DISTRO) {
        Write-Info "using WSL distro: $($env:AISLANG_WSL_DISTRO)"
        return @('-d', $env:AISLANG_WSL_DISTRO)
    }
    Write-Info 'using WSL default distro'
    return @()
}

# -- build the env-var prefix forwarded into WSL -----------------
# install.sh reads REPO / VERSION / PREFIX / BASE_URL from its
# environment; we mirror whatever the caller set on $env:.
# Concatenated into a single `KEY=value KEY=value sh -c "..."` form
# so wsl.exe sees one shell invocation with the env populated.
function Resolve-EnvPrefix {
    $pairs = @()
    foreach ($name in @('REPO', 'VERSION', 'PREFIX', 'BASE_URL')) {
        $val = [Environment]::GetEnvironmentVariable($name)
        if ($val) {
            # POSIX-shell single-quote escaping: `'` -> `'\''`.
            $escaped = $val -replace "'", "'\''"
            $pairs += "$name='$escaped'"
        }
    }
    if ($pairs.Count -eq 0) {
        return ''
    }
    return ($pairs -join ' ') + ' '
}

# -- derive the install.sh URL -----------------------------------
# Defaults match install.sh's defaults; allow per-flag overrides
# at PowerShell scope to keep parity.
function Resolve-InstallShUrl {
    $repo    = if ($env:REPO)    { $env:REPO }    else { 'aislang/aislang' }
    $version = if ($env:VERSION) { $env:VERSION } else { 'latest' }
    $base    = if ($env:BASE_URL) {
        # Caller-supplied base, used by the cold-VM rehearsal
        # (PLAN Section Week 11 Thu) to mirror releases to an http mirror.
        "$($env:BASE_URL)/$version"
    } else {
        "https://github.com/$repo/releases/$version/download"
    }
    return "$base/install.sh"
}

# -- main --------------------------------------------------------
function Main {
    Write-Info 'aiSlang installer (PowerShell wrapper for WSL2)'

    Test-Wsl

    $distroArgs  = Resolve-Distro
    $envPrefix   = Resolve-EnvPrefix
    $installShUrl = Resolve-InstallShUrl

    Write-Info "fetching $installShUrl inside WSL"

    # The full shell command we run inside WSL:
    #   <env vars> curl -sSfL <url> | sh
    # `bash -lc` ensures a login-shell env so PATH includes
    # ~/.local/bin etc. (so install.sh's success banner can echo
    # the right PATH-add hint for the user).
    $cmd = "${envPrefix}curl -sSfL '$installShUrl' | sh"

    # Spread the optional `-d <distro>` args, then `--`, then the
    # bash invocation.  `--` separates wsl's own args from the
    # shell-side command.
    $wslArgs = @() + $distroArgs + @('--', 'bash', '-lc', $cmd)

    & wsl.exe @wslArgs
    $exit = $LASTEXITCODE

    if ($exit -ne 0) {
        Write-Die "install.sh exited with code $exit (run inside WSL -- see output above for details)"
    }

    Write-Info 'done.'
    Write-Info 'open a new WSL shell (`wsl`) and run: aislang --help'
}

Main
