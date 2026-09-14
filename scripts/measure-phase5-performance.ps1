param(
    [string]$OutputDirectory = "target/phase5-performance",
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
Set-Location -LiteralPath $root

if (-not $SkipBuild) {
    cargo build --release --locked -p lvos-agent -p lvos
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
}

$agentExe = (Resolve-Path -LiteralPath 'target/release/lvos-agent.exe').Path
$uiExe = (Resolve-Path -LiteralPath 'target/release/lvos-ui.exe').Path
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$outputRoot = (Resolve-Path -LiteralPath $OutputDirectory).Path
$diagnosticPath = Join-Path $outputRoot 'phase5-diagnostic.json'
$stdoutPath = Join-Path $outputRoot 'phase5.stdout.log'
$stderrPath = Join-Path $outputRoot 'phase5.stderr.log'
foreach ($path in @($diagnosticPath, $stdoutPath, $stderrPath)) {
    if (Test-Path -LiteralPath $path -PathType Leaf) {
        Remove-Item -LiteralPath $path -Force
    }
}

$existingUi = @(Get-Process -Name 'lvos-ui' -ErrorAction SilentlyContinue | Where-Object {
    try { $_.Path -eq $uiExe } catch { $false }
})
if ($existingUi.Count -ne 0) {
    throw 'The release lvos-ui.exe is already running; close the diagnostic copy before measuring.'
}

$oldOutput = $env:LVOS_PHASE5_OUTPUT
$oldScale = $env:SLINT_SCALE_FACTOR
$env:LVOS_PHASE5_OUTPUT = $diagnosticPath
$env:SLINT_SCALE_FACTOR = '1.25'
$samples = [System.Collections.Generic.List[object]]::new()
$watch = [System.Diagnostics.Stopwatch]::StartNew()
try {
    $process = Start-Process -FilePath $agentExe -ArgumentList '--phase5-performance-check' `
        -PassThru -WindowStyle Hidden -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath
    while (-not $process.HasExited) {
        if ($watch.Elapsed.TotalSeconds -gt 180) {
            $process.Kill()
            throw 'Phase 5 performance diagnostic timed out.'
        }
        $process.Refresh()
        try {
            $samples.Add([pscustomobject][ordered]@{
                elapsed_ms = $watch.Elapsed.TotalMilliseconds
                role = 'agent'
                process_id = $process.Id
                private_bytes = $process.PrivateMemorySize64
                working_set_bytes = $process.WorkingSet64
            })
        } catch {
            if (-not $process.HasExited) { throw }
        }
        foreach ($uiProcess in @(Get-Process -Name 'lvos-ui' -ErrorAction SilentlyContinue)) {
            try {
                if ($uiProcess.Path -eq $uiExe) {
                    $samples.Add([pscustomobject][ordered]@{
                        elapsed_ms = $watch.Elapsed.TotalMilliseconds
                        role = 'ui'
                        process_id = $uiProcess.Id
                        private_bytes = $uiProcess.PrivateMemorySize64
                        working_set_bytes = $uiProcess.WorkingSet64
                    })
                }
            } catch {
                if (-not $uiProcess.HasExited) { throw }
            }
        }
        Start-Sleep -Milliseconds 100
    }
    $process.WaitForExit()
    $exitCode = $process.ExitCode
    if ($null -ne $exitCode -and $exitCode -ne 0) {
        throw "Phase 5 diagnostic exited with ${exitCode}: $(Get-Content -LiteralPath $stderrPath -Raw)"
    }
} finally {
    $env:LVOS_PHASE5_OUTPUT = $oldOutput
    $env:SLINT_SCALE_FACTOR = $oldScale
}

if (-not (Test-Path -LiteralPath $diagnosticPath -PathType Leaf)) {
    throw 'The Agent did not produce the Phase 5 diagnostic report.'
}
$diagnostic = Get-Content -LiteralPath $diagnosticPath -Raw | ConvertFrom-Json
$agentIdle = @($samples | Where-Object { $_.role -eq 'agent' -and $_.elapsed_ms -ge 500 -and $_.elapsed_ms -le 2700 })
$prematureUi = @($samples | Where-Object { $_.role -eq 'ui' -and $_.elapsed_ms -le 2700 })
if ($agentIdle.Count -lt 10 -or $prematureUi.Count -ne 0) {
    throw 'Agent-only sampling was incomplete or observed an unexpected GUI process.'
}
$agentPrivate = @($agentIdle | ForEach-Object { [double]$_.private_bytes })
$agentPrivateMaximum = ($agentPrivate | Measure-Object -Maximum).Maximum

$warmSamples = @($samples | Where-Object {
    $_.role -eq 'ui' -and $_.process_id -eq $diagnostic.warm_process_id
})
if ($warmSamples.Count -lt 30) {
    throw 'Insufficient warm UI memory samples for the 500-cycle stability check.'
}
$steadySkip = [Math]::Floor($warmSamples.Count * 0.2)
$steady = @($warmSamples | Select-Object -Skip $steadySkip)
$quarter = [Math]::Max(5, [Math]::Floor($steady.Count / 4))

function Get-Median([double[]]$Values) {
    $ordered = @($Values | Sort-Object)
    $middle = [Math]::Floor($ordered.Count / 2)
    if ($ordered.Count % 2 -eq 0) {
        return ($ordered[$middle - 1] + $ordered[$middle]) / 2
    }
    return $ordered[$middle]
}

$earlyMedian = Get-Median @($steady | Select-Object -First $quarter | ForEach-Object { [double]$_.private_bytes })
$lateMedian = Get-Median @($steady | Select-Object -Last $quarter | ForEach-Object { [double]$_.private_bytes })
$meanX = ($steady.Count - 1) / 2.0
$meanY = (@($steady | ForEach-Object { [double]$_.private_bytes }) | Measure-Object -Average).Average
$numerator = 0.0
$denominator = 0.0
$totalVariance = 0.0
for ($index = 0; $index -lt $steady.Count; $index++) {
    $xOffset = $index - $meanX
    $yOffset = [double]$steady[$index].private_bytes - $meanY
    $numerator += $xOffset * $yOffset
    $denominator += $xOffset * $xOffset
    $totalVariance += $yOffset * $yOffset
}
$slopePerSample = if ($denominator -eq 0) { 0.0 } else { $numerator / $denominator }
$explained = 0.0
if ($totalVariance -ne 0) {
    for ($index = 0; $index -lt $steady.Count; $index++) {
        $prediction = $meanY + $slopePerSample * ($index - $meanX)
        $explained += ($prediction - $meanY) * ($prediction - $meanY)
    }
}
$rSquared = if ($totalVariance -eq 0) { 0.0 } else { [Math]::Min(1.0, $explained / $totalVariance) }
$growthBytes = $lateMedian - $earlyMedian
$linearGrowthDetected = $growthBytes -gt 8MB -and $rSquared -ge 0.8

$computer = Get-CimInstance Win32_ComputerSystem
$os = Get-CimInstance Win32_OperatingSystem
$result = [ordered]@{
    schema_version = 1
    measured_at_utc = [DateTime]::UtcNow.ToString('o')
    version = (python scripts/workspace_version.py).Trim()
    commit = (git rev-parse HEAD).Trim()
    host = [ordered]@{
        os = $os.Caption
        os_version = $os.Version
        processor_count = $computer.NumberOfLogicalProcessors
        memory_bytes = [uint64]$computer.TotalPhysicalMemory
        scale_factor = 1.25
        build_profile = 'release'
    }
    agent_only = [ordered]@{
        sample_count = $agentIdle.Count
        private_bytes_maximum = [uint64]$agentPrivateMaximum
        target_bytes = 20MB
        target_met = $agentPrivateMaximum -le 20MB
        ui_process_absent = $prematureUi.Count -eq 0
    }
    lifecycle = [ordered]@{
        lookup_cycles = $diagnostic.lookup_cycles
        ui_sample_count = $warmSamples.Count
        private_bytes_peak = [uint64](($warmSamples.private_bytes | Measure-Object -Maximum).Maximum)
        early_private_bytes_median = [uint64]$earlyMedian
        late_private_bytes_median = [uint64]$lateMedian
        median_growth_bytes = [int64]$growthBytes
        regression_r_squared = $rSquared
        estimated_slope_bytes_per_cycle = $slopePerSample * $steady.Count / $diagnostic.lookup_cycles
        sustained_linear_growth_detected = $linearGrowthDetected
        theme_variants = @('light', 'dark-reduced-motion')
        main_window_intervals = 5
    }
    latency = [ordered]@{
        cold_start_p95_ms = $diagnostic.cold_start_p95_ms
        cold_start_target_ms = $diagnostic.cold_start_target_ms
        cold_start_target_met = $diagnostic.cold_start_target_met
        warm_lookup_p95_ms = $diagnostic.warm_lookup_p95_ms
        warm_lookup_target_ms = $diagnostic.warm_lookup_target_ms
        warm_lookup_target_met = $diagnostic.warm_lookup_target_met
        boundary = $diagnostic.measurement_boundary
    }
    idle_exit = [ordered]@{
        configured_timeout_ms = $diagnostic.popup_idle_timeout_ms
        request_observed_ms = $diagnostic.idle_exit_request_ms
        exited_within_timeout_plus_five = $diagnostic.idle_exit_within_timeout_plus_five
    }
}
$resultPath = Join-Path $outputRoot 'phase5-baseline.json'
$result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $resultPath -Encoding utf8
$samples | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $outputRoot 'phase5-samples.json') -Encoding utf8
Write-Output "Phase 5 baseline: $resultPath"
Write-Output ($result | ConvertTo-Json -Depth 8)
if ($linearGrowthDetected) {
    throw 'The 500-cycle sample shows sustained linear UI private-memory growth.'
}
