param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$Pdf)
$ErrorActionPreference = 'Stop'
$binary = (Resolve-Path -LiteralPath $Executable).Path
$inputPdf = (Resolve-Path -LiteralPath $Pdf).Path
$folder = Join-Path ([IO.Path]::GetTempPath()) ("SumiPDF CLI " + [guid]::NewGuid())
[void][IO.Directory]::CreateDirectory($folder)

function Invoke-SignCli([string[]]$Arguments, [switch]$ExpectError) {
    $start = [Diagnostics.ProcessStartInfo]::new($binary)
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = [Text.Encoding]::UTF8
    $start.StandardErrorEncoding = [Text.Encoding]::UTF8
    foreach ($arg in $Arguments) { $start.ArgumentList.Add($arg) }
    $proc = [Diagnostics.Process]::Start($start)
    try {
        $stdout = $proc.StandardOutput.ReadToEndAsync()
        $stderr = $proc.StandardError.ReadToEndAsync()
        if (!$proc.WaitForExit(30000)) { $proc.Kill(); throw "Timed out: $($Arguments[0])" }
        if ($ExpectError) {
            if ($proc.ExitCode -eq 0 -or !(($stderr.Result | ConvertFrom-Json).error)) { throw "Expected a JSON error" }
            return
        }
        if ($proc.ExitCode -ne 0) { throw "$($Arguments[0]) failed: $($stderr.Result)" }
        return ($stdout.Result | ConvertFrom-Json)
    } finally { $proc.Dispose() }
}

$cert = $null
try {
    $info = Invoke-SignCli @('--inspect', $inputPdf)
    $field = @($info.fields | Where-Object name -eq 'Approval')
    if ($info.pages -ne 2 -or $field.Count -ne 1 -or $field[0].page -ne 2 -or $info.sha256 -ne (Get-FileHash $inputPdf).Hash) {
        throw 'Unexpected field or source digest'
    }
    $previewPath = Join-Path $folder 'preview.png'
    $preview = Invoke-SignCli @('--preview', $inputPdf, '--field', $field[0].id, '--out', $previewPath)
    if ($preview.sha256 -ne $info.sha256 -or $preview.page -ne 2 -or !(Test-Path $previewPath)) { throw 'Preview mismatch' }
    $subject = "SumiPDF CLI 試験 " + [guid]::NewGuid()
    $cert = New-SelfSignedCertificate -Type Custom -Subject "CN=$subject" -KeyAlgorithm RSA -KeyLength 2048 -KeyUsage DigitalSignature -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.36') -CertStoreLocation Cert:\CurrentUser\My
    $list = Invoke-SignCli @('--certificates')
    if (@($list.certificates | Where-Object thumbprint -eq $cert.Thumbprint).Count -ne 1) { throw 'Certificate missing from list' }

    $logo = Join-Path $folder '透かし.png'
    Add-Type -AssemblyName System.Drawing
    $bmp = [Drawing.Bitmap]::new(16, 16)
    try { $bmp.SetPixel(8, 8, [Drawing.Color]::Red); $bmp.Save($logo, [Drawing.Imaging.ImageFormat]::Png) } finally { $bmp.Dispose() }
    $signed = Join-Path $folder '契約書 署名済.pdf'
    $signArgs = @('--sign',$inputPdf,'--out',$signed,'--field',$field[0].id,'--watermark',$logo,'--certificate',$cert.Thumbprint,'--expect-sha256')
    Invoke-SignCli -Arguments ($signArgs + ('0'*64)) -ExpectError
    if (Test-Path $signed) { throw 'Hash mismatch produced a signed file' }
    $result = Invoke-SignCli ($signArgs + $info.sha256)
    if ($result.signer -ne $subject -or $result.page -ne 2 -or $result.sha256 -ne (Get-FileHash $signed).Hash) { throw 'Signed output mismatch' }
    Invoke-SignCli -Arguments ($signArgs + $info.sha256) -ExpectError
    $area = Join-Path $folder 'free area.pdf'
    $result = Invoke-SignCli @('--sign',$inputPdf,'--page','1','--rect','0.1,0.2,0.7,0.4','--out',$area,'--certificate',$cert.Thumbprint)
    if ($result.page -ne 1 -or $result.sha256 -ne (Get-FileHash $area).Hash) { throw 'Manual placement failed' }
    Write-Host 'CLI verified field discovery, page preview, source digest, Windows certificate signing, custom PNG, free placement and no overwrite.'
} finally {
    if ($null -ne $cert) { Remove-Item -LiteralPath ("Cert:\CurrentUser\My\" + $cert.Thumbprint) -DeleteKey -Force }
    [IO.Directory]::Delete($folder, $true)
}
