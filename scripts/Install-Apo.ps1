<#
.SYNOPSIS
  Install (or uninstall) a windows-apo DLL as an effect on one audio endpoint.

.DESCRIPTION
  Must run elevated. Steps:
    1. Copy the DLL to $env:ProgramData\<Name>\ (readable by audiodg.exe).
    2. regsvr32 it (COM + AudioProcessingObjects registration).
    3. Take ownership of the endpoint's registry key and FxProperties (owned by
       SYSTEM / TrustedInstaller) and grant Administrators full control.
    4. Back up every FxProperties value it will touch to
       $env:ProgramData\<Name>\backup-<endpoint>.json.
    5. Write the effect CLSID into the chosen slot (legacy and composite keys)
       with its processing modes, and clear "disable enhancements".
    6. Restart AudioEndpointBuilder + Audiosrv, and re-apply if the endpoint
       builder reset the values.
  -Uninstall restores the backup exactly (values that were absent are removed).

  Unsigned DLLs only load while
  HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio\DisableProtectedAudioDG = 1;
  -AllowUnsignedAudioDG sets it (a system-wide downgrade; -Uninstall leaves it).

.EXAMPLE
  .\Install-Apo.ps1 -Dll .\my_apo.dll -Name MyApo -Clsid '{...}' -Flow Capture -Endpoint '{3f135177-...}' -Slot EFX
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $Name,
    [Parameter(Mandatory)] [string] $Clsid,
    [Parameter(Mandatory)] [ValidateSet('Render', 'Capture')] [string] $Flow,
    [Parameter(Mandatory)] [string] $Endpoint,
    [string] $Dll,
    [ValidateSet('SFX', 'MFX', 'EFX')] [string] $Slot = 'SFX',
    [ValidateSet('DEFAULT', 'RAW', 'COMMUNICATIONS', 'SPEECH', 'MEDIA', 'MOVIE', 'NOTIFICATION')]
    [string[]] $Modes = @('DEFAULT'),
    [switch] $AllowUnsignedAudioDG,
    [switch] $Uninstall,
    [switch] $Unregister,
    [string] $Log
)
$ErrorActionPreference = 'Stop'

$dataDir = Join-Path $env:ProgramData $Name
New-Item -ItemType Directory -Force $dataDir | Out-Null
if (-not $Log) { $Log = Join-Path $dataDir 'install.log' }
function Say($msg) { $line = "[$(Get-Date -Format HH:mm:ss)] $msg"; Write-Host $line; Add-Content -Path $Log -Value $line -Encoding utf8 }

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Install-Apo.ps1 must run elevated.' }

$Endpoint = $Endpoint.ToLowerInvariant()
if ($Endpoint -notmatch '^\{[0-9a-f-]{36}\}$') { $Endpoint = "{$($Endpoint.Trim('{}'))}" }
$Clsid = $Clsid.ToUpperInvariant()
if ($Clsid -notmatch '^\{') { $Clsid = "{$Clsid}" }

$endpointSub = "SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\$Flow\$Endpoint"
$fxSub = "$endpointSub\FxProperties"
$fxPath = "HKLM:\$fxSub"
if (-not (Test-Path "HKLM:\$endpointSub")) { throw "No $Flow endpoint $Endpoint" }

$modeGuids = @{
    DEFAULT = '{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}'; RAW = '{9E90EA20-B493-4FD1-A1A8-7E1361A956CF}'
    COMMUNICATIONS = '{98951333-B9CD-48B1-A0A3-FF40682D73F7}'; SPEECH = '{FC1CFC9B-B9D6-4CFA-B5E0-4BB2166878B2}'
    MEDIA = '{4780004E-7133-41D8-8C74-660DADD2C0EE}'; MOVIE = '{B26FEB0D-EC94-477C-9494-D1AB8E753F6E}'
    NOTIFICATION = '{9CF2A70B-F377-403B-BD6B-360863E0355C}'
}
# PKEY_FX_{Stream,Mode,Endpoint}EffectClsid = {d04e05a6-...},5/6/7
# PKEY_CompositeFX_{Stream,Mode,Endpoint}EffectClsid = {d04e05a6-...},13/14/15
# PKEY_{SFX,MFX,EFX}_ProcessingModes_Supported_For_Streaming = {d3993a3f-...},5/6/7
$fx = '{d04e05a6-594b-4fb6-a80d-01af5eed7d1d}'
$pm = '{d3993a3f-99c2-4402-b5ec-a92a0367664b}'
$slotIndex = @{ SFX = 5; MFX = 6; EFX = 7 }[$Slot]
$legacyKey = "$fx,$slotIndex"
$compositeKey = "$fx,$($slotIndex + 8)"
$modesKey = "$pm,$slotIndex"
$disableEnhancements = '{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},5'
$touched = @($legacyKey, $compositeKey, $modesKey, $disableEnhancements)
$backupFile = Join-Path $dataDir "backup-$($Flow.ToLower())-$($Endpoint.Trim('{}')).json"

# ── Registry ownership (the EqualizerAPO technique) ────────────────────
Add-Type -ErrorAction SilentlyContinue -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class ApoTokenPrivilege {
    [DllImport("advapi32.dll", SetLastError=true)] static extern bool OpenProcessToken(IntPtr h, uint access, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError=true)] static extern bool LookupPrivilegeValue(string sys, string name, out long luid);
    [DllImport("advapi32.dll", SetLastError=true)] static extern bool AdjustTokenPrivileges(IntPtr token, bool disableAll, ref TP state, int len, IntPtr prev, IntPtr retLen);
    [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
    [StructLayout(LayoutKind.Sequential, Pack=1)] struct TP { public int Count; public long Luid; public int Attr; }
    public static bool Enable(string privilege) {
        IntPtr token; if (!OpenProcessToken(GetCurrentProcess(), 0x28, out token)) return false;
        TP tp = new TP(); tp.Count = 1; tp.Attr = 2;
        if (!LookupPrivilegeValue(null, privilege, out tp.Luid)) return false;
        return AdjustTokenPrivileges(token, false, ref tp, 0, IntPtr.Zero, IntPtr.Zero) && Marshal.GetLastWin32Error() == 0;
    }
}
'@
[void][ApoTokenPrivilege]::Enable('SeTakeOwnershipPrivilege')
[void][ApoTokenPrivilege]::Enable('SeRestorePrivilege')
$admins = New-Object Security.Principal.SecurityIdentifier('S-1-5-32-544')

function Grant-AdminControl([string] $sub) {
    $hklm = [Microsoft.Win32.Registry]::LocalMachine
    $k = $hklm.OpenSubKey($sub, [Microsoft.Win32.RegistryKeyPermissionCheck]::ReadWriteSubTree, [Security.AccessControl.RegistryRights]::TakeOwnership)
    if (-not $k) { return $false }
    $acl = $k.GetAccessControl([Security.AccessControl.AccessControlSections]::None)
    $acl.SetOwner($admins); $k.SetAccessControl($acl); $k.Close()
    $k = $hklm.OpenSubKey($sub, [Microsoft.Win32.RegistryKeyPermissionCheck]::ReadWriteSubTree, [Security.AccessControl.RegistryRights]::ChangePermissions)
    $acl = $k.GetAccessControl()
    $rule = New-Object Security.AccessControl.RegistryAccessRule($admins, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
    $acl.AddAccessRule($rule); $k.SetAccessControl($acl); $k.Close()
    return $true
}

function Read-Value([string] $name) {
    if (-not (Test-Path $fxPath)) { return $null }
    $item = Get-Item $fxPath
    if ($item.GetValueNames() -notcontains $name) { return $null }
    $kind = $item.GetValueKind($name).ToString()
    return [pscustomobject]@{ kind = $kind; value = $item.GetValue($name, $null, 'DoNotExpandEnvironmentNames') }
}

function Write-Value([string] $name, $entry) {
    if ($null -eq $entry) { Remove-ItemProperty -Path $fxPath -Name $name -ErrorAction SilentlyContinue; return }
    $type = @{ String = 'String'; MultiString = 'MultiString'; DWord = 'DWord'; Binary = 'Binary'; ExpandString = 'ExpandString'; QWord = 'QWord' }[$entry.kind]
    $value = $entry.value
    if ($entry.kind -eq 'MultiString') { $value = [string[]]@($value) }
    if ($entry.kind -eq 'Binary') { $value = [byte[]]@($value) }
    New-ItemProperty -Path $fxPath -Name $name -PropertyType $type -Value $value -Force | Out-Null
}

function Restart-Audio {
    Say 'Restarting AudioEndpointBuilder and Audiosrv'
    Stop-Service -Name Audiosrv -Force
    Restart-Service -Name AudioEndpointBuilder -Force
    Start-Service -Name Audiosrv
    Start-Sleep -Seconds 2
}

function Apply-Ours {
    New-ItemProperty -Path $fxPath -Name $legacyKey -PropertyType String -Value $Clsid -Force | Out-Null
    New-ItemProperty -Path $fxPath -Name $compositeKey -PropertyType MultiString -Value ([string[]]@($Clsid)) -Force | Out-Null
    New-ItemProperty -Path $fxPath -Name $modesKey -PropertyType MultiString -Value ([string[]]($Modes | ForEach-Object { $modeGuids[$_] })) -Force | Out-Null
    Remove-ItemProperty -Path $fxPath -Name $disableEnhancements -ErrorAction SilentlyContinue
}

function Ours-Present {
    $v = Read-Value $compositeKey
    return ($null -ne $v) -and (@($v.value) -contains $Clsid)
}

Say "=== $(if ($Uninstall) { 'Uninstall' } else { 'Install' }) $Name $Clsid on $Flow $Endpoint ($Slot) ==="
if (-not (Grant-AdminControl $endpointSub)) { throw "Could not take ownership of $endpointSub" }
if (-not (Test-Path $fxPath)) { New-Item -Path $fxPath | Out-Null; Say 'Created FxProperties' }
[void](Grant-AdminControl $fxSub)

if ($Uninstall) {
    if (-not (Test-Path $backupFile)) { throw "No backup at $backupFile; refusing to guess the original values." }
    $backup = Get-Content $backupFile -Raw | ConvertFrom-Json
    foreach ($name in $touched) {
        $entry = $backup.values.PSObject.Properties[$name]
        $orig = if ($entry) { $entry.Value } else { $null }
        Write-Value $name $orig
        Say "restored $name = $(if ($orig) { @($orig.value) -join ', ' } else { '<absent>' })"
    }
    Restart-Audio
    Remove-Item $backupFile
    if ($Unregister) {
        $installed = Join-Path $dataDir (Split-Path -Leaf $backup.dll)
        if (Test-Path $installed) { & regsvr32.exe /s /u $installed; Say "unregistered $installed" }
    }
    Say 'Uninstalled.'
    return
}

if (-not $Dll) { throw '-Dll is required to install.' }
$src = (Resolve-Path $Dll).Path
$dst = Join-Path $dataDir (Split-Path -Leaf $src)
try { Copy-Item $src $dst -Force } catch {
    # audiodg.exe keeps the old DLL mapped while any stream runs. A mapped
    # file can't be overwritten but can be renamed; the restart below then
    # loads the new copy, and the stale one is removed on a later install.
    $stale = "$dst.old-$(Get-Date -Format yyyyMMddHHmmss)"
    Move-Item $dst $stale -Force
    Copy-Item $src $dst -Force
    Say "previous DLL was in use; moved to $stale"
}
Get-ChildItem "$dst.old-*" -ErrorAction SilentlyContinue | ForEach-Object { try { Remove-Item $_.FullName -Force } catch {} }
& icacls.exe $dataDir /grant '*S-1-1-0:(OI)(CI)RX' '*S-1-5-19:(OI)(CI)RX' /T /Q | Out-Null
$p = Start-Process regsvr32.exe -ArgumentList '/s', "`"$dst`"" -Wait -PassThru
if ($p.ExitCode -ne 0) { throw "regsvr32 $dst failed ($($p.ExitCode))" }
Say "registered $dst"

if ($AllowUnsignedAudioDG) {
    New-ItemProperty -Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Audio' -Name DisableProtectedAudioDG -PropertyType DWord -Value 1 -Force | Out-Null
    Say 'DisableProtectedAudioDG = 1'
}

if (-not (Test-Path $backupFile)) {
    $values = [ordered]@{}
    foreach ($name in $touched) { $values[$name] = Read-Value $name }
    [pscustomobject]@{ endpoint = $Endpoint; flow = $Flow; slot = $Slot; clsid = $Clsid; dll = $dst; values = $values } |
        ConvertTo-Json -Depth 5 | Set-Content -Path $backupFile -Encoding utf8
    Say "backed up original values to $backupFile"
    foreach ($name in $touched) { $v = $values[$name]; if ($v) { Say "  was $name = $(@($v.value) -join ', ')" } }
} else {
    Say "keeping existing backup $backupFile (re-install)"
}

Apply-Ours
Restart-Audio
if (-not (Ours-Present)) {
    Say 'AudioEndpointBuilder reset the values; re-applying'
    Apply-Ours
    Restart-Audio
}
Say "installed: $compositeKey = $((Read-Value $compositeKey).value -join ', '); $modesKey = $((Read-Value $modesKey).value -join ', ')"
