Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

public static class ScrollSettings {
    [DllImport("user32.dll", SetLastError = true)]
    static extern bool SystemParametersInfoW(uint action, uint param, out int value, uint settings);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr window);
    public static bool ClientAnimations() {
        int enabled;
        if (!SystemParametersInfoW(0x1042, 0, out enabled, 0))
            throw new Win32Exception(Marshal.GetLastWin32Error());
        return enabled != 0;
    }
}
'@

function Wait-StableBoundary([IntPtr]$Window) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $previous = ''
    $lastChange = 0
    do {
        $stripe = [ReadingProbe]::Viewport($Window)
        if ($stripe -ne $previous) {
            $previous = $stripe
            $lastChange = $clock.ElapsedMilliseconds
        } elseif ($stripe -match 'B{4,}\.+O{4,}' -and $clock.ElapsedMilliseconds - $lastChange -ge 150) {
            return $stripe
        }
        [Threading.Thread]::Sleep(12)
    } while ($clock.ElapsedMilliseconds -lt 3000)
    throw 'The page boundary did not settle before the wheel probe.'
}

function Measure-WheelDetent([IntPtr]$Window, [string]$Before, [int]$Delta, [double]$Expected) {
    # ReadingProbe samples one pixel every four physical rows. The last blue
    # sample is the same page-1 boundary on every frame at this zoom.
    $startEdge = $Before.LastIndexOf('B')
    $positions = [Collections.Generic.List[string]]::new()
    $lastMove = 0
    $intermediate = $false
    $firstMove = $null
    $lastChange = 0
    $samples = 0
    $clock = [Diagnostics.Stopwatch]::StartNew()
    [ReadingProbe]::Wheel($Window, $Delta)
    do {
        $stripe = [ReadingProbe]::Viewport($Window)
        $ms = [int]$clock.ElapsedMilliseconds
        if ($stripe -match 'B{4,}\.+O{4,}') {
            $samples++
            $move = ($stripe.LastIndexOf('B') - $startEdge) * [Math]::Sign($Delta) * 4
            if ($move -ne $lastMove) {
                if ($null -eq $firstMove) { $firstMove = $ms }
                $positions.Add("${ms}ms/${move}px")
                if ($move -lt $lastMove - 4) { throw "Wheel motion reversed at $($positions -join ', ')." }
                if ($move -gt 0 -and $move -lt $Expected - 4) { $intermediate = $true }
                $lastMove = $move
                $lastChange = $ms
            }
            if ([Math]::Abs($move - $Expected) -le 4 -and $ms -ge 160 -and $ms - $lastChange -ge 45) {
                break
            }
        }
        [Threading.Thread]::Sleep(4)
    } while ($clock.ElapsedMilliseconds -lt 3000)
    Write-Host ("Wheel {0}: {1} samples, first motion {2} ms, last change {3} ms, travel {4}/{5} px; frames [{6}]." -f
        $Delta, $samples, $firstMove, $lastChange, $lastMove, $Expected, ($positions -join ', '))
    if ($stripe -notmatch 'B{4,}\.+O{4,}' -or [Math]::Abs($lastMove - $Expected) -gt 4) {
        throw "A full wheel detent moved the painted boundary $lastMove px, expected $Expected px (four-pixel sampling)."
    }
    [pscustomobject]@{ Stripe = $stripe; Intermediate = $intermediate }
}

function Test-ScrollMotion([IntPtr]$Window, [string]$Before) {
    $animations = [ScrollSettings]::ClientAnimations()
    $dpi = [ScrollSettings]::GetDpiForWindow($Window)
    if ($dpi -eq 0) { throw 'Could not measure the viewer DPI for the wheel test.' }
    $expected = [Math]::Floor(72 * $dpi / 96.0 + 0.5)
    $edge = $Before.LastIndexOf('B')
    # Scroll towards the center to keep both page colors in view.
    $delta = if ($edge -ge $Before.Length / 2) { -120 } else { 120 }
    $available = if ($delta -lt 0) { $edge } else { $Before.Length - $edge }
    if (($available - 6) * 4 -lt $expected) { throw 'The viewport cannot fit one wheel detent and both page colors.' }
    Write-Host "Wheel motion probe: Windows client-area animation=$animations, DPI=$dpi."
    $stripe = $Before
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        $out = Measure-WheelDetent $Window $stripe $delta $expected
        $back = Measure-WheelDetent $Window $out.Stripe (-$delta) $expected
        $stripe = $back.Stripe
        if ([Math]::Abs($stripe.LastIndexOf('B') - $edge) -gt 1) {
            throw 'Opposite detents did not restore the reading position.'
        }
        if (!$animations) {
            Write-Host 'Windows client-area animations are disabled; checked travel and reversal.'
            return $stripe
        }
        if ($out.Intermediate -or $back.Intermediate) { return $stripe }
    }
    throw 'No intermediate painted position was observed during six full detents with Windows animations enabled.'
}
