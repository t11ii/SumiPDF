param([string]$Executable = "$PSScriptRoot/../target/release/SumiPDF.exe")
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Query the native controls through the same messages they receive from the user.
# WM_GETTEXT is marshalled by Windows for controls belonging to another process.
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

public static class SumiSmoke {
    [DllImport("user32.dll", CharSet=CharSet.Unicode, ExactSpelling=true)]
    static extern int GetClassNameW(IntPtr h, StringBuilder text, int max);
    [DllImport("user32.dll", SetLastError=true)]
    public static extern IntPtr GetDlgItem(IntPtr parent, int id);
    [DllImport("user32.dll")]
    public static extern bool IsWindowEnabled(IntPtr window);
    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr window);
    [StructLayout(LayoutKind.Sequential)]
    struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll", SetLastError=true)]
    static extern bool GetClientRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")]
    static extern uint GetDpiForWindow(IntPtr window);
    [DllImport("user32.dll")]
    static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll", CharSet=CharSet.Unicode, ExactSpelling=true, SetLastError=true)]
    static extern IntPtr SendMessageTimeoutW(IntPtr h, uint message, UIntPtr wparam,
        [Out] StringBuilder text, uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll", SetLastError=true)]
    static extern bool PostMessageW(IntPtr h, uint message, UIntPtr wparam, IntPtr lparam);

    public static string ClassName(IntPtr h) {
        var value = new StringBuilder(256);
        GetClassNameW(h, value, value.Capacity);
        return value.ToString();
    }
    public static string ReadText(IntPtr h) {
        if (h == IntPtr.Zero) return "<no control>";
        var value = new StringBuilder(1024);
        UIntPtr result;
        if (SendMessageTimeoutW(h, 0x000D, new UIntPtr((uint)value.Capacity), value, 2, 2000, out result) == IntPtr.Zero)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "WM_GETTEXT failed or timed out.");
        return value.ToString();
    }
    public static void NextPage(IntPtr window) {
        if (!PostMessageW(window, 0x0111, new UIntPtr(103u), IntPtr.Zero))
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Could not send the Next Page command.");
    }
    static void Post(IntPtr handle, uint message, uint keys, IntPtr position) {
        if (handle == IntPtr.Zero || !PostMessageW(handle, message, new UIntPtr(keys), position))
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Could not post UI input.");
    }
    public static void Click(IntPtr window, int id) {
        Post(GetDlgItem(window, id), 0x00F5, 0, IntPtr.Zero); // BM_CLICK
    }
    public static void Pick(IntPtr window, int id, uint index) {
        var picker = GetDlgItem(window, id);
        UIntPtr selected;
        if (picker == IntPtr.Zero || SendMessageTimeoutW(picker, 0x014E, new UIntPtr(index),
            null, 2, 2000, out selected) == IntPtr.Zero || selected.ToUInt64() != index)
            throw new InvalidOperationException("Could not select an item in control " + id);
        Post(window, 0x0111, (1u << 16) | (uint)id, picker); // WM_COMMAND / CBN_SELCHANGE
    }
    static IntPtr Point(int x, int y) {
        return new IntPtr((y << 16) | (x & 0xffff));
    }
    public static void DragOnPage(IntPtr window, int pageWidth, int pageHeight,
        double fromX, double fromY, double toX, double toY) {
        var canvas = GetDlgItem(window, 110);
        var previous = SetThreadDpiAwarenessContext(new IntPtr(-4)); // Per Monitor V2, same as viewer.
        try {
            Rect r;
            if (!GetClientRect(canvas, out r)) throw new Win32Exception(Marshal.GetLastWin32Error());
            int margin = (int)Math.Round(18 * Math.Max(96u, GetDpiForWindow(canvas)) / 96.0);
            double fit = Math.Min((r.Right - margin * 2.0) / pageWidth, (r.Bottom - margin * 2.0) / pageHeight);
            int w = (int)Math.Round(pageWidth * fit), h = (int)Math.Round(pageHeight * fit);
            int x = (r.Right - w) / 2, y = (r.Bottom - h) / 2;
            var start = Point(x + (int)(w * fromX), y + (int)(h * fromY));
            var end = Point(x + (int)(w * toX), y + (int)(h * toY));
            Post(canvas, 0x0201, 1, start); // WM_LBUTTONDOWN
            Post(canvas, 0x0200, 1, end);   // WM_MOUSEMOVE
            Post(canvas, 0x0202, 0, end);   // WM_LBUTTONUP
        } finally {
            if (previous != IntPtr.Zero) SetThreadDpiAwarenessContext(previous);
        }
    }
}
'@

function Write-TestPdf([string]$Path) {
    # Every inserted character is ASCII, so StringBuilder lengths are also byte offsets.
    $pdf = [Text.StringBuilder]::new("%PDF-1.4`n")
    $objects = @(
        '<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [8 0 R] >> >>'
        '<< /Type /Pages /Count 2 /Kids [3 0 R 4 0 R] >>'
        '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Resources << /Font << /F1 7 0 R >> >> /Contents 5 0 R >>'
        '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << >> /Contents 6 0 R /Annots [8 0 R] >>'
    )
    foreach ($drawing in @(
        "1 0 0 rg 0 0 150 150 re f`n0 g BT /F1 20 Tf 40 180 Td (Highlight me) Tj ET`n",
        "0 0 1 rg 0 0 150 150 re f`n"
    )) {
        $objects += "<< /Length $($drawing.Length) >>`nstream`n${drawing}endstream"
    }
    $objects += '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>'
    $objects += '<< /Type /Annot /Subtype /Widget /FT /Sig /T (Approval) /P 4 0 R /F 4 /Rect [165 150 285 185] >>'
    $size = $objects.Count + 1
    $xref = [Text.StringBuilder]::new("xref`n0 $size`n0000000000 65535 f `n")
    for ($i = 0; $i -lt $objects.Count; $i++) {
        [void]$xref.Append(("{0:D10} 00000 n `n" -f $pdf.Length))
        [void]$pdf.Append("$($i + 1) 0 obj`n$($objects[$i])`nendobj`n")
    }
    $offset = $pdf.Length
    [void]$pdf.Append($xref.ToString())
    [void]$pdf.Append("trailer`n<< /Root 1 0 R /Size $size >>`nstartxref`n$offset`n%%EOF`n")
    [IO.File]::WriteAllText($Path, $pdf.ToString(), [Text.Encoding]::ASCII)
}

function Wait-Page([Diagnostics.Process]$Viewer, [string]$Filename, [string]$Expected, [Diagnostics.Stopwatch]$Elapsed) {
    $class = $title = $page = $status = '<none>'
    do {
        $Viewer.Refresh()
        if ($Viewer.HasExited) { throw "SumiPDF exited with code $($Viewer.ExitCode) while waiting for page '$Expected'." }
        $window = $Viewer.MainWindowHandle
        if ($window -ne [IntPtr]::Zero) {
            $class = [SumiSmoke]::ClassName($window)
            $title = [SumiSmoke]::ReadText($window)
            if ($class -eq 'SumiPDF.Window') {
                $page = [SumiSmoke]::ReadText([SumiSmoke]::GetDlgItem($window, 111))
                $status = [SumiSmoke]::ReadText([SumiSmoke]::GetDlgItem($window, 112))
                if ($title.Contains($Filename) -and $page -eq $Expected -and $status.StartsWith('ページ全体')) {
                    $Elapsed.Stop()
                    $Viewer.Refresh()
                    Write-Host ("Page '{0}' ready after {1} ms; working set {2} bytes; peak working set {3} bytes." -f
                        $page, $Elapsed.ElapsedMilliseconds, $Viewer.WorkingSet64, $Viewer.PeakWorkingSet64)
                    Write-Host "Title '$title'; status '$status'."
                    return $window
                }
            }
        }
        Start-Sleep -Milliseconds 50
    } while ($Elapsed.Elapsed.TotalSeconds -lt 30)
    throw "Timed out waiting for '$Expected' in '$Filename'. Window class '$class', title '$title', page '$page', status '$status'."
}

function Wait-HighlightState([Diagnostics.Process]$Viewer, [IntPtr]$Window, [string]$ModeText, [bool]$Dirty) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        if ($Viewer.HasExited) { throw "SumiPDF exited while checking highlight state." }
        $title = [SumiSmoke]::ReadText($Window)
        $status = [SumiSmoke]::ReadText([SumiSmoke]::GetDlgItem($Window, 112))
        $save = [SumiSmoke]::IsWindowEnabled([SumiSmoke]::GetDlgItem($Window, 109))
        $undo = [SumiSmoke]::IsWindowEnabled([SumiSmoke]::GetDlgItem($Window, 114))
        if ($status.Contains($ModeText) -and $title.Contains('*') -eq $Dirty -and $save -eq $Dirty -and $undo -eq $Dirty) { return }
        Start-Sleep -Milliseconds 50
    } while ($timer.Elapsed.TotalSeconds -lt 15)
    throw "Expected '$ModeText', dirty $Dirty. Status '$status', title '$title', Save $save, Undo $undo."
}

function Wait-SignatureState([IntPtr]$Window, [string]$StatusText, [bool]$CanSave, [bool]$CanPickImage) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        $status = [SumiSmoke]::ReadText([SumiSmoke]::GetDlgItem($Window, 112))
        $save = [SumiSmoke]::GetDlgItem($Window, 129)
        $watermark = [SumiSmoke]::GetDlgItem($Window, 128)
        if ($status.Contains($StatusText) -and [SumiSmoke]::IsWindowVisible($save) -and
            ([SumiSmoke]::IsWindowEnabled($save) -eq $CanSave) -and
            ([SumiSmoke]::IsWindowEnabled($watermark) -eq $CanPickImage)) { return }
        Start-Sleep -Milliseconds 50
    } while ($timer.Elapsed.TotalSeconds -lt 15)
    throw "Expected signature status '$StatusText', save $CanSave, watermark $CanPickImage; saw '$status'."
}

$exe = (Resolve-Path -LiteralPath $Executable).Path
$dir = Join-Path ([IO.Path]::GetTempPath()) ("SumiPDF-smoke-" + [Guid]::NewGuid().ToString('N'))
$fixture = Join-Path $dir 'two pages.pdf'
$viewer = $null
try {
    [void][IO.Directory]::CreateDirectory($dir)
    Write-TestPdf $fixture
    & (Join-Path $PSScriptRoot 'signingCli.ps1') -Executable $exe -Pdf $fixture
    Write-Host "Release EXE size $((Get-Item -LiteralPath $exe).Length) bytes."
    # The first observation includes process launch. Both include polling latency.
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $viewer = Start-Process -FilePath $exe -ArgumentList ('"{0}"' -f $fixture) -PassThru
    $window = Wait-Page $viewer 'two pages.pdf' '1 / 2' $timer
    $timer.Restart()
    [SumiSmoke]::NextPage($window)
    [void](Wait-Page $viewer 'two pages.pdf' '2 / 2' $timer)
    # The second page is graphics-only. Drag to mark a rectangle, then undo.
    [SumiSmoke]::Click($window, 108)
    Wait-HighlightState $viewer $window '範囲をドラッグ' $false
    [SumiSmoke]::DragOnPage($window, 300, 200, 0.30, 0.40, 0.62, 0.54)
    Wait-HighlightState $viewer $window '範囲をドラッグ' $true
    [SumiSmoke]::Click($window, 114)
    Wait-HighlightState $viewer $window '範囲をドラッグ' $false
    [SumiSmoke]::Click($window, 115)
    Wait-HighlightState $viewer $window '下線（黄）' $false
    [SumiSmoke]::Pick($window, 116, 3)
    Wait-HighlightState $viewer $window '下線（青）' $false
    [SumiSmoke]::DragOnPage($window, 300, 200, 0.62, 0.60, 0.30, 0.60)
    Wait-HighlightState $viewer $window '下線（青）' $true
    [SumiSmoke]::Click($window, 114)
    Wait-HighlightState $viewer $window '下線（青）' $false
    [SumiSmoke]::Click($window, 115)
    Wait-HighlightState $viewer $window 'ページ全体' $false
    [SumiSmoke]::Click($window, 102)
    $timer.Restart()
    [void](Wait-Page $viewer 'two pages.pdf' '1 / 2' $timer)
    # The baseline is at y=180 on a 300-point-tall page. Drag through the glyphs.
    [SumiSmoke]::Click($window, 108)
    Wait-HighlightState $viewer $window '文字をドラッグ' $false
    Wait-HighlightState $viewer $window '蛍光ペン（青）' $false
    [SumiSmoke]::DragOnPage($window, 200, 300, 0.22, 0.38, 0.56, 0.38)
    Wait-HighlightState $viewer $window '文字をドラッグ' $true
    [SumiSmoke]::Click($window, 114)
    Wait-HighlightState $viewer $window '文字をドラッグ' $false
    [SumiSmoke]::Click($window, 115)
    Wait-HighlightState $viewer $window '下線（青）' $false
    [SumiSmoke]::DragOnPage($window, 200, 300, 0.56, 0.38, 0.22, 0.38)
    Wait-HighlightState $viewer $window '文字をドラッグ' $true
    # Keep the pending underline while switching to signing. The existing empty field is on page 2.
    [SumiSmoke]::Click($window, 107)
    Wait-SignatureState $window '青枠が選択した署名欄' $true $true
    if ([SumiSmoke]::ReadText([SumiSmoke]::GetDlgItem($window, 111)) -ne '2 / 2') {
        throw 'Selecting the existing field did not navigate to its page.'
    }
    [SumiSmoke]::Pick($window, 127, 0)
    Wait-SignatureState $window '枠の角から' $false $true
    [SumiSmoke]::DragOnPage($window, 300, 200, 0.83, 0.35, 0.40, 0.15)
    Wait-SignatureState $window 'ドラッグで描き直す' $true $true
    [SumiSmoke]::Click($window, 102)
    Wait-SignatureState $window '署名の位置は 2 ページ' $false $true
    [SumiSmoke]::Pick($window, 127, 2)
    Wait-SignatureState $window 'ページに表示しない電子署名' $true $false
    [SumiSmoke]::Click($window, 130)
    Wait-HighlightState $viewer $window 'ページ全体' $true
    if ([SumiSmoke]::IsWindowVisible([SumiSmoke]::GetDlgItem($window, 129))) {
        throw 'The signature toolbar remained visible after cancellation.'
    }
    [SumiSmoke]::Click($window, 114)
    Wait-HighlightState $viewer $window 'ページ全体' $false
    if (-not $viewer.CloseMainWindow() -or -not $viewer.WaitForExit(5000)) {
        throw 'The viewer did not close normally.'
    }
    if ($viewer.ExitCode -ne 0) { throw "The viewer exited with code $($viewer.ExitCode)." }
} finally {
    if ($null -ne $viewer) {
        if (-not $viewer.HasExited) { $viewer.Kill(); $viewer.WaitForExit() }
        $viewer.Dispose()
    }
    if ([IO.Directory]::Exists($dir)) { [IO.Directory]::Delete($dir, $true) }
}
