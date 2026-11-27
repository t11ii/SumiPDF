function Write-ReadingPdf([string]$Path, [int]$Count) {
    $objects = [Collections.Generic.List[string]]::new()
    $kids = (0..($Count - 1) | ForEach-Object { "$(4 + 2 * $_) 0 R" }) -join ' '
    $objects.Add('<< /Type /Catalog /Pages 2 0 R >>')
    $objects.Add("<< /Type /Pages /Count $Count /Kids [$kids] >>")
    $objects.Add('<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>')
    $palette = @('0.78 0.865 0.95', '0.96 0.88 0.78', '0.825 0.92 0.83', '0.905 0.845 0.95')
    for ($page = 1; $page -le $Count; $page++) {
        $stream = $objects.Count + 2
        $objects.Add("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents $stream 0 R >>")
        $color = if ($page -eq $Count) { '0.92 0.745 0.835' } else { $palette[($page - 1) % $palette.Length] }
        $drawing = "$color rg 0 0 612 792 re f`n0.12 0.16 0.2 rg BT /F1 24 Tf 48 696 Td (Reading notes) Tj " +
            "/F1 11 Tf 0 -26 Td (Page $page of $Count) Tj ET`n"
        $objects.Add("<< /Length $($drawing.Length) >>`nstream`n${drawing}endstream")
    }
    Write-PdfObjects $Path $objects.ToArray()
}

function Read-Status([IntPtr]$Window) {
    [SumiSmoke]::ReadText([SumiSmoke]::GetDlgItem($Window, 112))
}

function Wait-Viewport([IntPtr]$Window, [string]$Pattern, [int]$Timeout = 10000, [char]$CenterColor = 0,
    [switch]$IfAvailable) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    do {
        $stripe = [ReadingProbe]::Viewport($Window)
        if ($CenterColor -ne [char]0) {
            if ($stripe -and $stripe[[int][Math]::Floor($stripe.Length / 2)] -eq $CenterColor) { return $stripe }
        } elseif ($stripe -match $Pattern) { return $stripe }
        Start-Sleep -Milliseconds 35
    } while ($clock.ElapsedMilliseconds -lt $Timeout)
    if ($IfAvailable) { return }
    $expected = if ($CenterColor -ne [char]0) { "center color $CenterColor" } else { $Pattern }
    throw "Rendered viewport did not match '$expected': '$stripe'."
}

function Save-ReadingScreenshot([IntPtr]$Window, [string]$Path) {
    $size = [ReadingProbe]::ClientSize($Window)
    $bitmap = [Drawing.Bitmap]::new($size[0], $size[1])
    try {
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try {
            $dc = $graphics.GetHdc()
            try {
                $stripe = [ReadingProbe]::Capture($Window, $dc, $size[0], $size[1])
                if ($stripe -notmatch 'B{4,}\.+O{4,}') { throw 'Could not capture both pages in the application window.' }
            } finally { $graphics.ReleaseHdc($dc) }
        } finally { $graphics.Dispose() }
        # A parent-only WM_PRINT can omit native children. Check that the tab
        # row actually contributed detail before publishing the captured PNG.
        $tabColors = [Collections.Generic.HashSet[int]]::new()
        for ($y = 4; $y -lt [Math]::Min(30, $bitmap.Height); $y += 2) {
            for ($x = 12; $x -lt $bitmap.Width / 2; $x += 3) {
                [void]$tabColors.Add($bitmap.GetPixel($x, $y).ToArgb())
            }
        }
        if ($tabColors.Count -lt 3) { throw 'The capture did not include the native tab strip.' }
        $bitmap.Save([IO.Path]::GetFullPath($Path), [Drawing.Imaging.ImageFormat]::Png)
    } finally { $bitmap.Dispose() }
}

function Test-ReadingTabs([Diagnostics.Process]$Viewer, [IntPtr]$Window, [string]$ScreenshotPath) {
    $name = 'Reading notes.pdf'
    $timer = [Diagnostics.Stopwatch]::StartNew()
    [ReadingProbe]::TabShortcut($Window, 9, $false, 0, 3) # Ctrl+Tab, wrapping from last to first
    [void](Wait-Page $Viewer $name '1 / 240' $timer)
    $Viewer.Refresh()
    $baseline = $Viewer.WorkingSet64
    [SumiSmoke]::Click($Window, 105)
    [void](Wait-Page $Viewer $name '1 / 240' ([Diagnostics.Stopwatch]::StartNew()) '125%')
    if ((Read-Status $Window) -notmatch '表示中 1 ページ') { throw 'A zoomed portrait page should fill the viewport.' }
    # Probe whether the runner exposes the release EXE's painted client surface.
    # Once a fixture color is visible, all later pixel checks are mandatory.
    $capture = [bool](Wait-Viewport $Window 'B{30,}' 3000 -IfAvailable)
    if (!$capture) { Write-Host "No PDF pixels in the runner's window capture. Checking the visible-page status." }

    # A sub-detent wheel should reveal page 2 while the leading page remains 1.
    for ($steps = 0; $steps -lt 60; $steps++) {
        [ReadingProbe]::Wheel($Window, -30)
        Start-Sleep -Milliseconds 25
        if ((Read-Status $Window).Contains('表示中 1–2 ページ')) { break }
    }
    [void](Wait-Page $Viewer $name '1 / 240' ([Diagnostics.Stopwatch]::StartNew()) '表示中 1–2 ページ')
    # Leave enough of the second page exposed for a visible, reproducible boundary.
    [ReadingProbe]::Wheel($Window, -120)
    if ($capture) {
        $before = Wait-Viewport $Window 'B{4,}\.+O{4,}' 2500
        [ReadingProbe]::Wheel($Window, -30)
        $edge = $before.LastIndexOf('B')
        $moved = [Diagnostics.Stopwatch]::StartNew()
        do {
            $savedStripe = Wait-Viewport $Window 'B{4,}\.+O{4,}'
            $movement = $edge - $savedStripe.LastIndexOf('B')
            if ($movement -gt 0) { break }
            Start-Sleep -Milliseconds 25
        } while ($moved.ElapsedMilliseconds -lt 3000)
        if ($movement -le 0 -or $movement -ge $savedStripe.Length / 5) {
            throw "A quarter-detent wheel did not smoothly shift the page boundary: $movement samples."
        }
    }
    if ($ScreenshotPath) {
        try { Save-ReadingScreenshot $Window $ScreenshotPath }
        catch { Write-Warning "Application screenshot unavailable on this runner: $($_.Exception.Message)" }
    }

    [ReadingProbe]::TabShortcut($Window, 9, $true, 2, 3) # Ctrl+Shift+Tab, wrapping back
    [void](Wait-Page $Viewer 'two pages.pdf' '1 / 2' ([Diagnostics.Stopwatch]::StartNew()))
    [ReadingProbe]::ClickTab($Window, $name)
    [void](Wait-Page $Viewer $name '1 / 240' ([Diagnostics.Stopwatch]::StartNew()) '表示中 1–2 ページ')
    if (!(Read-Status $Window).Contains('125%')) { throw 'Switching tabs lost the zoom.' }
    if ($capture) {
        $restored = Wait-Viewport $Window 'B{4,}\.+O{4,}'
        if ($restored -ne $savedStripe) { throw 'Switching tabs lost the sub-page scroll position.' }
    }

    # Visit and settle on 44 pages to exercise eviction, then invalidate a burst
    # of queued page requests and verify the final destination independently.
    $colors = 'BOGV'
    $paging = [Diagnostics.Stopwatch]::StartNew()
    for ($page = 2; $page -le 45; $page++) {
        [ReadingProbe]::Command($Window, 103)
        [void](Wait-Page $Viewer $name "$page / 240" ([Diagnostics.Stopwatch]::StartNew()) '125%' -Quiet)
        if ($capture) { [void](Wait-Viewport $Window -CenterColor $colors[($page - 1) % 4]) }
    }
    $paging.Stop()
    $burst = [Diagnostics.Stopwatch]::StartNew()
    foreach ($i in 1..120) { [ReadingProbe]::Command($Window, 103) }
    [ReadingProbe]::Command($Window, 122) # Last, then reverse before the renderer settles.
    [ReadingProbe]::Command($Window, 121)
    [ReadingProbe]::Command($Window, 122)
    [void](Wait-Page $Viewer $name '240 / 240' ([Diagnostics.Stopwatch]::StartNew()) '125%')
    if ($capture) { [void](Wait-Viewport $Window -CenterColor 'R') }
    $burst.Stop()
    $Viewer.Refresh()
    $delta = $Viewer.WorkingSet64 - $baseline
    Write-Host (("240-page fixture: 44 page changes {0} ms, final page after navigation burst {1} ms, " +
        "working set {2:N1} -> {3:N1} MiB, delta {4:N1} MiB, peak {5:N1} MiB; native pixels captured: {6}.") -f
        $paging.ElapsedMilliseconds, $burst.ElapsedMilliseconds, ($baseline / 1MB), ($Viewer.WorkingSet64 / 1MB),
        ($delta / 1MB), ($Viewer.PeakWorkingSet64 / 1MB), $capture)
    if ($delta -gt 128MB -or $Viewer.PeakWorkingSet64 -gt 384MB) {
        throw 'The release viewer exceeded the working-set sanity budget for the synthetic document.'
    }
    [ReadingProbe]::ClickTab($Window, 'two pages.pdf')
    [void](Wait-Page $Viewer 'two pages.pdf' '1 / 2' ([Diagnostics.Stopwatch]::StartNew()))
}
