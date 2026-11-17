# Run on Windows (PowerShell 5.1+) so counters describe the PC, not the WSL VM.
[CmdletBinding()]
param(
    [string]$Endpoint = 'http://localhost:8080',
    [ValidateRange(1, 3600)][int]$IntervalSeconds = 1,
    [string]$ComputerLabel = $env:COMPUTERNAME,
    [switch]$Once,
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function New-Metric {
    param([string]$Name, [double]$Value, [hashtable]$ExtraLabels = @{})
    if ([double]::IsNaN($Value) -or [double]::IsInfinity($Value)) { return }
    $labels = @{ __name__ = $Name; host = $ComputerLabel; source = 'windows' }
    foreach ($key in $ExtraLabels.Keys) { $labels[$key] = [string]$ExtraLabels[$key] }
    @{ labels = $labels; samples = @(@{ t = $script:Timestamp; v = $Value }) }
}

function Get-Telemetry {
    # Each category fails independently; unavailable counters are omitted, not zeroed.
    try {
        $cpu = Get-CimInstance -ClassName Win32_PerfFormattedData_PerfOS_Processor -Filter "Name='_Total'"
        if ($null -ne $cpu) { New-Metric 'pc_cpu_usage_percent' $cpu.PercentProcessorTime }
    } catch { Write-Warning "CPU counters unavailable: $_" }
    try {
        $os = Get-CimInstance -ClassName Win32_OperatingSystem
        $total = [double]$os.TotalVisibleMemorySize * 1024
        $used = $total - [double]$os.FreePhysicalMemory * 1024
        if ($total -gt 0) {
            New-Metric 'pc_memory_total_bytes' $total
            New-Metric 'pc_memory_used_bytes' $used
            New-Metric 'pc_memory_usage_percent' (100 * $used / $total)
        }
    } catch { Write-Warning "Memory counters unavailable: $_" }
    try {
        foreach ($disk in Get-CimInstance -ClassName Win32_LogicalDisk -Filter 'DriveType=3') {
            if ($null -ne $disk.Size -and $disk.Size -gt 0 -and $null -ne $disk.FreeSpace) {
                $labels = @{ volume = $disk.DeviceID }
                New-Metric 'pc_disk_total_bytes' $disk.Size $labels
                New-Metric 'pc_disk_used_bytes' ($disk.Size - $disk.FreeSpace) $labels
                New-Metric 'pc_disk_usage_percent' (100 * ($disk.Size - $disk.FreeSpace) / $disk.Size) $labels
            }
        }
    } catch { Write-Warning "Disk space counters unavailable: $_" }
    try {
        foreach ($disk in Get-CimInstance -ClassName Win32_PerfFormattedData_PerfDisk_LogicalDisk) {
            if ($disk.Name -eq '_Total') { continue }
            $labels = @{ volume = $disk.Name }
            New-Metric 'pc_disk_read_bytes_per_second' $disk.DiskReadBytesPersec $labels
            New-Metric 'pc_disk_write_bytes_per_second' $disk.DiskWriteBytesPersec $labels
        }
    } catch { Write-Warning "Disk I/O counters unavailable: $_" }
    try {
        foreach ($adapter in Get-CimInstance -ClassName Win32_PerfFormattedData_Tcpip_NetworkInterface) {
            $labels = @{ interface = $adapter.Name }
            New-Metric 'pc_network_receive_bytes_per_second' $adapter.BytesReceivedPersec $labels
            New-Metric 'pc_network_send_bytes_per_second' $adapter.BytesSentPersec $labels
        }
    } catch { Write-Warning "Network counters unavailable: $_" }
}

if (-not $DryRun) { Write-Host "Sending Windows telemetry to $Endpoint every $IntervalSeconds seconds. Ctrl+C to stop." }
do {
    $cycle = [System.Diagnostics.Stopwatch]::StartNew()
    $script:Timestamp = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    $metrics = @(Get-Telemetry)
    if ($metrics.Count -gt 0) {
        $json = ConvertTo-Json -InputObject $metrics -Depth 6 -Compress
        if ($DryRun) { Write-Output $json }
        else {
            try {
                Invoke-RestMethod -Method Post -Uri ($Endpoint.TrimEnd('/') + '/debug/write_json') -ContentType 'application/json; charset=utf-8' -Body ([System.Text.Encoding]::UTF8.GetBytes($json)) -TimeoutSec 10 | Out-Null
                Write-Host ("{0:HH:mm:ss} sent {1} series" -f (Get-Date), $metrics.Count)
            } catch { Write-Warning "Write failed; next interval will retry with fresh data: $_" }
        }
    } else { Write-Warning 'No telemetry available. Check the counter warnings above.' }
    # Include collection and HTTP time in the interval instead of adding a full sleep.
    if (-not $Once) {
        $remainingMs = [int][Math]::Max(0, $IntervalSeconds * 1000 - $cycle.ElapsedMilliseconds)
        if ($remainingMs -gt 0) { Start-Sleep -Milliseconds $remainingMs }
    }
} while (-not $Once)
