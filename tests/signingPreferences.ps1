function Start-SmokeViewer([string]$ExecutablePath, [string]$ProfileDirectory, [string[]]$Documents) {
    [void][IO.Directory]::CreateDirectory($ProfileDirectory)
    $start = [Diagnostics.ProcessStartInfo]::new($ExecutablePath)
    $start.UseShellExecute = $false
    $start.Environment['LOCALAPPDATA'] = $ProfileDirectory
    $roaming = Join-Path $ProfileDirectory 'Roaming'
    [void][IO.Directory]::CreateDirectory($roaming)
    $start.Environment['APPDATA'] = $roaming
    foreach ($document in $Documents) { $start.ArgumentList.Add($document) }
    [Diagnostics.Process]::Start($start)
}

function Select-TestPfx([Diagnostics.Process]$Viewer, [IntPtr]$Window, $Identity, [bool]$Remember) {
    [SumiSmoke]::Click($Window, 132)
    $filePicker = Wait-Dialog $Viewer.Id 'Open a PFX file for signing'
    [SumiSmoke]::SetFileName($filePicker, $Identity.Path)
    $prompt = Wait-Dialog $Viewer.Id 'PFX password'
    [SumiSmoke]::PasswordPreference($prompt, $Remember)
    [SumiSmoke]::EnterTestPassword($prompt, $Identity.Password)
    Wait-SignatureState $Window "Signer: $($Identity.Name)" $true $true
}

function Wait-WatermarkButton([IntPtr]$Window, [bool]$Custom) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $label = if ($Custom) { 'Choose watermark (custom image)' } else { 'Choose watermark (default)' }
    do {
        if ([SumiSmoke]::IsWindowEnabled($Window) -and [ReadingProbe]::ControlName($Window, 128) -eq $label) { return }
        Start-Sleep -Milliseconds 40
    } while ($timer.ElapsedMilliseconds -lt 10000)
    throw 'The watermark menu did not save the expected choice.'
}

function Assert-GuiSignature([string]$OutputPdf, [string]$Thumbprint, [string]$Watermark) {
    # This fixture has one signature and its only image is the optional PNG.
    # Verify the actual CMS signature and its signer using the PDF's signed byte ranges.
    $bytes = [IO.File]::ReadAllBytes($OutputPdf)
    $pdf = [Text.Encoding]::Latin1.GetString($bytes)
    $image = [regex]::Match($pdf, '/Watermark\s+(\d+)\s+(\d+)\s+R')
    if ($image.Success -ne ($Watermark -ne 'builtin')) {
        throw "Unexpected signature watermark in $OutputPdf."
    }
    if ($image.Success) {
        $objectPattern = "(?ms)^$($image.Groups[1].Value) $($image.Groups[2].Value) obj\s*<<(.*?)>>\s*stream"
        $dict = [regex]::Match($pdf, $objectPattern).Groups[1].Value
        $width = [int][regex]::Match($dict, '/Width\s+(\d+)').Groups[1].Value
        $height = [int][regex]::Match($dict, '/Height\s+(\d+)').Groups[1].Value
        if (($Watermark -eq 'png' -and ($width -ne 17 -or $height -ne 9)) -or
            ($Watermark -eq 'pdf' -and ($height -le 9 -or $width -ne 2 * $height))) {
            throw "Wrong image for the selected $Watermark watermark ($width x $height)."
        }
    }
    $range = [regex]::Match($pdf, '/ByteRange\s*\[\s*0\s+(\d+)\s+(\d+)\s+(\d+)\s*\]')
    if (!$range.Success) { throw 'No signature byte range in the GUI output.' }
    $prefix = [int]$range.Groups[1].Value
    $suffix = [int]$range.Groups[2].Value
    $tail = [int]$range.Groups[3].Value
    if ($prefix -ge $suffix -or $suffix + $tail -ne $bytes.Length -or $pdf[$prefix] -ne '<' -or $pdf[$suffix - 1] -ne '>') {
        throw 'The signature byte range does not cover the GUI output.'
    }
    $signed = [byte[]]::new($prefix + $tail)
    [Array]::Copy($bytes, $signed, $prefix)
    [Array]::Copy($bytes, $suffix, $signed, $prefix, $tail)
    $cms = [Security.Cryptography.Pkcs.SignedCms]::new([Security.Cryptography.Pkcs.ContentInfo]::new($signed), $true)
    $padded = [Convert]::FromHexString($pdf.Substring($prefix + 1, $suffix - $prefix - 2))
    $reader = [System.Formats.Asn1.AsnReader]::new($padded, [System.Formats.Asn1.AsnEncodingRules]::BER)
    $cms.Decode($reader.ReadEncodedValue().ToArray())
    $cms.CheckSignature($true)
    if ($cms.SignerInfos.Count -ne 1 -or $cms.SignerInfos[0].Certificate.Thumbprint -ne $Thumbprint) {
        throw "Unexpected PFX signer in $OutputPdf."
    }
}

function Test-SigningPreferences([string]$ExecutablePath, [string]$InputPdf, [string]$Folder, $Identity) {
    Add-Type -AssemblyName System.Security.Cryptography.Pkcs
    Add-Type -AssemblyName System.Drawing
    $profileDir = Join-Path $Folder 'PFX profile'
    $acrobatDirectory = Join-Path $profileDir 'Roaming/Adobe/Acrobat/DC/Security'
    [void][IO.Directory]::CreateDirectory($acrobatDirectory)
    $acrobatLogo = Join-Path $acrobatDirectory 'SignatureLogo.pdf'
    $drawing = "0 0.5 0.9 rg 10 10 90 30 re f`n"
    Write-PdfObjects $acrobatLogo @(
        '<< /Type /Catalog /Pages 2 0 R >>'
        '<< /Type /Pages /Count 1 /Kids [3 0 R] >>'
        '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 120 60] /Resources << >> /Contents 4 0 R >>'
        "<< /Length $($drawing.Length) >>`nstream`n${drawing}endstream"
    )
    $logo = Join-Path $Folder '背景 logo.png'
    $bitmap = [Drawing.Bitmap]::new(17, 9)
    try {
        $bitmap.SetPixel(5, 4, [Drawing.Color]::Red)
        $bitmap.Save($logo, [Drawing.Imaging.ImageFormat]::Png)
    } finally { $bitmap.Dispose() }

    foreach ($run in 1..4) {
        # Exercise the filename proposed by the viewer in the real Save dialog.
        # Give each launch its own input so the proposed output is always new.
        $runPdf = Join-Path $Folder "Preference test $run.pdf"
        Copy-Item -LiteralPath $InputPdf -Destination $runPdf
        $viewer = Start-SmokeViewer $ExecutablePath $profileDir @($runPdf)
        try {
            $window = Wait-Page $viewer ([IO.Path]::GetFileName($runPdf)) '1 / 2' ([Diagnostics.Stopwatch]::StartNew()) -Quiet
            [SumiSmoke]::Click($window, 107)
            Wait-SignatureState $window 'The blue box shows the selected signature field' $true $true
            if ($run -eq 1) {
                Select-TestPfx $viewer $window $Identity $true
            } else {
                Wait-SignatureState $window "Signer: $($Identity.Name)" $true $true
            }
            Wait-WatermarkButton $window ($run -lt 4)
            [SumiSmoke]::Click($window, 129)
            if ($run -eq 4) {
                $prompt = Wait-Dialog $viewer.Id 'PFX password'
                [SumiSmoke]::PasswordPreference($prompt, $false)
                [SumiSmoke]::EnterTestPassword($prompt, $Identity.Password)
            }
            # After opting in, the saved PFX must reach Save without another password prompt.
            $dialog = Wait-Dialog $viewer.Id 'Save a signed copy'
            $out = Join-Path $Folder "Preference test $run-signed.pdf"
            [SumiSmoke]::AcceptFileDialog($dialog)
            [SumiSmoke]::DismissSaveConfirmation($viewer.Id, $window)
            $expectedWatermark = @('pdf', 'png', 'pdf', 'builtin')[$run - 1]
            Assert-GuiSignature $out $Identity.Thumbprint $expectedWatermark
            if ($run -lt 4) {
                [SumiSmoke]::Click($window, 107)
                Wait-SignatureState $window "Signer: $($Identity.Name)" $true $true
                if ($run -eq 3) {
                    [SumiSmoke]::PickWatermark($window, 0)
                    Wait-WatermarkButton $window $false
                    Select-TestPfx $viewer $window $Identity $false
                } else {
                    [SumiSmoke]::PickWatermark($window, $run)
                    $title = if ($run -eq 1) { 'Choose a PNG for the signature background' } else { 'Choose a single-page Acrobat watermark PDF' }
                    $path = if ($run -eq 1) { $logo } else { $acrobatLogo }
                    [SumiSmoke]::SetFileName((Wait-Dialog $viewer.Id $title), $path)
                    Wait-SignatureState $window "Signer: $($Identity.Name)" $true $true
                }
            }
            if (!$viewer.CloseMainWindow() -or !$viewer.WaitForExit(5000) -or $viewer.ExitCode -ne 0) {
                throw "Signing preference viewer $run did not close normally."
            }
        } finally {
            if (!$viewer.HasExited) { $viewer.Kill(); $viewer.WaitForExit() }
            $viewer.Dispose()
        }
    }
    Write-Host 'Verified the PFX signer, opt-in password reuse and subsequent opt-out across four GUI launches. Signed PDFs confirmed Acrobat watermark discovery, restored PNG and PDF choices, and the persistent built-in override.'
}
