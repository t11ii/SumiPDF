function Wait-Find([Diagnostics.Process]$Viewer, [IntPtr]$Window, [string]$Expected, [int]$StableMs = 0) {
    $status = [SumiSmoke]::GetDlgItem($Window, 165)
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $since = $null
    do {
        if ($Viewer.HasExited) { throw 'Viewer exited during Find.' }
        $text = [SumiSmoke]::ReadText($status)
        if ([SumiSmoke]::IsWindowVisible($status) -and $text -like $Expected) {
            if ($null -eq $since) { $since = $clock.ElapsedMilliseconds }
            if ($clock.ElapsedMilliseconds - $since -ge $StableMs) { return }
        } elseif ($null -ne $since) {
            throw "Find changed from '$Expected' to '$text' while the input was idle."
        }
        Start-Sleep -Milliseconds 35
    } while ($clock.ElapsedMilliseconds -lt 15000)
    throw "Find reported '$text', expected '$Expected'."
}

function Wait-FindPixels([IntPtr]$Window, [bool]$Highlighted) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    do {
        $pixels = [ReadingProbe]::SearchHighlightPixels($Window)
        if (($Highlighted -and $pixels -ge 3) -or (!$Highlighted -and $pixels -eq 0)) { return }
        Start-Sleep -Milliseconds 50
    } while ($clock.ElapsedMilliseconds -lt 6000)
    throw "Expected active highlight $Highlighted on the painted page, sampled $pixels orange pixels."
}

# Measure the normal edit notification through to the visible counter and the
# highlighted span in the viewer's own paint path. Only the first page of the
# fixture is displayed. Sampling and Windows scheduling are part of these times.
function Measure-FindInput([Diagnostics.Process]$Viewer, [IntPtr]$Window, [string]$Query, [bool]$CapturePixels) {
    $status = [SumiSmoke]::GetDlgItem($Window, 165)
    $first = $complete = $ink = $null
    $clock = [Diagnostics.Stopwatch]::StartNew()
    [ReadingProbe]::SetFindQuery($Window, $Query)
    do {
        if ($Viewer.HasExited) { throw 'Viewer exited during Find measurement.' }
        $text = [SumiSmoke]::ReadText($status)
        $now = $clock.Elapsed.TotalMilliseconds
        if ($null -eq $first -and $text -match '^1 of [1-9]' -and [SumiSmoke]::IsWindowVisible($status)) { $first = $now }
        if ($null -ne $first -and $null -eq $complete -and $text -eq '1 of 3') { $complete = $now }
        if ($CapturePixels -and $null -ne $first -and $null -eq $ink -and [ReadingProbe]::SearchHighlightPixels($Window) -ge 3) {
            $ink = $clock.Elapsed.TotalMilliseconds
        }
        if ($null -ne $complete -and (!$CapturePixels -or $null -ne $ink)) {
            return [pscustomobject]@{ First = $first; Complete = $complete; Painted = $ink }
        }
        [Threading.Thread]::Sleep(2)
    } while ($clock.ElapsedMilliseconds -lt 15000)
    throw "Find did not display all three matches and the expected highlight. Counter '$text', first $first ms, painted $ink ms."
}

function Test-FindInputLatency([Diagnostics.Process]$Viewer, [IntPtr]$Window, [string]$Query, [bool]$CapturePixels, [string]$Build) {
    if ($CapturePixels) { Wait-FindPixels $Window $false }
    $cold = Measure-FindInput $Viewer $Window $Query $CapturePixels
    $warm = foreach ($trial in 1..5) {
        [ReadingProbe]::SetFindQuery($Window, 'word absent from this fixture')
        Wait-Find $Viewer $Window 'No matches'
        if ($CapturePixels) { Wait-FindPixels $Window $false }
        Measure-FindInput $Viewer $Window $Query $CapturePixels
    }
    $painted = if ($CapturePixels) {
        '{0:N1}/{1:N1}' -f $cold.Painted, ($warm.Painted | Sort-Object)[2]
    } else { 'unavailable' }
    Write-Host (("Find latency ($Build), 240 pages, 3 matches, ms (cold / median of 5 repeated queries): " +
        'first visible count {0:N1}/{1:N1}, completed count {2:N1}/{3:N1}, ' +
        'first painted highlight {4}. Times begin before setting the edit text and include polling.') -f
        $cold.First, ($warm.First | Sort-Object)[2], $cold.Complete, ($warm.Complete | Sort-Object)[2], $painted)
}

function Test-FindBaseline([Diagnostics.Process]$Candidate, [string]$Query) {
    $exe = (Resolve-Path -LiteralPath $env:SUMIPDF_FIND_BASELINE_EXE).Path
    # Use exactly the PDFs passed to the candidate by Start-SmokeViewer, opening
    # the reading fixture last to activate it with the tab strip visible.
    $files = $Candidate.StartInfo.ArgumentList
    $reading = $files[0]
    $profile = Join-Path ([IO.Path]::GetDirectoryName($reading)) 'baseline profile'
    $baseline = Start-SmokeViewer $exe $profile @($files[-1], $reading)
    try {
        $window = Wait-Page $baseline ([IO.Path]::GetFileName($reading)) '1 / 240' ([Diagnostics.Stopwatch]::StartNew()) -Quiet
        [SumiSmoke]::CheckResize($window, 900, $false)
        $capture = [bool](Wait-Viewport $window 'B{15,}' 2500 -IfAvailable)
        [ReadingProbe]::OpenFind($window)
        Test-FindInputLatency $baseline $window $Query $capture 'baseline'
        [ReadingProbe]::CloseFind($window)
    } finally {
        if (!$baseline.HasExited -and (!$baseline.CloseMainWindow() -or !$baseline.WaitForExit(5000))) {
            $baseline.Kill()
            $baseline.WaitForExit()
        }
        $baseline.Dispose()
    }
}

function Test-Find([Diagnostics.Process]$Viewer, [IntPtr]$Window) {
    $query = 'Which section'
    $literal = $query + '?'
    if ($env:SUMIPDF_FIND_BASELINE_EXE) { Test-FindBaseline $Viewer $query }
    $capture = [bool](Wait-Viewport $Window 'B{15,}' 2500 -IfAvailable)
    if ($capture) { Wait-FindPixels $Window $false }
    [ReadingProbe]::OpenFind($Window)
    Test-FindInputLatency $Viewer $Window $query $capture 'candidate'
    [ReadingProbe]::AppendFindQuestionMark($Window)
    $edit = [SumiSmoke]::GetDlgItem($Window, 161)
    if ([SumiSmoke]::ReadText($edit) -ne $literal) { throw 'Find did not insert the literal question mark.' }
    Wait-Find $Viewer $Window '1 of 2'
    if ($capture) { Wait-FindPixels $Window $true }
    [SumiSmoke]::Click($Window, 163)
    Wait-Find $Viewer $Window '2 of 2'
    [void](Wait-Page $Viewer 'Reading notes.pdf' '37 / 240' ([Diagnostics.Stopwatch]::StartNew()) -Quiet)
    if ($capture) { Wait-FindPixels $Window $true }
    [SumiSmoke]::Click($Window, 162)
    Wait-Find $Viewer $Window '1 of 2'
    [void](Wait-Page $Viewer 'Reading notes.pdf' '1 / 240' ([Diagnostics.Stopwatch]::StartNew()) -Quiet)
    [ReadingProbe]::SetFindQuery($Window, 'Reading notes')
    Wait-Find $Viewer $Window '1 of [0-9][0-9]*'
    foreach ($value in @($literal, 'Reading notes', 'Which section!', '')) {
        [ReadingProbe]::SetFindQuery($Window, $value)
    }
    Wait-Find $Viewer $Window '' -StableMs 250
    if ($capture) { Wait-FindPixels $Window $false }
    foreach ($value in @('Which section!', 'no such term', $literal)) {
        [ReadingProbe]::SetFindQuery($Window, $value)
    }
    Wait-Find $Viewer $Window '1 of 2'
    if ($capture) { Wait-FindPixels $Window $true }
    [ReadingProbe]::CloseFind($Window)
    if ($capture) { Wait-FindPixels $Window $false }
    if ([SumiSmoke]::ReadText($Window).Contains('*') -or [SumiSmoke]::IsWindowEnabled([SumiSmoke]::GetDlgItem($Window, 109))) {
        throw 'Searching dirtied the PDF or enabled Save Annotations.'
    }

    # Leave work queued on the long document. The next tab has no matching text.
    [ReadingProbe]::OpenFind($Window)
    [ReadingProbe]::SetFindQuery($Window, 'Reading notes')
    Wait-Find $Viewer $Window '1 of *'
    [ReadingProbe]::ClickTab($Window, 'two pages.pdf')
    [void](Wait-Page $Viewer 'two pages.pdf' '1 / 2' ([Diagnostics.Stopwatch]::StartNew()) -Quiet)
    if ([SumiSmoke]::IsWindowVisible($edit)) { throw 'The previous tab left Find visible.' }
    [ReadingProbe]::OpenFind($Window)
    if ([SumiSmoke]::ReadText($edit) -ne '') { throw 'The previous tab leaked its query into the new tab.' }
    [ReadingProbe]::SetFindQuery($Window, $literal)
    Wait-Find $Viewer $Window 'No matches'
    [SumiSmoke]::Click($Window, 164)
    [ReadingProbe]::ClickTab($Window, 'Reading notes.pdf')
    [void](Wait-Page $Viewer 'Reading notes.pdf' '1 / 240' ([Diagnostics.Stopwatch]::StartNew()) -Quiet)
    if ([SumiSmoke]::IsWindowVisible($edit)) { throw 'Find was visible after returning to the reading tab.' }
    if ($capture) { Wait-FindPixels $Window $false }
    Write-Host "Verified literal Find text, navigation across 37 pages, tab isolation and clean document state. Painted highlight checks: $capture."
}
