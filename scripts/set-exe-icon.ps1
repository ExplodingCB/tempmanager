# Embeds an .ico into a built exe as its application icon (the one Explorer,
# the taskbar, and Task Manager show).
#
# The GNU toolchain ships no resource compiler, so rather than adding windres
# or rc.exe to the build this patches the finished PE with the Win32
# resource-update API: one RT_ICON per image plus an RT_GROUP_ICON directory.
#
#   .\scripts\set-exe-icon.ps1 target\x86_64-pc-windows-gnu\release\tempmanager.exe assets\tempmanager.ico
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [Parameter(Mandatory = $true)][string]$Icon
)
$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class ResUpdate {
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    public static extern IntPtr BeginUpdateResource(string file, bool deleteExisting);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool UpdateResource(IntPtr h, IntPtr type, IntPtr name, ushort lang, byte[] data, uint size);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool EndUpdateResource(IntPtr h, bool discard);
}
'@

$exePath = (Resolve-Path -LiteralPath $Exe).Path
$ico = [IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $Icon).Path)
if ([BitConverter]::ToUInt16($ico, 0) -ne 0 -or [BitConverter]::ToUInt16($ico, 2) -ne 1) {
    throw "$Icon is not an .ico file"
}
$count = [BitConverter]::ToUInt16($ico, 4)

$RT_ICON = [IntPtr]3
$RT_GROUP_ICON = [IntPtr]14
$LANG_NEUTRAL = 0

$h = [ResUpdate]::BeginUpdateResource($exePath, $false)
if ($h -eq [IntPtr]::Zero) { throw "BeginUpdateResource failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())" }

try {
    # GRPICONDIR: the ICONDIR header followed by 14-byte entries whose last
    # field is the RT_ICON resource id instead of a file offset.
    $group = New-Object IO.MemoryStream
    $w = New-Object IO.BinaryWriter $group
    $w.Write([UInt16]0); $w.Write([UInt16]1); $w.Write([UInt16]$count)

    for ($i = 0; $i -lt $count; $i++) {
        $e = 6 + 16 * $i
        $size = [BitConverter]::ToUInt32($ico, $e + 8)
        $offset = [BitConverter]::ToUInt32($ico, $e + 12)
        $image = New-Object byte[] $size
        [Array]::Copy($ico, $offset, $image, 0, $size)

        $id = $i + 1
        if (-not [ResUpdate]::UpdateResource($h, $RT_ICON, [IntPtr]$id, $LANG_NEUTRAL, $image, $size)) {
            throw "UpdateResource(RT_ICON $id) failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
        }
        $w.Write($ico, $e, 12)          # width..bytesInRes, unchanged
        $w.Write([UInt16]$id)
    }

    $bytes = $group.ToArray()
    if (-not [ResUpdate]::UpdateResource($h, $RT_GROUP_ICON, [IntPtr]1, $LANG_NEUTRAL, $bytes, $bytes.Length)) {
        throw "UpdateResource(RT_GROUP_ICON) failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
} catch {
    [ResUpdate]::EndUpdateResource($h, $true) | Out-Null
    throw
}

if (-not [ResUpdate]::EndUpdateResource($h, $false)) {
    throw "EndUpdateResource failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
}
Write-Host "Embedded $count icon images into $exePath"
