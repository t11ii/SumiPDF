function Invoke-SessionRestoreLaunch([string]$ExecutablePath, [string]$ProfileDirectory, [string[]]$Documents,
    [scriptblock]$Check) {
    $viewer = Start-SmokeViewer $ExecutablePath $ProfileDirectory $Documents
    try {
        & $Check $viewer
        $viewer.Refresh()
        if (!$viewer.CloseMainWindow() -or !$viewer.WaitForExit(5000) -or $viewer.ExitCode -ne 0) {
            throw 'The session restore test viewer did not close normally.'
        }
    } finally {
        try {
            if (!$viewer.HasExited) {
                $viewer.Kill()
                [void]$viewer.WaitForExit(5000)
            }
        } finally { $viewer.Dispose() }
    }
}

function Wait-SessionPage([Diagnostics.Process]$Viewer, [string]$Filename, [string]$Page) {
    Wait-Page $Viewer $Filename $Page ([Diagnostics.Stopwatch]::StartNew()) -Quiet
}

function Assert-SessionTabs([IntPtr]$Window, [int]$Count, [int]$Selected) {
    $observedCount = [ReadingProbe]::TabCount($Window)
    $observedSelected = [ReadingProbe]::SelectedTabIndex($Window)
    if ($observedCount -ne $Count -or $observedSelected -ne $Selected) {
        throw "Expected $Count tabs with index $Selected selected, found $observedCount tabs with index $observedSelected selected."
    }
}

function Wait-EmptySession([Diagnostics.Process]$Viewer, [string]$AppTitle) {
    $window = Wait-Page $Viewer $AppTitle '— / —' ([Diagnostics.Stopwatch]::StartNew()) '' -Quiet
    if ([SumiSmoke]::ReadText($window) -cne $AppTitle) { throw 'Expected an empty viewer.' }
    # A restore can add tabs after the window becomes visible.
    $settle = [Diagnostics.Stopwatch]::StartNew()
    do {
        Assert-SessionTabs $window 0 -1
        Start-Sleep -Milliseconds 50
    } while ($settle.ElapsedMilliseconds -lt 500)
    $window
}

function Test-SessionRestore([string]$ExecutablePath, [string]$Folder, [string]$AppTitle) {
    $profileDirectory = Join-Path $Folder 'Session profile'
    $firstPdf = Join-Path $Folder 'Session first.pdf'
    $middlePdf = Join-Path $Folder 'Session middle.pdf'
    $lastPdf = Join-Path $Folder 'Session last.pdf'
    $explicitPdf = Join-Path $Folder 'Session explicit.pdf'
    Write-ReadingPdf $firstPdf 3
    Write-ReadingPdf $middlePdf 5
    Write-ReadingPdf $lastPdf 4
    Write-ReadingPdf $explicitPdf 2

    # Give each tab a different page and leave the middle tab selected.
    Invoke-SessionRestoreLaunch $ExecutablePath $profileDirectory @($firstPdf, $middlePdf, $lastPdf) {
        param($viewer)
        $window = Wait-SessionPage $viewer 'Session last.pdf' '1 / 4'
        [ReadingProbe]::Command($window, 122) # Last page
        [void](Wait-SessionPage $viewer 'Session last.pdf' '4 / 4')
        [ReadingProbe]::TabShortcut($window, 9, $false, 0, 3) # Ctrl+Tab
        [void](Wait-SessionPage $viewer 'Session first.pdf' '1 / 3')
        [SumiSmoke]::NextPage($window)
        [void](Wait-SessionPage $viewer 'Session first.pdf' '2 / 3')
        [ReadingProbe]::TabShortcut($window, 9, $false, 1, 3)
        [void](Wait-SessionPage $viewer 'Session middle.pdf' '1 / 5')
        [SumiSmoke]::NextPage($window)
        [SumiSmoke]::NextPage($window)
        [void](Wait-SessionPage $viewer 'Session middle.pdf' '3 / 5')
    }

    Invoke-SessionRestoreLaunch $ExecutablePath $profileDirectory @() {
        param($viewer)
        $window = Wait-SessionPage $viewer 'Session middle.pdf' '3 / 5'
        Assert-SessionTabs $window 3 1
        if (![ReadingProbe]::SessionRestoreChecked($window)) { throw 'Session restore should be checked by default.' }
        [ReadingProbe]::TabShortcut($window, 9, $false, 2, 3)
        [void](Wait-SessionPage $viewer 'Session last.pdf' '4 / 4')
        [ReadingProbe]::TabShortcut($window, 9, $false, 0, 3)
        [void](Wait-SessionPage $viewer 'Session first.pdf' '2 / 3')
        [ReadingProbe]::ClickTab($window, 'Session middle.pdf')
        [void](Wait-SessionPage $viewer 'Session middle.pdf' '3 / 5')
    }

    # The selected file is unreadable, the last file moved, and the first one survives.
    [IO.File]::WriteAllText($middlePdf, 'This is no longer a PDF.')
    Move-Item -LiteralPath $lastPdf -Destination ($lastPdf + '.moved')
    Invoke-SessionRestoreLaunch $ExecutablePath $profileDirectory @() {
        param($viewer)
        $window = Wait-SessionPage $viewer 'Session first.pdf' '2 / 3'
        Assert-SessionTabs $window 1 0
    }

    Invoke-SessionRestoreLaunch $ExecutablePath $profileDirectory @($explicitPdf) {
        param($viewer)
        $window = Wait-SessionPage $viewer 'Session explicit.pdf' '1 / 2'
        Assert-SessionTabs $window 1 0
        if (![ReadingProbe]::SessionRestoreChecked($window)) { throw 'Session restore was switched off unexpectedly.' }
        [ReadingProbe]::ToggleSessionRestore($window, $false)
        [void](Wait-SessionPage $viewer 'Session explicit.pdf' '1 / 2')
        Assert-SessionTabs $window 1 0
    }

    Invoke-SessionRestoreLaunch $ExecutablePath $profileDirectory @() {
        param($viewer)
        $window = Wait-EmptySession $viewer $AppTitle
        if ([ReadingProbe]::SessionRestoreChecked($window)) { throw 'Session restore was checked after being switched off.' }
        [ReadingProbe]::ToggleSessionRestore($window, $true)
        Assert-SessionTabs $window 0 -1
    }

    Invoke-SessionRestoreLaunch $ExecutablePath $profileDirectory @() {
        param($viewer)
        $window = Wait-SessionPage $viewer 'Session first.pdf' '2 / 3'
        Assert-SessionTabs $window 1 0
        if (![ReadingProbe]::SessionRestoreChecked($window)) { throw 'Session restore did not stay on after re-enabling it.' }
        [ReadingProbe]::ClickClose($window, 'Session first.pdf')
        [void](Wait-EmptySession $viewer $AppTitle)
    }
    Invoke-SessionRestoreLaunch $ExecutablePath $profileDirectory @() {
        param($viewer)
        [void](Wait-EmptySession $viewer $AppTitle)
    }
    Write-Host 'Verified session restore, including tab order, selected tab, page positions, missing and unreadable files, explicit arguments, and the persistent on/off switch.'
}
