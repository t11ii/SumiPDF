function Write-PdfObjects([string]$Path, [string[]]$Objects) {
    # Every inserted character is ASCII, so StringBuilder lengths are also byte offsets.
    $pdf = [Text.StringBuilder]::new("%PDF-1.4`n")
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

function Write-ReadingPdf([string]$Path, [int]$Count, [int]$ExtraShapes = 0) {
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
        if ($page -in @(1, 2, 37)) {
            $punctuation = if ($page -eq 2) { '!' } else { '?' }
            $drawing += "BT /F1 24 Tf 122 445 Td (Which section$punctuation) Tj ET`n"
        }
        if ($page -eq 1 -and $ExtraShapes -gt 0) {
            $busy = [Text.StringBuilder]::new("0.24 0.30 0.40 rg`n")
            for ($i = 0; $i -lt $ExtraShapes; $i++) {
                $x = 24 + 3 * ($i % 80)
                $y = 70 + 5 * [int][Math]::Floor($i / 80)
                [void]$busy.AppendFormat("{0} {1} 2 2 re f`n", $x, $y)
            }
            $drawing += $busy.ToString()
        }
        $objects.Add("<< /Length $($drawing.Length) >>`nstream`n${drawing}endstream")
    }
    Write-PdfObjects $Path $objects.ToArray()
}

function New-SmokeViewerStartInfo([string]$ExecutablePath, [string]$ProfileDirectory, [string[]]$Documents) {
    [void][IO.Directory]::CreateDirectory($ProfileDirectory)
    $start = [Diagnostics.ProcessStartInfo]::new($ExecutablePath)
    $start.UseShellExecute = $false
    $start.Environment['LOCALAPPDATA'] = $ProfileDirectory
    $roaming = Join-Path $ProfileDirectory 'Roaming'
    [void][IO.Directory]::CreateDirectory($roaming)
    $start.Environment['APPDATA'] = $roaming
    foreach ($document in $Documents) { $start.ArgumentList.Add($document) }
    return $start
}

function Start-SmokeViewer([string]$ExecutablePath, [string]$ProfileDirectory, [string[]]$Documents) {
    [Diagnostics.Process]::Start((New-SmokeViewerStartInfo $ExecutablePath $ProfileDirectory $Documents))
}
