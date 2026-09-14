param(
    [Parameter(Mandatory = $true)][string]$Baseline,
    [Parameter(Mandatory = $true)][string]$Candidate,
    [Parameter(Mandatory = $true)][string]$OutputDirectory,
    [int]$Runs = 3
)
$ErrorActionPreference = 'Stop'
if ($Runs -lt 3) { throw 'At least three runs are needed to observe measurement spread.' }
$baselineExe = (Resolve-Path -LiteralPath $Baseline).Path
$candidateExe = (Resolve-Path -LiteralPath $Candidate).Path
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$outputRoot = (Resolve-Path -LiteralPath $OutputDirectory).Path
$results = [System.Collections.Generic.List[object]]::new()
foreach ($item in @(@('baseline', $baselineExe), @('candidate', $candidateExe))) {
    if ($item[0] -eq 'candidate') {
        $bounds = [ordered]@{}
        foreach ($metric in @('first_frame_ms', 'peak_private_bytes', 'final_private_bytes', 'idle_cpu_one_core_percent', 'workload_ms')) {
            $range = $results | Measure-Object -Property $metric -Minimum -Maximum
            $spread = $range.Maximum - $range.Minimum
            $bounds[$metric] = [ordered]@{ minimum = $range.Minimum; maximum = $range.Maximum; spread = $spread; exploratory_upper_bound = $range.Maximum + 2 * $spread }
        }
        $bounds | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $outputRoot 'baseline-envelope.json') -Encoding utf8
        Write-Output 'Baseline spread recorded before candidate runs. Bounds are exploratory, not a release budget.'
    }
    for ($run = 1; $run -le $Runs; $run++) {
        $label = $item[0]
        $stdout = Join-Path $outputRoot "$label-$run.log"
        $stderr = Join-Path $outputRoot "$label-$run.stderr.log"
        $samples = [System.Collections.Generic.List[object]]::new()
        $watch = [System.Diagnostics.Stopwatch]::StartNew()
        $process = Start-Process -FilePath $item[1] -PassThru -WindowStyle Hidden -RedirectStandardOutput $stdout -RedirectStandardError $stderr
        while (-not $process.HasExited) {
            if ($watch.Elapsed.TotalSeconds -gt 120) {
                $process.Kill()
                throw "Synthetic UI workload timed out; no performance result is valid."
            }
            $process.Refresh()
            try {
                $samples.Add([pscustomobject][ordered]@{
                    seconds = $watch.Elapsed.TotalSeconds
                    cpu_seconds = $process.TotalProcessorTime.TotalSeconds
                    private_bytes = $process.PrivateMemorySize64
                    working_set_bytes = $process.WorkingSet64
                    handles = $process.HandleCount
                })
            } catch {
                if (-not $process.HasExited) { throw }
            }
            Start-Sleep -Milliseconds 200
        }
        $process.WaitForExit()
        if ($process.ExitCode -ne 0) { throw "$label run $run failed: $(Get-Content -LiteralPath $stderr -Raw)" }
        $events = @(Get-Content -LiteralPath $stdout | ForEach-Object { $_ | ConvertFrom-Json })
        $idle = @($events | Where-Object { $_.phase -eq 'idle' })
        $complete = @($events | Where-Object { $_.phase -eq 'complete' })
        if ($idle.Count -ne 1 -or $complete.Count -ne 1 -or $idle[0].cycles -ne 120) {
            throw 'Incomplete synthetic workload; do not report performance from this run.'
        }
        $stableIdle = @($samples | Where-Object {
            $_.seconds -ge ($idle[0].elapsed_ms / 1000 + 1) -and
            $_.seconds -le ($complete[0].elapsed_ms / 1000 - 0.5)
        })
        if ($stableIdle.Count -lt 20) { throw 'Insufficient idle samples.' }
        $first = $stableIdle[0]
        $last = $stableIdle[-1]
        $results.Add([pscustomobject][ordered]@{
            label = $label; run = $run
            executable_sha256 = (Get-FileHash -LiteralPath $item[1] -Algorithm SHA256).Hash.ToLower()
            first_frame_ms = @($events | Where-Object { $null -ne $_.first_frame_ms })[0].first_frame_ms
            idle_cpu_one_core_percent = 100 * ($last.cpu_seconds - $first.cpu_seconds) / ($last.seconds - $first.seconds)
            workload_ms = $idle[0].elapsed_ms - @($events | Where-Object { $_.phase -eq 'workload' })[0].elapsed_ms
            final_private_bytes = $last.private_bytes
            idle_private_growth_bytes = $last.private_bytes - $first.private_bytes
            peak_private_bytes = ($samples | Measure-Object -Property private_bytes -Maximum).Maximum
            final_handles = $last.handles
            events = $events
        })
        $samples | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $outputRoot "$label-$run.samples.json") -Encoding utf8
        Write-Output "$label run $run complete"
    }
}
$results | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $outputRoot 'results.json') -Encoding utf8
Write-Output 'Performance samples saved; review spread before choosing regression budgets.'
