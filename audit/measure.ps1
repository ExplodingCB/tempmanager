param(
    [Parameter(Mandatory=$true)][string]$Executable,
    [Parameter(Mandatory=$true)][string]$OutputPath,
    [int]$IntervalSeconds = 30,
    [int]$DurationSeconds = 65
)
$ErrorActionPreference = 'Stop'
$taskExe = (Resolve-Path -LiteralPath $Executable).Path
$taskOutput = [System.IO.Path]::GetFullPath($OutputPath)
$taskConfigRoot = Join-Path (Split-Path $taskOutput) ([System.IO.Path]::GetFileNameWithoutExtension($taskOutput) + '-config')
New-Item -ItemType Directory -Path (Join-Path $taskConfigRoot 'tempmanager') -Force | Out-Null
Set-Content -LiteralPath (Join-Path $taskConfigRoot 'tempmanager/config.ini') -Value "interval_s=$IntervalSeconds`ntray_source=hottest`nfahrenheit=0`nautostart=0"
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class TempmanagerAudit {
    [DllImport("user32.dll")]
    public static extern uint GetGuiResources(IntPtr process, uint flag);
}
'@
$taskPreviousAppData = $env:APPDATA
try {
    $env:APPDATA = $taskConfigRoot
    $taskProcess = Start-Process -FilePath $taskExe -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 5
    $taskProcess.Refresh()
    if ($taskProcess.HasExited) { throw 'Measurement process exited during startup.' }
    $taskStartCpu = $taskProcess.TotalProcessorTime.TotalSeconds
    $taskWatch = [System.Diagnostics.Stopwatch]::StartNew()
    $taskReadings = @()
    for ($i = 0; $i -lt $DurationSeconds; $i++) {
        Start-Sleep -Seconds 1
        $taskProcess.Refresh()
        if ($taskProcess.HasExited) { throw 'Measurement process exited unexpectedly.' }
        $taskReadings += [pscustomobject]@{
            Seconds = [Math]::Round($taskWatch.Elapsed.TotalSeconds, 3)
            CpuSeconds = $taskProcess.TotalProcessorTime.TotalSeconds - $taskStartCpu
            WorkingSetBytes = $taskProcess.WorkingSet64
            PrivateBytes = $taskProcess.PrivateMemorySize64
            Handles = $taskProcess.HandleCount
            Threads = $taskProcess.Threads.Count
            GdiObjects = [TempmanagerAudit]::GetGuiResources($taskProcess.Handle, 0)
        }
    }
    $taskLast = $taskReadings[-1]
    [pscustomobject]@{
        Executable = $taskExe
        BinaryBytes = (Get-Item -LiteralPath $taskExe).Length
        IntervalSeconds = $IntervalSeconds
        DurationSeconds = $taskLast.Seconds
        CpuSeconds = $taskLast.CpuSeconds
        CpuPercentOfOneCore = 100 * $taskLast.CpuSeconds / $taskLast.Seconds
        WorkingSetBytes = $taskLast.WorkingSetBytes
        PrivateBytes = $taskLast.PrivateBytes
        Threads = $taskLast.Threads
        Handles = $taskLast.Handles
        GdiObjects = $taskLast.GdiObjects
        Readings = $taskReadings
    } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $taskOutput
    Get-Content -LiteralPath $taskOutput -TotalCount 13
} finally {
    $env:APPDATA = $taskPreviousAppData
    if ($null -ne $taskProcess -and -not $taskProcess.HasExited) {
        Stop-Process -Id $taskProcess.Id
    }
}
