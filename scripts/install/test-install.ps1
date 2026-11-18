<#
Runs install.ps1 against the archive this workflow just built, the way a person would, into a
temporary installation root and a temporary profile (KUROKO_PROFILE_DIR): never a real profile, and
never the network (the script's test source, enabled only with KUROKO_INSTALL_TEST=1).

  powershell -NoProfile -ExecutionPolicy Bypass -File scripts/install/test-install.ps1 -Dist DIST -Version VERSION

DIST holds kuroko-x86_64-pc-windows-msvc-v<VERSION>.zip and its .sha256. With
KUROKO_INSTALL_TEST_RELEASE=X.Y.Z (or the former PTA_INSTALL_TEST_RELEASE) it also installs that
published release from GitHub through the real download path.
#>
param(
    [Parameter(Mandatory = $true)] [string] $Dist,
    [Parameter(Mandatory = $true)] [string] $Version
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 1.0

$script = Join-Path $PSScriptRoot 'install.ps1'
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("kuroko-install-test-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$root = Join-Path $work 'root'
$env:KUROKO_INSTALL_TEST = '1'
$env:KUROKO_PROFILE_DIR = Join-Path $work 'profile'
# Builds before the rename (the published release below) read only the former name.
$env:PTA_PROFILE_DIR = $env:KUROKO_PROFILE_DIR
$env:KUROKO_INSTALL_ROOT = ''
$env:PTA_INSTALL_ROOT = ''
$env:CARGO_HOME = Join-Path $work 'cargo'
$release = if ($env:KUROKO_INSTALL_TEST_RELEASE) { $env:KUROKO_INSTALL_TEST_RELEASE } else { $env:PTA_INSTALL_TEST_RELEASE }
# The upgrade from an installation made before the rename runs with a LOCALAPPDATA of its own, so
# the default roots it reads and writes are never the real ones.
$localAppData = Join-Path $work 'localappdata'
$legacy = Join-Path $localAppData 'Programs\personal-teams-assistant'
$legacyBin = Join-Path $legacy 'bin'
$script:failures = 0
$out = Join-Path $work 'out.txt'

function Check([string] $Description, [bool] $Ok) {
    if ($Ok) { Write-Host "ok   - $Description" } else { Write-Host "FAIL - $Description"; $script:failures++ }
}
# The installer as a person runs it: Windows PowerShell, a process-only execution policy.
function Run-Installer {
    $ErrorActionPreference = 'Continue'
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $script -Root $root @args *> $out
    return $LASTEXITCODE
}
# The installer with no root given and the separate LOCALAPPDATA, as a person upgrading would run it.
function Run-DefaultRoot {
    $ErrorActionPreference = 'Continue'
    $saved = $env:LOCALAPPDATA
    $env:LOCALAPPDATA = $localAppData
    try {
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $script @args *> $out
        return $LASTEXITCODE
    } finally {
        $env:LOCALAPPDATA = $saved
    }
}
function Said([string] $Text) { return [bool] (Select-String -LiteralPath $out -SimpleMatch $Text -Quiet) }
function Kuroko {
    $ErrorActionPreference = 'Continue'
    $output = '' | & (Join-Path $root 'bin\kuroko.exe') @args 2>&1 | Out-String
    return $output.Trim()
}
function UserPath {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
    try { return [string] $key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) } finally { $key.Close() }
}
function Set-TestUserPath([string] $Value) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
    try {
        $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
        if ($key.GetValueNames() -contains 'Path') { $kind = $key.GetValueKind('Path') }
        if ($Value) { $key.SetValue('Path', $Value, $kind) } else { $key.DeleteValue('Path', $false) }
    } finally {
        $key.Close()
    }
}
function PathEntries([string] $Entry) { return @((UserPath).Split(';') | Where-Object { $_.TrimEnd('\') -ieq $Entry }).Count }
$pathBefore = UserPath
$bins = 'kuroko', 'kuroko-host', 'personal-teams-assistant', 'pta'

try {
    $archive = "kuroko-x86_64-pc-windows-msvc-v$Version.zip"
    foreach ($dir in 'source', 'tampered', 'empty', 'profile', 'cargo') { New-Item -ItemType Directory -Force -Path (Join-Path $work $dir) | Out-Null }
    foreach ($dir in 'source', 'tampered') {
        Copy-Item (Join-Path $Dist $archive), (Join-Path $Dist "$archive.sha256") (Join-Path $work $dir)
    }
    $bytes = [System.IO.File]::ReadAllBytes((Join-Path $work "tampered\$archive"))
    $bytes[4096] = $bytes[4096] -bxor 0xff
    [System.IO.File]::WriteAllBytes((Join-Path $work "tampered\$archive"), $bytes)
    Set-Content -LiteralPath (Join-Path $env:KUROKO_PROFILE_DIR 'adopted.txt') -Value 'synthetic profile marker'

    Write-Host '# Refusals that change nothing'
    Check 'a digest mismatch is refused' ((Run-Installer -TestSource (Join-Path $work 'tampered')) -ne 0)
    Check '  and says what happened' (Said "does not match the release's published SHA-256")
    Check '  and installs nothing' (-not (Test-Path (Join-Path $root 'bin')))
    $env:KUROKO_INSTALL_TEST = ''
    $env:PTA_INSTALL_TEST = '1'
    Check 'the former PTA_INSTALL_TEST name still enables the test source' ((Run-Installer -TestSource (Join-Path $work 'tampered')) -ne 0)
    Check '  (it got as far as the digest)' (Said "does not match the release's published SHA-256")
    $env:PTA_INSTALL_TEST = ''
    $env:KUROKO_INSTALL_TEST = '1'
    Check 'a missing archive is refused' ((Run-Installer -TestSource (Join-Path $work 'empty') -Version $Version) -ne 0)
    Check '  and says what to do' (Said 'has no archive for')
    Set-Content -LiteralPath (Join-Path $env:CARGO_HOME '.crates2.json') -Value '{"installs":{"personal-teams-assistant 0.11.2 (registry+https://github.com/rust-lang/crates.io-index)":{"bins":["personal-teams-assistant.exe","pta.exe"]}}}'
    Check 'a second copy next to a Cargo installation is refused' ((Run-Installer -TestSource (Join-Path $work 'source')) -ne 0)
    Check '  and says how to switch' (Said 'cargo uninstall personal-teams-assistant')
    Check "  with that package's own CLI" (Said 'pta service uninstall --confirmed')
    Set-Content -LiteralPath (Join-Path $env:CARGO_HOME '.crates2.json') -Value '{"installs":{"kuroko 0.18.0 (registry+https://github.com/rust-lang/crates.io-index)":{"bins":["kuroko.exe","kuroko-host.exe","personal-teams-assistant.exe","pta.exe"]}}}'
    Check 'a second copy next to a Cargo installation of kuroko is refused' ((Run-Installer -TestSource (Join-Path $work 'source')) -ne 0)
    Check '  and says how to switch' (Said 'cargo uninstall kuroko')
    Check '  or how to stay with Cargo' (Said 'cargo install kuroko --locked')
    Remove-Item (Join-Path $env:CARGO_HOME '.crates2.json')
    # A live Cargo installation (its own record beside binstall's) is refused and binstall's record is left alone.
    $binstallDir = Join-Path $env:CARGO_HOME 'binstall'
    New-Item -ItemType Directory -Force -Path $binstallDir | Out-Null
    $binstallFile = Join-Path $binstallDir 'crates-v1.json'
    $other = '{"name":"other-crate","version_req":"*","current_version":"1.0.0","bins":["other"]}'
    $ours = '{"name":"personal-teams-assistant","version_req":"*","current_version":"0.11.2","bins":["personal-teams-assistant","pta"]}'
    $successor = '{"name":"kuroko","version_req":"*","current_version":"0.18.0","bins":["kuroko","kuroko-host","personal-teams-assistant","pta"]}'
    [System.IO.File]::WriteAllText($binstallFile, "$other`n$ours`n$successor`n", (New-Object System.Text.UTF8Encoding $false))
    $cratesToml = Join-Path $env:CARGO_HOME '.crates.toml'
    Set-Content -LiteralPath $cratesToml -Value "[v1]`n`"personal-teams-assistant 0.11.2 (registry+https://github.com/rust-lang/crates.io-index)`" = [`"pta.exe`"]"
    $binstallBefore = (Get-FileHash $binstallFile).Hash
    Check 'a live binstall installation is refused' ((Run-Installer -TestSource (Join-Path $work 'source')) -ne 0)
    Check '  and its binstall record is untouched' ((Get-FileHash $binstallFile).Hash -eq $binstallBefore)
    Remove-Item $cratesToml
    $env:KUROKO_INSTALL_TEST = ''
    Check 'the test source needs the test mode' ((Run-Installer -TestSource (Join-Path $work 'source')) -ne 0)
    $env:KUROKO_INSTALL_TEST = '1'
    Check 'a coordinator origin must be HTTPS' ((Run-Installer -TestSource (Join-Path $work 'source') -Origin 'http://portal.example') -ne 0)
    Check 'nothing was installed by any refusal' (-not (Test-Path (Join-Path $root 'bin')))
    Check 'the user PATH was not changed by any refusal' ((UserPath) -eq $pathBefore)

    Write-Host '# Install'
    $code = Run-Installer -TestSource (Join-Path $work 'source')
    Check 'install.ps1 installs' ($code -eq 0)
    if ($code -ne 0) { Get-Content $out | Write-Host }
    Check 'it checked the digest' (Said 'matches its published SHA-256')
    Check 'it says the existing profile is used' (Said 'Using the existing profile')
    Check 'it removed the stale binstall entries of both package names' (Said 'Removed the stale cargo binstall entry for personal-teams-assistant and kuroko')
    Check '  keeping the other entry byte for byte' (([System.IO.File]::ReadAllText($binstallFile)) -eq "$other`n")
    Check '  and leaving no temporary file' (@(Get-ChildItem $binstallDir).Count -eq 1)
    $staleAfter = (Get-FileHash $binstallFile).Hash
    Check 'kuroko --version from the installed path' ((Kuroko --version) -match [regex]::Escape($Version))
    foreach ($name in $bins) { Check "$name.exe is installed" (Test-Path (Join-Path $root "bin\$name.exe")) }
    $record = Get-Content -LiteralPath (Join-Path $root 'install.json') -Raw | ConvertFrom-Json
    Check 'the record marks an archive installation of the kuroko package, version and target' ($record.kind -eq 'archive' -and $record.package -eq 'kuroko' -and $record.version -eq $Version -and $record.target -eq 'x86_64-pc-windows-msvc')
    Check '  with its four executables' ((@($record.bins) -join ',') -eq ($bins -join ','))
    Check '  from its repository' ($record.repository -eq 'https://github.com/elvisbrevi/kuroko')
    Check '  with the archive digest' ($record.sha256 -eq (Get-FileHash (Join-Path $Dist $archive) -Algorithm SHA256).Hash.ToLowerInvariant())
    $update = Kuroko --json update status
    Check 'the updater recognizes the archive kind' ($update -match '"kind":"archive"')
    Check '  as a supported installation it updates itself' ($update -match '"supported":true')
    Check '  with unattended updates off by default' ($update -match '"unattended":\{[^}]*"enabled":false')
    Check '  and says how to turn them on' (Said 'kuroko update unattended on --confirmed')
    Check 'the user PATH (registry) starts with the bin directory' ((UserPath).Split(';')[0] -eq (Join-Path $root 'bin'))
    Check '  and keeps every previous entry' ((UserPath).EndsWith($pathBefore))
    $found = & powershell.exe -NoProfile -Command "`$env:Path = [Environment]::GetEnvironmentVariable('Path','Machine') + ';' + [Environment]::GetEnvironmentVariable('Path','User'); (Get-Command kuroko.exe).Source"
    Check "a new shell finds kuroko ($found)" ($found -eq (Join-Path $root 'bin\kuroko.exe'))
    Check 'the profile was adopted, not replaced' ((Get-Content (Join-Path $env:KUROKO_PROFILE_DIR 'adopted.txt')) -eq 'synthetic profile marker')
    $machine = (Kuroko --json tools call machine_settings | ConvertFrom-Json).data.id
    Check 'the installation has a machine identity' ([bool] $machine)
    $status = Kuroko --json service status | ConvertFrom-Json
    $name = $status.data.name
    Check "the login task is installed ($name)" ($status.data.installed -eq $true -and $status.data.current -eq $true)
    # A runner without an interactive session may not run a logon task; the definition still counts.
    Write-Host "     the task's host running now: $($status.data.running)"

    Write-Host '# Again: repair or upgrade in place'
    $before = (Get-FileHash (Join-Path $root 'install.json')).Hash
    Check 'a failed upgrade is refused' ((Run-Installer -TestSource (Join-Path $work 'tampered')) -ne 0)
    Check '  and leaves the installation working' ((Kuroko --version) -match [regex]::Escape($Version))
    Check '  and its record unchanged' ((Get-FileHash (Join-Path $root 'install.json')).Hash -eq $before)
    $code = Run-Installer -TestSource (Join-Path $work 'source') -UnattendedUpdates
    Check 'running it again succeeds' ($code -eq 0)
    if ($code -ne 0) { Get-Content $out | Write-Host }
    Check 'a second run leaves the binstall record unchanged' ((Get-FileHash $binstallFile).Hash -eq $staleAfter)
    Check '  and does not mention it' (-not (Said 'stale cargo binstall'))
    Check '-UnattendedUpdates turned unattended updates on' ((Kuroko --json update status) -match '"unattended":\{[^}]*"enabled":true')
    Check 'the bin directory is on the user PATH once' ((PathEntries (Join-Path $root 'bin')) -eq 1)
    Check 'the same machine identity is kept' ((Kuroko --json tools call machine_settings | ConvertFrom-Json).data.id -eq $machine)
    $status = Kuroko --json service status | ConvertFrom-Json
    Check 'the login task is still installed' ($status.data.installed -eq $true -and $status.data.current -eq $true)

    Write-Host '# Uninstall'
    $code = Run-Installer -Uninstall
    Check '-Uninstall succeeds' ($code -eq 0)
    if ($code -ne 0) { Get-Content $out | Write-Host }
    Check 'the binaries are removed' (-not (Test-Path (Join-Path $root 'bin')))
    Check 'the record is removed' (-not (Test-Path (Join-Path $root 'install.json')))
    Check 'the profile is kept' ((Get-Content (Join-Path $env:KUROKO_PROFILE_DIR 'adopted.txt')) -eq 'synthetic profile marker')
    Check 'the user PATH is as before' ((UserPath) -eq $pathBefore)
    $ErrorActionPreference = 'Continue'
    & schtasks.exe /Query /TN $name *> $null
    Check 'the login task is removed' ($LASTEXITCODE -ne 0)
    $ErrorActionPreference = 'Stop'

    # An archive installation made before the rename: the default root, two executables, its record
    # and its user PATH entry. Running the command again without a root upgrades it where it is to
    # the four executables, with one PATH entry.
    Write-Host '# Upgrade of an installation made before the rename'
    $unpacked = Join-Path $work 'unpacked'
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::ExtractToDirectory((Join-Path $Dist $archive), $unpacked)
    New-Item -ItemType Directory -Force -Path $legacyBin | Out-Null
    foreach ($name in 'personal-teams-assistant', 'pta') {
        Copy-Item -LiteralPath (Join-Path $unpacked "kuroko-x86_64-pc-windows-msvc-v$Version\$name.exe") -Destination (Join-Path $legacyBin "$name.exe")
    }
    $legacyRecord = [ordered] @{
        schema = 1; kind = 'archive'; package = 'personal-teams-assistant'; version = '0.17.0'; target = 'x86_64-pc-windows-msvc'
        bins = @('personal-teams-assistant', 'pta'); repository = 'https://github.com/elvisbrevi/personal-teams-assistant'
        archive = 'personal-teams-assistant-x86_64-pc-windows-msvc-v0.17.0.zip'; sha256 = ('0' * 64); installed_at = '2026-11-01T00:00:00Z'
    } | ConvertTo-Json
    [System.IO.File]::WriteAllText((Join-Path $legacy 'install.json'), $legacyRecord, (New-Object System.Text.UTF8Encoding $false))
    Set-TestUserPath $(if ($pathBefore) { "$legacyBin;$pathBefore" } else { $legacyBin })
    $code = Run-DefaultRoot -NoService -TestSource (Join-Path $work 'source')
    Check 'install.ps1 upgrades it' ($code -eq 0)
    if ($code -ne 0) { Get-Content $out | Write-Host }
    Check '  in its own root' (Said "into $legacy.")
    Check '  without creating the new default root' (-not (Test-Path (Join-Path $localAppData 'Programs\kuroko')))
    foreach ($name in $bins) { Check "  $name.exe is installed there" (Test-Path (Join-Path $legacyBin "$name.exe")) }
    Check '  and kuroko runs' (((& (Join-Path $legacyBin 'kuroko.exe') --version) | Out-String) -match [regex]::Escape($Version))
    $record = Get-Content -LiteralPath (Join-Path $legacy 'install.json') -Raw | ConvertFrom-Json
    Check 'the record names the kuroko package and the installed version' ($record.package -eq 'kuroko' -and $record.version -eq $Version)
    Check '  with its four executables' ((@($record.bins) -join ',') -eq ($bins -join ','))
    Check '  from its repository' ($record.repository -eq 'https://github.com/elvisbrevi/kuroko')
    Check 'the bin directory is on the user PATH once' ((PathEntries $legacyBin) -eq 1)
    Check '  with no entry for the new default root' ((PathEntries (Join-Path $localAppData 'Programs\kuroko\bin')) -eq 0)
    Check '  and every other entry kept' ((UserPath) -eq $(if ($pathBefore) { "$legacyBin;$pathBefore" } else { $legacyBin }))
    $found = & powershell.exe -NoProfile -Command "`$env:Path = [Environment]::GetEnvironmentVariable('Path','Machine') + ';' + [Environment]::GetEnvironmentVariable('Path','User'); (Get-Command kuroko.exe).Source"
    Check "a new shell finds kuroko there ($found)" ($found -eq (Join-Path $legacyBin 'kuroko.exe'))
    Check 'the profile was not touched' ((Get-Content (Join-Path $env:KUROKO_PROFILE_DIR 'adopted.txt')) -eq 'synthetic profile marker')
    $code = Run-DefaultRoot -Uninstall
    Check '-Uninstall finds and removes it' ($code -eq 0)
    if ($code -ne 0) { Get-Content $out | Write-Host }
    Check '  the binaries are removed' (-not (Test-Path $legacyBin))
    Check '  the record is removed' (-not (Test-Path (Join-Path $legacy 'install.json')))
    Check '  the user PATH is as before' ((UserPath) -eq $pathBefore)
    Check '  the profile is kept' ((Get-Content (Join-Path $env:KUROKO_PROFILE_DIR 'adopted.txt')) -eq 'synthetic profile marker')

    # The real download path (release redirect, asset host, digest) for a version already published.
    # A release before 0.18.0 is installed in its own layout, which also has pta.exe.
    if ($release) {
        Write-Host '# A published release, downloaded from GitHub'
        $net = Join-Path $work 'release-root'
        $ErrorActionPreference = 'Continue'
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $script -Root $net -NoService -Version $release *> $out
        $code = $LASTEXITCODE
        Check "release $release installs from GitHub" ($code -eq 0)
        if ($code -ne 0) { Get-Content $out | Write-Host }
        Check '  after matching its published SHA-256' (Said 'matches its published SHA-256')
        Check '  and runs' (((& (Join-Path $net 'bin\pta.exe') --version) | Out-String) -match [regex]::Escape($release))
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $script -Root $net -Uninstall *> $out
        $uninstalled = $LASTEXITCODE -eq 0 -and -not (Test-Path (Join-Path $net 'bin'))
        Check '  and uninstalls' $uninstalled
        if (-not $uninstalled) { Get-Content $out | Write-Host }
        Check '  and leaves the user PATH as before' ((UserPath) -eq $pathBefore)
        $ErrorActionPreference = 'Stop'
    }
} finally {
    if (Test-Path (Join-Path $root 'bin\kuroko.exe')) { Run-Installer -Uninstall | Out-Null }
    if ((Test-Path (Join-Path $legacyBin 'kuroko.exe')) -or (Test-Path (Join-Path $legacyBin 'pta.exe'))) { Run-DefaultRoot -Uninstall | Out-Null }
    if ((UserPath) -ne $pathBefore) { Set-TestUserPath $pathBefore }
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

if ($script:failures -gt 0) { Write-Host "$($script:failures) installer check(s) failed."; exit 1 }
Write-Host 'Every installer check passed.'
