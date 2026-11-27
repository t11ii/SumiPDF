function Wait-Find([Diagnostics.Process]$Viewer, [IntPtr]$Window, [string]$Expected) {
    $status = [SumiSmoke]::GetDlgItem($Window, 165)
    $clock = [Diagnostics.Stopwatch]::StartNew()
    do {
        if ($Viewer.HasExited) { throw 'Viewer exited during Find.' }
        $text = [SumiSmoke]::ReadText($status)
        if ([SumiSmoke]::IsWindowVisible($status) -and $text -like $Expected) { return }
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

function Test-Find([Diagnostics.Process]$Viewer, [IntPtr]$Window) {
    $query = 'Which section'
    $literal = $query + '?'
    $capture = [bool](Wait-Viewport $Window 'B{15,}' 2500 -IfAvailable)
    if ($capture) { Wait-FindPixels $Window $false }
    [ReadingProbe]::OpenFind($Window)
    [ReadingProbe]::SetFindQuery($Window, $query)
    Wait-Find $Viewer $Window '1 of 3'
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
