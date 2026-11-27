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
    [DllImport("user32.dll", CharSet=CharSet.Unicode, ExactSpelling=true)]
    static extern IntPtr FindWindowExW(IntPtr parent, IntPtr previous, string kind, string title);
    [DllImport("user32.dll")]
    static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);

    public static IntPtr Dialog(int processId, string title) {
        var h = IntPtr.Zero;
        while ((h = FindWindowExW(IntPtr.Zero, h, "#32770", title)) != IntPtr.Zero) {
            uint pid;
            GetWindowThreadProcessId(h, out pid);
            if (pid == (uint)processId && IsWindowVisible(h)) return h;
        }
        return IntPtr.Zero;
    }
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", ExactSpelling=true, CharSet=CharSet.Unicode)]
    static extern IntPtr SendString(IntPtr h, uint message, UIntPtr wparam, string text,
        uint flags, uint timeout, out UIntPtr result);
    public static void EnterTestPassword(IntPtr dialog, string password) {
        UIntPtr result;
        if (SendString(GetDlgItem(dialog, 1001), 0x000c, UIntPtr.Zero,
            password, 2, 2000, out result) == IntPtr.Zero || result == UIntPtr.Zero)
            throw new InvalidOperationException("Could not fill the test password dialog.");
        Click(dialog, 1);
    }
    public static void ChooseCertificate(IntPtr dialog, string name) {
        UIntPtr index;
        if (SendString(GetDlgItem(dialog, 1001), 0x018c, UIntPtr.MaxValue,
            name, 2, 2000, out index) == IntPtr.Zero || index.ToUInt64() > int.MaxValue)
            throw new InvalidOperationException("The test identity is missing from the certificate picker.");
        Click(dialog, 1);
    }
    public static void CloseDialog(IntPtr dialog) {
        Post(dialog, 0x0010, 0, IntPtr.Zero); // WM_CLOSE
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
    [DllImport("user32.dll", SetLastError=true)]
    static extern bool GetWindowRect(IntPtr h, out Rect r);
    [DllImport("user32.dll", SetLastError=true)]
    static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int height, uint flags);
    [DllImport("user32.dll")]
    static extern int MapWindowPoints(IntPtr from, IntPtr to, ref Rect rect, uint count);

    public static void CheckResize(IntPtr window, int clientWidth, bool signing) {
        var previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
        try {
            Rect outer, client;
            if (!GetWindowRect(window, out outer) || !GetClientRect(window, out client))
                throw new Win32Exception(Marshal.GetLastWin32Error());
            int dpi = (int)GetDpiForWindow(window);
            int width = (clientWidth * dpi + 48) / 96;
            if (!SetWindowPos(window, IntPtr.Zero, 0, 0, outer.Right - outer.Left - client.Right + width,
                outer.Bottom - outer.Top, 0x0016)) // NOMOVE | NOZORDER | NOACTIVATE
                throw new Win32Exception(Marshal.GetLastWin32Error());
            if (!GetClientRect(window, out client) || client.Right != width)
                throw new InvalidOperationException("Did not reach the requested client width.");
            int[] icons = signing ? new[] {101,102,103,104,105,106,107,108,109,114,115,128,129,130,131,132}
                                  : new[] {101,102,103,104,105,106,107,108,109,114,115};
            var occupied = new System.Collections.Generic.List<Rect>();
            // Include the page number, color combo and signature-position combo in overlap checks.
            foreach (int id in signing ? new[] {111,116,127} : new[] {111,116}) {
                Rect field;
                if (!GetWindowRect(GetDlgItem(window, id), out field)) throw new Win32Exception();
                MapWindowPoints(IntPtr.Zero, window, ref field, 2);
                occupied.Add(field);
            }
            foreach (int id in icons) {
                var button = GetDlgItem(window, id);
                Rect r;
                if (!IsWindowVisible(button) || !GetWindowRect(button, out r)) throw new InvalidOperationException("Missing icon " + id);
                MapWindowPoints(IntPtr.Zero, window, ref r, 2);
                if (r.Right - r.Left != (36 * dpi + 48) / 96 || r.Bottom - r.Top != (32 * dpi + 48) / 96 ||
                    r.Left < 0 || r.Top < 0 || r.Right > width || r.Bottom > client.Bottom)
                    throw new InvalidOperationException("Clipped or resized icon " + id + " at width " + clientWidth);
                foreach (var other in occupied)
                    if (other.Left < r.Right && r.Left < other.Right && other.Top < r.Bottom && r.Top < other.Bottom)
                        throw new InvalidOperationException("Overlapping controls at icon " + id + " at width " + clientWidth);
                occupied.Add(r);
            }
        } finally { if (previous != IntPtr.Zero) SetThreadDpiAwarenessContext(previous); }
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

function Wait-Dialog([int]$ViewerId, [string]$Title) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        $popup = [SumiSmoke]::Dialog($ViewerId, $Title)
        if ($popup -ne [IntPtr]::Zero) { return $popup }
        Start-Sleep -Milliseconds 50
    } while ($timer.Elapsed.TotalSeconds -lt 15)
    throw "The dialog did not open: $Title"
}

function Wait-SignatureState([IntPtr]$Window, [string]$StatusText, [bool]$CanSave, [bool]$CanPickImage) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    do {
        $status = [SumiSmoke]::ReadText([SumiSmoke]::GetDlgItem($Window, 112))
        $save = [SumiSmoke]::GetDlgItem($Window, 129)
        $watermark = [SumiSmoke]::GetDlgItem($Window, 128)
        $certificate = [SumiSmoke]::GetDlgItem($Window, 131)
        if ($status.Contains($StatusText) -and [SumiSmoke]::IsWindowEnabled($Window) -and [SumiSmoke]::IsWindowVisible($save) -and
            [SumiSmoke]::IsWindowVisible($certificate) -and [SumiSmoke]::IsWindowEnabled($certificate) -and
            ([SumiSmoke]::IsWindowEnabled($save) -eq $CanSave) -and
            ([SumiSmoke]::IsWindowEnabled($watermark) -eq $CanPickImage)) { return }
        Start-Sleep -Milliseconds 50
    } while ($timer.Elapsed.TotalSeconds -lt 15)
    throw "Expected signature status '$StatusText', save $CanSave, watermark $CanPickImage; saw '$status'."
}

function Test-ToolbarResize([IntPtr]$Window, [bool]$Signing) {
    foreach ($width in @(900,650,649,440,700,480,900)) {
        [SumiSmoke]::CheckResize($Window, $width, $Signing)
    }
}

function Test-PfxSigning([string]$ExecutablePath, [string]$InputPdf, [string]$Folder) {
    $name = "SumiPDF PFX " + [guid]::NewGuid()
    $password = "試験 " + [guid]::NewGuid()
    $pfx = Join-Path $Folder '署名用 ID.pfx'
    $out = Join-Path $Folder 'PFX signed.pdf'
    $rsa = [Security.Cryptography.RSA]::Create(2048)
    try {
        $request = [Security.Cryptography.X509Certificates.CertificateRequest]::new(
            "CN=$name", $rsa, [Security.Cryptography.HashAlgorithmName]::SHA256, [Security.Cryptography.RSASignaturePadding]::Pkcs1)
        $cert = $request.CreateSelfSigned([DateTimeOffset]::UtcNow.AddMinutes(-5), [DateTimeOffset]::UtcNow.AddDays(1))
        try {
            $thumbprint = $cert.Thumbprint
            [IO.File]::WriteAllBytes($pfx, $cert.Export([Security.Cryptography.X509Certificates.X509ContentType]::Pfx, $password))
        } finally { $cert.Dispose() }
    } finally { $rsa.Dispose() }
    $start = [Diagnostics.ProcessStartInfo]::new($ExecutablePath)
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = $start.StandardErrorEncoding = [Text.Encoding]::UTF8
    foreach ($arg in @('--sign', $InputPdf, '--out', $out, '--page', '1', '--rect', '0.1,0.4,0.7,0.6', '--pfx', $pfx)) {
        $start.ArgumentList.Add($arg)
    }
    foreach ($attempt in @('cancel', 'wrong', 'correct')) {
        $proc = [Diagnostics.Process]::Start($start)
        try {
            $stdout = $proc.StandardOutput.ReadToEndAsync()
            $stderr = $proc.StandardError.ReadToEndAsync()
            $dialog = Wait-Dialog $proc.Id 'PFX のパスワード'
            switch ($attempt) {
                'cancel' { [SumiSmoke]::CloseDialog($dialog) }
                'wrong' { [SumiSmoke]::EnterTestPassword($dialog, 'wrong') }
                'correct' { [SumiSmoke]::EnterTestPassword($dialog, $password) }
            }
            if (!$proc.WaitForExit(30000)) { throw 'PFX signing timed out.' }
            if ($attempt -eq 'correct') {
                if ($proc.ExitCode -ne 0) { throw "PFX signing failed: $($stderr.Result)" }
                $result = $stdout.Result | ConvertFrom-Json
                if ($result.signer -ne $name -or $result.sha256 -ne (Get-FileHash $out).Hash) { throw 'Wrong PFX signer or digest.' }
            } elseif ($proc.ExitCode -eq 0 -or (Test-Path $out)) { throw "Unexpected output after $attempt." }
        } finally {
            if (!$proc.HasExited) { $proc.Kill(); $proc.WaitForExit() }
            $proc.Dispose()
        }
    }
    if ((Test-Path "Cert:\CurrentUser\My\$thumbprint") -or (Test-Path "Cert:\LocalMachine\My\$thumbprint")) {
        throw 'The PFX test certificate was installed in a personal store.'
    }
    Write-Host 'Verified PFX co-signing with a Unicode password, cancellation, wrong-password handling and no personal-store registration.'
}

$exe = (Resolve-Path -LiteralPath $Executable).Path
$dir = Join-Path ([IO.Path]::GetTempPath()) ("SumiPDF-smoke-" + [Guid]::NewGuid().ToString('N'))
$fixture = Join-Path $dir 'two pages.pdf'
$signedFixture = Join-Path $dir 'signed once.pdf'
$viewer = $null
$pickerCert = $null
try {
    [void][IO.Directory]::CreateDirectory($dir)
    Write-TestPdf $fixture
    & (Join-Path $PSScriptRoot 'signingCli.ps1') -Executable $exe -Pdf $fixture -SignedGuiFixture $signedFixture
    Test-PfxSigning $exe $signedFixture $dir
    Write-Host "Release EXE size $((Get-Item -LiteralPath $exe).Length) bytes."
    # The first observation includes process launch. Both include polling latency.
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $viewer = Start-Process -FilePath $exe -ArgumentList ('"{0}"' -f $fixture) -PassThru
    $window = Wait-Page $viewer 'two pages.pdf' '1 / 2' $timer
    Test-ToolbarResize $window $false
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
    Test-ToolbarResize $window $true
    [SumiSmoke]::Click($window, 132)
    $filePicker = Wait-Dialog $viewer.Id '署名に使う PFX ファイルを開く'
    [SumiSmoke]::CloseDialog($filePicker)
    Wait-SignatureState $window '青枠が選択した署名欄' $true $true
    $pickerName = "SumiPDF Picker " + [guid]::NewGuid()
    $pickerCert = New-SelfSignedCertificate -Type Custom -Subject "CN=$pickerName" -KeyUsage DigitalSignature -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.36') -CertStoreLocation Cert:\CurrentUser\My
    [SumiSmoke]::Click($window, 131)
    $popup = Wait-Dialog $viewer.Id 'Windows 証明書ストアから選ぶ'
    [SumiSmoke]::ChooseCertificate($popup, $pickerName)
    Wait-SignatureState $window "署名者：$pickerName" $true $true
    [SumiSmoke]::Click($window, 131)
    $popup = Wait-Dialog $viewer.Id 'Windows 証明書ストアから選ぶ'
    [SumiSmoke]::CloseDialog($popup)
    Wait-SignatureState $window "署名者：$pickerName" $true $true
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
    if ([SumiSmoke]::IsWindowVisible([SumiSmoke]::GetDlgItem($window, 129)) -or
        [SumiSmoke]::IsWindowVisible([SumiSmoke]::GetDlgItem($window, 131))) {
        throw 'The signature toolbar remained visible after cancellation.'
    }
    [SumiSmoke]::Click($window, 114)
    Wait-HighlightState $viewer $window 'ページ全体' $false
    if (-not $viewer.CloseMainWindow() -or -not $viewer.WaitForExit(5000)) {
        throw 'The viewer did not close normally.'
    }
    if ($viewer.ExitCode -ne 0) { throw "The viewer exited with code $($viewer.ExitCode)." }
    $viewer.Dispose()
    $viewer = $null

    # A PDF with an ordinary approval signature can use the remaining field or a new position.
    $timer.Restart()
    $viewer = Start-Process -FilePath $exe -ArgumentList ('"{0}"' -f $signedFixture) -PassThru
    $window = Wait-Page $viewer 'signed once.pdf' '1 / 2' $timer
    [SumiSmoke]::Click($window, 107)
    Wait-SignatureState $window '青枠が選択した署名欄' $true $true
    if ([SumiSmoke]::ReadText([SumiSmoke]::GetDlgItem($window, 111)) -ne '2 / 2') {
        throw 'Co-signing did not navigate to the remaining Approval field.'
    }
    [SumiSmoke]::Pick($window, 127, 0)
    Wait-SignatureState $window '枠の角から' $false $true
    [SumiSmoke]::Pick($window, 127, 2)
    Wait-SignatureState $window 'ページに表示しない電子署名' $true $false
    [SumiSmoke]::Click($window, 130)
    Wait-HighlightState $viewer $window 'ページ全体' $false
    foreach ($id in @(108, 115)) {
        if ([SumiSmoke]::IsWindowEnabled([SumiSmoke]::GetDlgItem($window, $id))) {
            throw 'Annotation tools were enabled after identifying an existing signature.'
        }
    }
    if (-not $viewer.CloseMainWindow() -or -not $viewer.WaitForExit(5000) -or $viewer.ExitCode -ne 0) {
        throw 'The viewer did not close the signed document normally.'
    }
} finally {
    if ($null -ne $viewer) {
        if (-not $viewer.HasExited) { $viewer.Kill(); $viewer.WaitForExit() }
        $viewer.Dispose()
    }
    if ($null -ne $pickerCert) { Remove-Item -LiteralPath ("Cert:\CurrentUser\My\" + $pickerCert.Thumbprint) -DeleteKey -Force }
    if ([IO.Directory]::Exists($dir)) { [IO.Directory]::Delete($dir, $true) }
}
