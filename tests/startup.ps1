param(
    [string]$Executable = "$PSScriptRoot/../dist/SumiPDF.exe",
    [Parameter(Mandatory = $true)][string]$BaselineExecutable,
    [ValidateRange(1, 100)][int]$Repeats = 20,
    [ValidateRange(0, 20000)][int]$ExtraShapes = 0,
    [string]$CsvPath
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (!$IsWindows) { throw 'Startup comparison requires Windows.' }

Add-Type -Path (Join-Path $PSScriptRoot 'ReadingProbe.cs')
. (Join-Path $PSScriptRoot 'smokeSupport.ps1')

function Measure-Startup([Diagnostics.ProcessStartInfo]$Start, [bool]$WithPdf) {
    $window = [IntPtr]::Zero
    $windowMs = $statusMs = $pixelsMs = $null
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $viewer = [Diagnostics.Process]::Start($Start)
    try {
        do {
            if ($viewer.HasExited) { throw "The viewer exited with code $($viewer.ExitCode) during startup." }
            if ($window -eq [IntPtr]::Zero) {
                $window = [ReadingProbe]::VisibleViewerWindow($viewer.Id)
                if ($window -ne [IntPtr]::Zero) { $windowMs = $clock.Elapsed.TotalMilliseconds }
            }
            if ($window -ne [IntPtr]::Zero) {
                if (!$WithPdf) { break }
                if ($null -eq $pixelsMs -and [ReadingProbe]::FirstPagePixelsVisible($window)) {
                    $pixelsMs = $clock.Elapsed.TotalMilliseconds
                }
                if ($null -eq $statusMs -and [ReadingProbe]::HasPageReadyStatus($window, '1 / 2')) {
                    $statusMs = $clock.Elapsed.TotalMilliseconds
                }
                if ($null -ne $statusMs -and ($null -ne $pixelsMs -or $clock.Elapsed.TotalMilliseconds - $statusMs -ge 750)) {
                    break
                }
            }
            [Threading.Thread]::Sleep(5)
        } while ($clock.Elapsed.TotalSeconds -lt 30)
        if ($null -eq $windowMs) { throw 'Timed out waiting for a visible SumiPDF window.' }
        if ($WithPdf -and $null -eq $statusMs) { throw 'Timed out waiting for the first PDF page-ready status.' }
        [pscustomobject]@{ WindowMs = $windowMs; PageReadyMs = $statusMs; FirstPagePixelsMs = $pixelsMs }
    } finally {
        try {
            if (!$viewer.HasExited -and (!$viewer.CloseMainWindow() -or !$viewer.WaitForExit(5000))) {
                $viewer.Kill()
                $viewer.WaitForExit()
            }
        } finally { $viewer.Dispose() }
    }
}

function Format-StartupRow([string]$Build, [object[]]$Trials, [string]$Metric, [string]$Milestone) {
    $first = if ($null -eq $Trials[0].$Metric) { 'n/a' } else { '{0:F1}' -f $Trials[0].$Metric }
    $times = @($Trials | Where-Object { $_.Trial -gt 0 -and $null -ne $_.$Metric } |
        ForEach-Object { $_.$Metric } | Sort-Object)
    $expected = $Trials.Count - 1
    $median = $p95 = 'n/a'
    if ($times.Count -eq $expected) {
        $middle = [int][Math]::Floor($times.Count / 2)
        $medianValue = if ($times.Count % 2 -eq 0) {
            ($times[$middle - 1] + $times[$middle]) / 2
        } else { $times[$middle] }
        $median = '{0:F1}' -f $medianValue
        $p95 = '{0:F1}' -f $times[([int][Math]::Ceiling(0.95 * $times.Count) - 1)]
    }
    "| $Build | $($Trials[0].Scenario) | $Milestone | $first | $($times.Count)/$expected | $median | $p95 |"
}

$builds = @(
    @{ Name = 'Baseline'; Executable = (Resolve-Path -LiteralPath $BaselineExecutable).Path }
    @{ Name = 'Candidate'; Executable = (Resolve-Path -LiteralPath $Executable).Path }
)
$scenarios = @('Empty', 'PDF')
$dir = Join-Path ([IO.Path]::GetTempPath()) ('SumiPDF-startup-' + [Guid]::NewGuid().ToString('N'))
try {
    [void][IO.Directory]::CreateDirectory($dir)
    $fixture = Join-Path $dir 'Reading notes.pdf'
    Write-ReadingPdf $fixture 2 $ExtraShapes
    $trials = @(for ($trial = 0; $trial -le $Repeats; $trial++) {
        $order = @(0, 1)
        if ($trial % 2 -ne 0) { $order = @(1, 0) }
        foreach ($scenario in $scenarios) {
            foreach ($index in $order) {
                $build = $builds[$index]
                $withPdf = $scenario -eq 'PDF'
                $documents = if ($withPdf) { @($fixture) }
                $profile = Join-Path $dir "$($build.Name)-$scenario-$trial"
                $start = New-SmokeViewerStartInfo $build.Executable $profile $documents
                try { $startup = Measure-Startup $start $withPdf }
                catch { throw "$($build.Name) $scenario trial $trial failed. $($_.Exception.Message)" }
                [pscustomobject]@{
                    Build = $build.Name
                    Scenario = $scenario
                    Trial = $trial
                    WindowMs = $startup.WindowMs
                    PageReadyMs = $startup.PageReadyMs
                    FirstPagePixelsMs = $startup.FirstPagePixelsMs
                }
            }
        }
    })
} finally {
    if ([IO.Directory]::Exists($dir)) { [IO.Directory]::Delete($dir, $true) }
}

if ($CsvPath) { $trials | Export-Csv -LiteralPath $CsvPath }
$summary = @(
    '### SumiPDF startup comparison'
    ''
    "Each build and scenario has one first sample and $Repeats repeated samples. The build order reverses each round. All launches share a Windows host and its existing system caches, with a fresh viewer profile for each launch."
    'Times start immediately before process launch and include polling (5 ms requested interval) and scheduling. The PDF is the shared synthetic two-page reading fixture.'
    "The first page contains $ExtraShapes additional vector marks."
    'The page-ready milestone uses the page counter and fit-size status. The first-page pixel milestone samples the displayed canvas and is unavailable if the blue page is absent from the captured surface within 750 ms after observing page-ready status.'
    'Each median and nearest-rank p95 requires all repeated samples for that milestone.'
    ''
    '| Build | Launch | Observed milestone | First sample (ms) | Repeated samples | Median (ms) | p95 (ms) |'
    '| --- | --- | --- | ---: | ---: | ---: | ---: |'
    foreach ($build in $builds) {
        $version = [Diagnostics.FileVersionInfo]::GetVersionInfo($build.Executable).FileVersion
        $label = if ($version) { "$($build.Name) v$version" } else { $build.Name }
        foreach ($scenario in $scenarios) {
            $cases = @($trials | Where-Object { $_.Build -eq $build.Name -and $_.Scenario -eq $scenario })
            Format-StartupRow $label $cases 'WindowMs' 'Visible window handle'
            if ($scenario -eq 'PDF') {
                Format-StartupRow $label $cases 'PageReadyMs' 'Page-ready status'
                Format-StartupRow $label $cases 'FirstPagePixelsMs' 'First visible page pixels'
            }
        }
    }
)
$summary | Write-Host
if ($env:GITHUB_STEP_SUMMARY) { $summary | Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY -Encoding utf8 }
