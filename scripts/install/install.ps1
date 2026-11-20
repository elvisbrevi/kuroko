<#
.SYNOPSIS
Installs Kuroko on Windows from this repository's prebuilt release archive, for the current user
only: no administrator rights, no WSL, no Rust.

.DESCRIPTION
The command the portal shows (Settings > Machines > Connect this computer):

  Invoke-RestMethod -UseBasicParsing https://github.com/elvisbrevi/kuroko/releases/latest/download/install.ps1 -OutFile "$env:TEMP\kuroko-install.ps1"
  powershell -NoProfile -ExecutionPolicy Bypass -File "$env:TEMP\kuroko-install.ps1" -Origin https://your-portal.example

In order: detects the target; resolves the version (the repository's latest release unless
-Version is given); downloads that release's archive and its .sha256 over HTTPS from the release,
following only the release host's own redirect to its asset storage; checks the digest before
anything is unpacked or run; verifies build provenance with `gh attestation verify` when gh is
installed and signed in; unpacks into a temporary directory inside the installation root and moves
it into place (a failure leaves the previous installation working); writes the archive
installation record; adds the bin directory to the user PATH through the registry (never setx);
registers the host to start at login (`kuroko service install`, a scheduled task, no console
window); and, with -Origin, starts the browser sign-in so the approval page opens for "Trust this
machine".

The archive holds the CLI `kuroko.exe`, the host `kuroko-host.exe` and, as aliases of the same
programs, their names before the rename (`pta.exe`, `personal-teams-assistant.exe`), so scheduled
tasks, scripts and MCP configurations written for those keep working.

The command carries no secret: only the coordinator's public HTTPS origin. Running it again
repairs or upgrades the same installation, including one made before the rename in
%LOCALAPPDATA%\Programs\personal-teams-assistant, which is upgraded where it is; the profile
(settings, credentials, Microsoft session, machine identity) is never created, replaced or removed
by this script.

.PARAMETER Origin
Your coordinator portal's HTTPS origin: after installing, start the browser sign-in.
.PARAMETER Version
Install this release instead of the latest one.
.PARAMETER Root
Installation root (default: %LOCALAPPDATA%\Programs\kuroko, or the installation made before the
rename in %LOCALAPPDATA%\Programs\personal-teams-assistant).
.PARAMETER Integrations
Also install the skill and register `kuroko mcp` with the coding CLIs found; otherwise the commands
are printed.
.PARAMETER UnattendedUpdates
Let this computer install newer releases by itself when nothing would be interrupted (off unless
you pass this; change it later with `kuroko update unattended on|off --confirmed` or the local
page).
.PARAMETER NoService
Do not register the host to start at login.
.PARAMETER NoBrowser
Print the approval link instead of opening the browser.
.PARAMETER Uninstall
Remove the service entry, the binaries and the PATH entry; keep the profile.
.PARAMETER TestSource
Only for the installer's own tests, with KUROKO_INSTALL_TEST=1 (or PTA_INSTALL_TEST=1): a local
directory instead of the release.
#>
[CmdletBinding()]
param(
    [string] $Origin = '',
    [string] $Version = '',
    [string] $Root = '',
    [switch] $Integrations,
    [switch] $UnattendedUpdates,
    [switch] $NoService,
    [switch] $NoBrowser,
    [switch] $Uninstall,
    [string] $TestSource = ''
)

Set-StrictMode -Version 1.0
# The switches as plain script values, read by the functions below.
$WantIntegrations = $Integrations.IsPresent
$WantUnattended = $UnattendedUpdates.IsPresent
$SkipService = $NoService.IsPresent
$SkipBrowser = $NoBrowser.IsPresent
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Repository = 'elvisbrevi/kuroko'
$Product = 'kuroko'
# The package before 0.18.0: its default root, PATH entry and Cargo or binstall records belong to
# this product too, and its releases can still be installed with -Version.
$Legacy = 'personal-teams-assistant'
# What the release being installed is published as; Use-LegacyLayout switches these for a release
# before the rename.
$Package = $Product
$Cli = 'kuroko'
$HostBin = 'kuroko-host'
$Bins = @('kuroko', 'kuroko-host', 'personal-teams-assistant', 'pta')
$Provenance = $Repository
$RecordRepository = "https://github.com/$Repository"
# The skill directory and the MCP server name registered with coding CLIs keep the former name
# until a later release migrates the clients' configuration.
$Integration = $Product
$Record = 'install.json'
$Target = 'x86_64-pc-windows-msvc'

function Say([string] $Text) { Write-Host $Text }
function Warn([string] $Text) { Write-Warning $Text }
function Fail([string] $Text) { throw "install.ps1: $Text" }

# A variable of this product: KUROKO_<NAME>, or the former PTA_<NAME> when that is not set.
function Get-EnvValue([string] $Name) {
    $value = [Environment]::GetEnvironmentVariable("KUROKO_$Name")
    if (-not $value) { $value = [Environment]::GetEnvironmentVariable("PTA_$Name") }
    return $value
}

# ---------------------------------------------------------------------------------------------
# Inputs.

# Validated as the host validates a coordinator: https, a host, no credentials, path, query or
# fragment. Returns the normalized origin.
function Test-Origin([string] $Value) {
    $uri = $null
    if (-not [Uri]::TryCreate($Value, [UriKind]::Absolute, [ref] $uri) -or $uri.Scheme -ne 'https' -or
        $uri.UserInfo -or ($uri.AbsolutePath -ne '/') -or $uri.Query -or $uri.Fragment -or
        -not $uri.Host -or $Value -match '\s') {
        Fail 'Use the HTTPS origin of your coordinator portal, for example https://assistant.example.com.'
    }
    $origin = 'https://' + $uri.Host.ToLowerInvariant()
    if (-not $uri.IsDefaultPort) { $origin += ':' + $uri.Port }
    return $origin
}

function Test-Version([string] $Value) {
    if ($Value -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
        Fail 'Use a release version such as 0.18.0.'
    }
}

# Releases before 0.18.0 were published as personal-teams-assistant, with two executables.
function Test-BeforeRename([string] $Value) { return ([version] $Value) -lt ([version] '0.18.0') }

function Use-LegacyLayout {
    $script:Package = $Legacy
    $script:Cli = 'pta'
    $script:HostBin = $Legacy
    $script:Bins = @($Legacy, 'pta')
    # Built, attested and recorded under the repository's name at the time.
    $script:Provenance = "elvisbrevi/$Legacy"
    $script:RecordRepository = "https://github.com/elvisbrevi/$Legacy"
}

function Get-Architecture {
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    return $arch
}

# ---------------------------------------------------------------------------------------------
# Downloads. A release asset is served by github.com with one redirect to its own asset storage;
# only that redirect is followed, and only over HTTPS. The test source is a local directory that
# only KUROKO_INSTALL_TEST=1 (or PTA_INSTALL_TEST=1) with -TestSource enables; the portal's command
# never sets either.

$AssetHosts = @('objects.githubusercontent.com', 'release-assets.githubusercontent.com', 'github-releases.githubusercontent.com')

function Get-Redirect([string] $Url) {
    $request = [System.Net.HttpWebRequest]::Create($Url)
    $request.AllowAutoRedirect = $false
    $request.Method = 'GET'
    $request.UserAgent = "$Product-installer"
    try {
        $response = $request.GetResponse()
    } catch [System.Net.WebException] {
        if ($null -eq $_.Exception.Response) { return $null }
        $response = $_.Exception.Response
    }
    try {
        $code = [int] $response.StatusCode
        if ($code -ge 300 -and $code -lt 400) { return [string] $response.Headers['Location'] }
        return $null
    } finally {
        $response.Close()
    }
}

function Get-Asset([string] $Name, [string] $Destination) {
    if ($TestSource) {
        $source = Join-Path $TestSource $Name
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { return $false }
        Copy-Item -LiteralPath $source -Destination $Destination
        return $true
    }
    $location = Get-Redirect "https://github.com/$Repository/releases/download/v$Version/$Name"
    if (-not $location) { return $false }
    $uri = [Uri] $location
    if ($uri.Scheme -ne 'https' -or $AssetHosts -notcontains $uri.Host) {
        Fail 'The release host redirected to an unexpected address; nothing was downloaded.'
    }
    $client = New-Object System.Net.WebClient
    try {
        $client.Headers['User-Agent'] = "$Product-installer"
        $client.DownloadFile($uri, $Destination)
    } catch {
        return $false
    } finally {
        $client.Dispose()
    }
    return $true
}

function Get-LatestVersion {
    $location = Get-Redirect "https://github.com/$Repository/releases/latest"
    $prefix = "https://github.com/$Repository/releases/tag/v"
    if (-not $location -or -not $location.StartsWith($prefix)) {
        Fail 'The latest release could not be found. Check the network connection, or pass -Version.'
    }
    return $location.Substring($prefix.Length)
}

# ---------------------------------------------------------------------------------------------
# What is already on this computer.

# The product's packages Cargo records in $Dir, under either name.
function Get-CargoPackages([string] $Dir) {
    $found = @()
    foreach ($candidate in $Legacy, $Product) {
        foreach ($name in '.crates2.json', '.crates.toml') {
            $file = Join-Path $Dir $name
            if ((Test-Path -LiteralPath $file -PathType Leaf) -and
                (Select-String -LiteralPath $file -SimpleMatch "`"$candidate " -Quiet)) {
                $found += $candidate
                break
            }
        }
    }
    return $found
}

# A Cargo installation of this product, under either package name, would be a second copy: two
# hosts for one profile, updated two ways. Refuse and say how to switch.
function Assert-NoCargoCopy {
    $found = $null
    $packages = @()
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
    $recorded = @(Get-CargoPackages $cargoHome)
    if ($recorded.Count -gt 0) { $found = $cargoHome; $packages = $recorded }
    foreach ($name in 'kuroko', 'pta') {
        $onPath = Get-Command "$name.exe" -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if (-not $onPath -or ($onPath.Source -eq (Join-Path $Bin "$name.exe"))) { continue }
        $other = Split-Path (Split-Path $onPath.Source -Parent) -Parent
        $recorded = @(Get-CargoPackages $other)
        if ($recorded.Count -gt 0) { $found = $other; $packages = $recorded }
        else { Warn "Another $name is on your PATH at $($onPath.Source); the one installed here may be shadowed by it." }
    }
    if (-not $found) { return }
    # The commands of the package installed there; kuroko takes over the former package's
    # executables only when forced.
    $theirCli = if ($packages -contains $Product) { 'kuroko' } else { 'pta' }
    $stay = if ($packages -contains $Legacy) { "cargo install $Product --locked --force" } else { "cargo install $Product --locked" }
    $uninstallLines = ($packages | ForEach-Object { "    cargo uninstall $_" }) -join "`n"
    Fail @"
$($packages -join ' and ') is already installed with Cargo in $found.
Installing a second copy next to it would give one profile two hosts. To switch to this
installation, which needs no Rust toolchain, run:

    $theirCli service uninstall --confirmed
    $theirCli app quit
$uninstallLines

and then this command again. Your profile, credentials and Microsoft session are kept.
To stay with Cargo instead, update with: $stay
"@
}

# `cargo uninstall` leaves binstall's own record (crates-v1.json, one JSON object per line) naming
# the package. After a successful install, with no Cargo installation left, drop only the lines of
# this product's packages when the file is exactly that format, writing it atomically; otherwise
# say so.
function Clear-StaleBinstallRecord {
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
    $file = Join-Path $cargoHome 'binstall\crates-v1.json'
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { return }
    $utf8 = New-Object System.Text.UTF8Encoding $false
    $text = [System.IO.File]::ReadAllText($file, $utf8)
    $legacyPrefix = "{`"name`":`"$Legacy`""
    $productPrefix = "{`"name`":`"$Product`""
    $names = @()
    if ($text.Contains($legacyPrefix)) { $names += $Legacy }
    if ($text.Contains($productPrefix)) { $names += $Product }
    if ($names.Count -eq 0) { return }
    $named = $names -join ' and '
    if (@(Get-CargoPackages $cargoHome).Count -gt 0) { return }
    $lines = $text -split "`n"
    foreach ($line in $lines) {
        $bare = $line.TrimEnd("`r")
        if ($bare.Trim() -and -not ($bare.StartsWith('{"name":"') -and $bare.EndsWith('}'))) {
            Warn "binstall's record $file still names $named, which is no longer installed with Cargo; delete its line by hand."
            return
        }
    }
    $kept = @($lines | Where-Object { -not ($_.StartsWith($legacyPrefix) -or $_.StartsWith($productPrefix)) })
    $temp = "$file.kuroko-tmp"
    try {
        [System.IO.File]::WriteAllText($temp, ($kept -join "`n"), $utf8)
        Move-Item -LiteralPath $temp -Destination $file -Force
        Say "Removed the stale cargo binstall entry for $named from $file."
    } catch {
        Remove-Item -LiteralPath $temp -Force -ErrorAction SilentlyContinue
        Warn "The stale cargo binstall entry for $named in $file could not be removed; delete that line by hand."
    }
}

function Get-ProfileDir {
    $dir = Get-EnvValue 'PROFILE_DIR'
    if ($dir) { return $dir }
    return Join-Path $env:APPDATA 'dev.personalteams.assistant'
}

# This product's archive record in the root $Dir, under either package name.
function Test-ProductRecord([string] $Dir) {
    $file = Join-Path $Dir $Record
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { return $false }
    try { $data = Get-Content -LiteralPath $file -Raw | ConvertFrom-Json } catch { return $false }
    return [bool] ($data.kind -eq 'archive' -and (@($Legacy, $Product) -contains $data.package))
}

function Get-LegacyRoot { return Join-Path $env:LOCALAPPDATA "Programs\$Legacy" }
function Get-ProductRoot { return Join-Path $env:LOCALAPPDATA "Programs\$Product" }

# An installation made before the rename is upgraded, or removed, where it is.
function Get-DefaultRoot {
    if (Test-ProductRecord (Get-LegacyRoot)) { return Get-LegacyRoot }
    return Get-ProductRoot
}

# ---------------------------------------------------------------------------------------------
# The host. Every CLI call reads no input, and its JSON reply is parsed, never executed.

# Windows PowerShell 5.1 turns a native program's standard error into a terminating error under
# 'Stop'; native calls run with 'Continue' and are judged by their exit code.
function Invoke-Native {
    $ErrorActionPreference = 'Continue'
    $program, $rest = $args
    $output = '' | & $program @rest 2>&1 | Out-String
    return [pscustomobject] @{ Code = $LASTEXITCODE; Text = $output.Trim() }
}

function Test-GhSignedIn {
    if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { return $false }
    return (Invoke-Native gh auth status).Code -eq 0
}

# The first of these executables in the bin directory. The names of the release being installed
# come first, then either layout's, so an installation from before the rename is stopped and
# removed with its own.
function Get-Present {
    foreach ($name in $args) {
        $exe = Join-Path $Bin "$name.exe"
        if (Test-Path -LiteralPath $exe -PathType Leaf) { return $exe }
    }
    return $null
}

function Get-CliPath { return Get-Present $Cli 'kuroko' 'pta' }

function Invoke-Cli {
    $ErrorActionPreference = 'Continue'
    $exe = Get-CliPath
    if (-not $exe) { return [pscustomobject] @{ Code = 1; Text = '' } }
    # Standard output only: a JSON reply is one line there, and notices go to standard error.
    $output = '' | & $exe @args 2>$null | Out-String
    return [pscustomobject] @{ Code = $LASTEXITCODE; Text = $output.Trim() }
}

function Get-CliReply {
    $result = Invoke-Cli '--json' @args
    try { return $result.Text | ConvertFrom-Json } catch { return $null }
}

function Test-HostRunning {
    if (-not (Get-CliPath)) { return $false }
    $reply = Get-CliReply 'status'
    return [bool] ($reply -and $reply.data -and $reply.data.host_running)
}

function Wait-Host([bool] $Running) {
    for ($i = 0; $i -lt 60; $i++) {
        if ((Test-HostRunning) -eq $Running) { return $true }
        Start-Sleep -Seconds 1
    }
    return $false
}

function Stop-AppHost {
    if (Test-HostRunning) {
        Say "Stopping the running host (the assistant's last state is kept)..."
        Invoke-Cli '--json' 'app' 'quit' | Out-Null
        if (-not (Wait-Host $false)) { Warn 'The host did not stop within a minute.' }
    }
}

# A host started from this session as the user (a GUI-subsystem program: no console window).
function Start-AppHost {
    if (Test-HostRunning) { return $true }
    $exe = Get-Present $HostBin 'kuroko-host' $Legacy
    if (-not $exe) { return $false }
    Start-Process -FilePath $exe -ArgumentList '--host' -WindowStyle Hidden | Out-Null
    return (Wait-Host $true)
}

function Install-HostService {
    if (-not (Start-AppHost)) {
        Warn "The host did not start; run '$Cli status' to see why. It was not registered to start at login."
        return
    }
    $reply = Get-CliReply 'service' 'install' '--confirmed'
    if (-not $reply -or -not $reply.ok) {
        $message = if ($reply) { $reply.message } else { 'no answer' }
        Warn "The host was not registered to start at login: $message"
        return
    }
    $name = $reply.data.name
    # Hand the host over to the Task Scheduler, so it runs now as it will after a login.
    Stop-AppHost
    $started = Invoke-Native schtasks.exe /Run /TN $name
    if ($started.Code -eq 0 -and (Wait-Host $true)) {
        Say "The host runs as a login task ($name)."
    } else {
        Warn 'The Task Scheduler did not start the host now; it starts at your next login.'
        Start-AppHost | Out-Null
    }
}

# Unattended updating is turned on only when the person passed -UnattendedUpdates.
function Enable-UnattendedUpdates {
    if (-not $WantUnattended) {
        Say "Unattended updates are off. To let this computer install new releases by itself: $Cli update unattended on --confirmed"
        return
    }
    if (-not (Start-AppHost)) {
        Warn "The host did not start, so unattended updates were not turned on. Later: $Cli update unattended on --confirmed"
        return
    }
    $reply = Get-CliReply 'update' 'unattended' 'on' '--confirmed'
    if (-not $reply -or -not $reply.ok) {
        $message = if ($reply) { $reply.message } else { 'no answer' }
        Warn "Unattended updates were not turned on: $message"
        return
    }
    Say 'Unattended updates are on: this computer installs new releases by itself when nothing would be interrupted.'
}

# ---------------------------------------------------------------------------------------------
# PATH for new shells: the user PATH in the registry, keeping its type and every other entry. It
# holds one entry for this product, the current root's bin directory: the default roots' entries
# (before and after the rename) are replaced by it.

function Get-UserPath {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
    try {
        if ($null -eq $key) { return '' }
        return [string] $key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    } finally {
        if ($key) { $key.Close() }
    }
}

function Set-UserPath([string] $Value) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
    try {
        $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
        if ($key.GetValueNames() -contains 'Path') { $kind = $key.GetValueKind('Path') }
        if ($Value) { $key.SetValue('Path', $Value, $kind) } else { $key.DeleteValue('Path', $false) }
    } finally {
        $key.Close()
    }
    # Tell running programs (Explorer, new terminals) that the environment changed.
    if (-not ('KurokoInstall.Environment' -as [type])) {
        Add-Type -Namespace KurokoInstall -Name Environment -MemberDefinition @'
[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Auto)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
'@
    }
    $result = [UIntPtr]::Zero
    [KurokoInstall.Environment]::SendMessageTimeout([IntPtr] 0xffff, 0x1A, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref] $result) | Out-Null
}

function Test-OurEntry([string] $Entry) { return $Entry.TrimEnd('\') -ieq $Bin }

# The bin directory of a default root other than this installation's.
function Test-OtherDefaultEntry([string] $Entry) {
    $trimmed = $Entry.TrimEnd('\')
    foreach ($defaultRoot in (Get-LegacyRoot), (Get-ProductRoot)) {
        $defaultBin = Join-Path $defaultRoot 'bin'
        if ($defaultBin -ine $Bin -and $trimmed -ieq $defaultBin) { return $true }
    }
    return $false
}

# The value is kept as it is, empty entries and separators included: adding prepends one entry and
# removing takes out only this product's entries, so an install and an uninstall leave it byte for
# byte.
function Add-UserPath {
    $current = Get-UserPath
    $kept = @($current -split ';' | Where-Object { -not (Test-OtherDefaultEntry $_) })
    $value = $kept -join ';'
    if (@($kept | Where-Object { Test-OurEntry $_ }).Count -gt 0) {
        Say "$Bin is already on your user PATH."
    } else {
        $value = if ($value) { "$Bin;$value" } else { $Bin }
        Say "Added $Bin to your user PATH (new terminals see it)."
    }
    if ($value -ne $current) { Set-UserPath $value }
    if (-not (($env:Path -split ';') | Where-Object { Test-OurEntry $_ })) { $env:Path = "$Bin;$env:Path" }
}

# This installation's entry, and a default root's entry whose bin directory no longer exists.
function Remove-UserPath {
    $current = Get-UserPath
    $parts = @($current -split ';')
    $kept = @($parts | Where-Object {
            -not ((Test-OurEntry $_) -or ((Test-OtherDefaultEntry $_) -and -not (Test-Path -LiteralPath $_)))
        })
    if ($kept.Count -ne $parts.Count) { Set-UserPath ($kept -join ';') }
}

# ---------------------------------------------------------------------------------------------
# Coding CLIs: the skill and `kuroko mcp`, with consent; the exact commands otherwise. Both are
# named `kuroko` ($Integration); a registration under the former name is left for
# `kuroko mcp migrate`, and installing the skill beside its former folder removes that copy.

function Register-Integrations {
    $config = Get-CliReply 'mcp' 'config'
    $command = if ($config -and $config.data -and $config.data.command) { $config.data.command } else { Join-Path $Bin "$Cli.exe" }
    $found = $false
    foreach ($tool in 'claude', 'codex') {
        if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { continue }
        $found = $true
        $skill = Join-Path $env:USERPROFILE ".$tool\skills\$Integration"
        $mcp = if ($tool -eq 'claude') { @('mcp', 'add', '--scope', 'user', $Integration, '--', $command, 'mcp') } else { @('mcp', 'add', $Integration, '--', $command, 'mcp') }
        if (-not $WantIntegrations) {
            Say "  ${tool}: $Cli skill install `"$skill`""
            $shown = foreach ($word in $mcp) { if ($word -match '\s') { '"' + $word + '"' } else { $word } }
            Say ("  ${tool}: $tool " + ($shown -join ' '))
            continue
        }
        if (Test-Path -LiteralPath $skill) {
            Say "${tool}: the skill is already at $skill (updates refresh it while unmodified)."
        } else {
            New-Item -ItemType Directory -Force -Path (Split-Path $skill -Parent) | Out-Null
            $installed = Invoke-Cli 'skill' 'install' $skill
            if ($installed.Code -eq 0) { Say "${tool}: installed the skill at $skill." } else { Warn "${tool}: the skill could not be installed at $skill." }
        }
        if ((Invoke-Native $tool mcp get $Integration).Code -eq 0) {
            Say "${tool}: $Integration is already registered as an MCP server."
        } elseif ((Invoke-Native $tool mcp get $Legacy).Code -eq 0) {
            Say "${tool}: the MCP server is registered under its former name $Legacy; run '$Cli mcp migrate --confirmed' to move it."
        } else {
            if ((Invoke-Native $tool @mcp).Code -eq 0) { Say "${tool}: registered '$command mcp' as the MCP server $Integration." } else { Warn "${tool}: registering the MCP server failed." }
        }
    }
    if (-not $found) {
        Say "No coding CLI (Claude Code, Codex) was found. For any agent, '$Cli mcp config' prints how to add '$Cli mcp'."
    } elseif (-not $WantIntegrations) {
        Say 'Run them yourself, or run this command again with -Integrations.'
    }
}

# ---------------------------------------------------------------------------------------------
# Joining the account.

function Join-Account {
    $settings = Get-CliReply 'tools' 'call' 'machine_settings'
    $current = if ($settings -and $settings.data -and $settings.data.coordinator) { ([string] $settings.data.coordinator).TrimEnd('/') } else { '' }
    if ($current) {
        if ($current -eq $Origin) { Say "This computer is already connected to $Origin; its machine identity is kept." }
        else { Warn "This computer is connected to $current. Disconnect it there first ('$Cli tools call machine_disconnect'), then run this command again." }
        return
    }
    $reply = Get-CliReply 'auth' 'machine' 'login' $Origin '--no-browser'
    $url = if ($reply -and $reply.data) { [string] $reply.data.approval_url } else { '' }
    if (-not $url.StartsWith("$Origin/login?enroll=")) {
        $message = if ($reply) { $reply.message } else { 'no answer' }
        Warn "The sign-in could not start: $message"
        Warn "Run '$Cli auth machine login $Origin' when it is fixed."
        return
    }
    Say ''
    Say "Approve this computer in your browser: $url"
    Say "Check that the page shows the code $($reply.data.code), then choose Trust this machine."
    if ($SkipBrowser) { Say 'Open that link on any device where you are signed in to the portal.' }
    else { Start-Process $url }
    Say 'Waiting for the approval (up to ten minutes)...'
    $finished = Get-CliReply 'auth' 'machine' 'finish' '--wait'
    if ($finished -and $finished.ok -and $finished.data.connected) {
        Say "Connected to $Origin. Choose this computer's allowed directories in the portal (Settings > Machines) before running work on it."
    } else {
        $message = if ($finished) { $finished.message } else { 'no approval' }
        Warn "Not connected yet: $message. After approving, run '$Cli auth machine finish --wait'."
    }
}

# ---------------------------------------------------------------------------------------------
# Install and uninstall.

function Install-Archive {
    $arch = Get-Architecture
    if ($arch -ne 'AMD64') {
        Fail "There is no prebuilt archive for Windows on $arch. Install with Cargo instead: cargo install $Product --locked (needs the Rust toolchain from https://rustup.rs)."
    }
    Assert-NoCargoCopy
    if (-not $Version) {
        if ($TestSource) {
            $found = Get-ChildItem -LiteralPath $TestSource -Filter "$Package-$Target-v*.zip" | Select-Object -First 1
            if ($found -and $found.Name -match '-v([0-9.]+)\.zip$') { $script:Version = $Matches[1] }
        } else {
            $script:Version = Get-LatestVersion
        }
    }
    Test-Version $Version
    if (Test-BeforeRename $Version) { Use-LegacyLayout }
    $archive = "$Package-$Target-v$Version.zip"
    Say "Installing $Package $Version for $Target into $Root."

    New-Item -ItemType Directory -Force -Path $Root | Out-Null
    $stage = Join-Path $Root (".stage." + [Guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory -Path $stage | Out-Null
    try {
        $zip = Join-Path $stage $archive
        $sums = "$zip.sha256"
        if (-not (Get-Asset $archive $zip) -or -not (Get-Asset "$archive.sha256" $sums)) {
            Fail "Release $Version has no archive for $Target, or it could not be downloaded. Nothing was changed. Check the version and the network connection and try again."
        }
        $line = (Get-Content -LiteralPath $sums -TotalCount 1).Trim() -split '\s+'
        $expected = $line[0].ToLowerInvariant()
        $named = if ($line.Count -gt 1) { $line[1].TrimStart('*') } else { '' }
        $digest = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($named -ne $archive -or $digest -ne $expected) {
            Fail "The download does not match the release's published SHA-256. Nothing was changed. Try again; if it keeps failing, report it at https://github.com/$Repository/issues."
        }
        Say 'The archive matches its published SHA-256.'
        if ($TestSource) {
            Say 'Build provenance not checked (test source).'
        } elseif (Test-GhSignedIn) {
            if ((Invoke-Native gh attestation verify $zip --repo $Provenance).Code -ne 0) { Fail "GitHub could not verify the archive's build provenance. Nothing was changed. Run 'gh attestation verify' on the archive to see why." }
            Say 'Build provenance verified with gh attestation verify.'
        } else {
            Say 'Build provenance not checked: install the GitHub CLI (gh) and sign in to verify it. The SHA-256 matched.'
        }

        $unpacked = Join-Path $stage 'unpacked'
        $newBin = Join-Path $stage 'bin'
        New-Item -ItemType Directory -Path $newBin | Out-Null
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        try { [System.IO.Compression.ZipFile]::ExtractToDirectory($zip, $unpacked) } catch { Fail 'The archive could not be unpacked. Nothing was changed.' }
        foreach ($name in $Bins) {
            $file = Join-Path $unpacked "$Package-$Target-v$Version\$name.exe"
            if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { Fail "The archive does not contain $name.exe. Nothing was changed." }
            Copy-Item -LiteralPath $file -Destination (Join-Path $newBin "$name.exe")
        }
        $reported = (Invoke-Native (Join-Path $newBin "$Cli.exe") --version --json).Text
        if ($reported -notmatch [regex]::Escape("`"cli_version`":`"$Version`"")) {
            Fail "The unpacked $Cli does not run on this computer or reports another version. Nothing was changed."
        }

        if (Test-Path -LiteralPath (Get-ProfileDir)) { Say "Using the existing profile at $(Get-ProfileDir); it is kept as it is." }
        # Only now does anything installed change: stop the old host, swap the bin directory. A
        # running executable cannot be replaced on Windows, so the host is stopped first.
        Stop-AppHost
        $previous = Join-Path $stage 'previous'
        if (Test-Path -LiteralPath $Bin) {
            try { Move-Item -LiteralPath $Bin -Destination $previous } catch { Fail "The previous installation could not be replaced (is a $Cli command still running?). Nothing was changed." }
        }
        try {
            Move-Item -LiteralPath $newBin -Destination $Bin
        } catch {
            if (Test-Path -LiteralPath $previous) { Move-Item -LiteralPath $previous -Destination $Bin }
            Fail 'The new binaries could not be moved into place; the previous installation was restored.'
        }
        $recordJson = [ordered] @{
            schema = 1; kind = 'archive'; package = $Package; version = $Version; target = $Target
            bins = @($Bins); repository = $RecordRepository; archive = $archive
            sha256 = $digest; installed_at = [DateTime]::UtcNow.ToString('yyyy-MM-ddTHH:mm:ssZ')
        } | ConvertTo-Json
        $recordFile = Join-Path $stage $Record
        [System.IO.File]::WriteAllText($recordFile, $recordJson, (New-Object System.Text.UTF8Encoding $false))
        Move-Item -LiteralPath $recordFile -Destination (Join-Path $Root $Record) -Force
    } finally {
        Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
    }
    Say "Installed $((Invoke-Cli '--version').Text) in $Bin."
    Clear-StaleBinstallRecord

    Add-UserPath
    if ($SkipService) { Say 'Not registered to start at login (-NoService).' } else { Install-HostService }
    Enable-UnattendedUpdates
    if ($Origin) { Join-Account }
    Say ''
    Say 'Skill and MCP for coding CLIs on this computer:'
    Register-Integrations
    Say ''
    Say "Open a new terminal to use '$Cli'. To remove the binaries later, run this script with -Uninstall (the profile is kept)."
}

# The `kk` alias block the host writes on the first start of each version
# (src/app/service/shell_alias.rs), in both PowerShell editions' profiles and Git Bash's files.
function Remove-ShellAlias {
    $documents = [Environment]::GetFolderPath('MyDocuments')
    $files = @()
    if ($documents) { $files += @((Join-Path $documents 'PowerShell\profile.ps1'), (Join-Path $documents 'WindowsPowerShell\profile.ps1')) }
    $files += @('.bashrc', '.bash_profile', '.zshrc' | ForEach-Object { Join-Path $env:USERPROFILE $_ })
    foreach ($file in $files) {
        if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { continue }
        $lines = @(Get-Content -LiteralPath $file -Encoding UTF8)
        if ($lines -notcontains '# >>> kuroko alias >>>') { continue }
        $skip = $false
        $kept = foreach ($line in $lines) {
            if ($line -eq '# >>> kuroko alias >>>') { $skip = $true; continue }
            if ($line -eq '# <<< kuroko alias <<<') { $skip = $false; continue }
            if (-not $skip) { $line }
        }
        [IO.File]::WriteAllLines($file, [string[]] @($kept))
    }
}

function Uninstall-Archive {
    $recordFile = Join-Path $Root $Record
    if (-not (Test-Path -LiteralPath $recordFile) -or ((Get-Content -LiteralPath $recordFile -Raw | ConvertFrom-Json).kind -ne 'archive')) {
        Fail "No installation by this script was found in $Root."
    }
    if (Get-CliPath) {
        if (Start-AppHost) {
            $reply = Get-CliReply 'service' 'uninstall' '--confirmed'
            if (-not $reply -or -not $reply.ok) { Warn "The login task could not be removed; remove it with '$Cli service uninstall --confirmed'." }
        }
        Stop-AppHost
    }
    Remove-UserPath
    Remove-ShellAlias
    Remove-Item -LiteralPath $Bin -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $recordFile -Force
    if (-not (Get-ChildItem -LiteralPath $Root -Force)) { Remove-Item -LiteralPath $Root -Force }
    Say "Removed Kuroko from $Root. The profile at $(Get-ProfileDir) was kept."
    Say "A skill copy or MCP registration made for a coding CLI stays; remove it there (for example 'claude mcp remove $Integration')."
}

# ---------------------------------------------------------------------------------------------

try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    if ($TestSource) {
        if ((Get-EnvValue 'INSTALL_TEST') -ne '1') { Fail '-TestSource is only for the installer''s own tests.' }
        if (-not (Test-Path -LiteralPath $TestSource -PathType Container)) { Fail 'The test source must be a local directory.' }
        $TestSource = (Resolve-Path -LiteralPath $TestSource).Path
        Warn "TEST MODE: installing from $TestSource instead of the release."
    }
    if ($Origin) { $Origin = Test-Origin $Origin }
    if ($Version) { Test-Version $Version }
    if (-not $Root) { $Root = Get-EnvValue 'INSTALL_ROOT' }
    if (-not $Root) { $Root = Get-DefaultRoot }
    $Root = [System.IO.Path]::GetFullPath($Root).TrimEnd('\')
    $Bin = Join-Path $Root 'bin'
    if ($Uninstall) { Uninstall-Archive } else { Install-Archive }
} catch {
    Write-Host $_.Exception.Message -ForegroundColor Red
    exit 1
}
exit 0
