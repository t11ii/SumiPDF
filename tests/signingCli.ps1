param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$Pdf, [string]$SignedGuiFixture)
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

function New-TestSigner([string]$Subject, [string]$Store = 'CurrentUser\My') {
    New-SelfSignedCertificate -Type Custom -Subject "CN=$Subject" -KeyAlgorithm RSA -KeyLength 2048 -KeyUsage DigitalSignature -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.36') -CertStoreLocation "Cert:\$Store"
}

$cert = $null
$reviewer = $null
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
$elevated = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$reviewStore = if ($elevated) { 'LocalMachine\My' } else { 'CurrentUser\My' }
try {
    if ($env:GITHUB_ACTIONS -eq 'true' -and !$elevated) { throw 'Windows CI requires a machine-store signer.' }
    $info = Invoke-SignCli @('--inspect', $inputPdf)
    $field = @($info.fields | Where-Object name -eq 'Approval')
    if ($info.pages -ne 2 -or $field.Count -ne 1 -or $field[0].page -ne 2 -or $info.sha256 -ne (Get-FileHash $inputPdf).Hash) {
        throw 'Unexpected field or source digest'
    }
    $previewPath = Join-Path $folder 'preview.png'
    $preview = Invoke-SignCli @('--preview', $inputPdf, '--field', $field[0].id, '--out', $previewPath)
    if ($preview.sha256 -ne $info.sha256 -or $preview.page -ne 2 -or !(Test-Path $previewPath)) { throw 'Preview mismatch' }
    $subject = "SumiPDF CLI 試験 " + [guid]::NewGuid()
    $cert = New-TestSigner $subject
    $reviewerName = "SumiPDF 承認 " + [guid]::NewGuid()
    $reviewer = New-TestSigner $reviewerName $reviewStore
    $list = Invoke-SignCli @('--certificates')
    $firstIdentity = @($list.certificates | Where-Object thumbprint -eq $cert.Thumbprint)
    $secondIdentity = @($list.certificates | Where-Object thumbprint -eq $reviewer.Thumbprint)
    if ($firstIdentity.Count -ne 1 -or $firstIdentity[0].store -ne 'CurrentUser\My' -or
        $secondIdentity.Count -ne 1 -or $secondIdentity[0].store -ne $reviewStore) {
        throw "Expected distinct identities in CurrentUser\My and $reviewStore."
    }

    $logo = Join-Path $folder '透かし.png'
    Add-Type -AssemblyName System.Drawing
    $bmp = [Drawing.Bitmap]::new(16, 16)
    try { $bmp.SetPixel(8, 8, [Drawing.Color]::Red); $bmp.Save($logo, [Drawing.Imaging.ImageFormat]::Png) } finally { $bmp.Dispose() }
    $area = Join-Path $folder 'free area.pdf'
    $first = Invoke-SignCli @('--sign',$inputPdf,'--page','1','--rect','0.1,0.2,0.7,0.4','--out',$area,'--certificate',$cert.Thumbprint)
    if ($first.signer -ne $subject -or $first.page -ne 1 -or $first.sha256 -ne (Get-FileHash $area).Hash) { throw 'Manual placement failed' }
    $secondInfo = Invoke-SignCli @('--inspect', $area)
    if ($secondInfo.sha256 -ne $first.sha256 -or @($secondInfo.fields).Count -ne 1 -or $secondInfo.fields[0].id -ne $field[0].id) {
        throw 'The original empty field is unavailable for the second signer'
    }
    $signed = Join-Path $folder '契約書 署名済.pdf'
    $signArgs = @('--sign',$area,'--out',$signed,'--field',$field[0].id,'--watermark',$logo,'--certificate',$reviewer.Thumbprint,'--expect-sha256')
    Invoke-SignCli -Arguments ($signArgs + ('0'*64)) -ExpectError
    if (Test-Path $signed) { throw 'Hash mismatch produced a signed file' }
    $result = Invoke-SignCli ($signArgs + $first.sha256)
    if ($result.signer -ne $reviewerName -or $result.page -ne 2 -or $result.sha256 -ne (Get-FileHash $signed).Hash) { throw 'Second signature output mismatch' }
    $signedPng = Join-Path $folder 'signed-page.png'
    $preview = Invoke-SignCli @('--preview',$signed,'--page','2','--out',$signedPng)
    $pageImage = [Drawing.Bitmap]::new($signedPng)
    try {
        if ($preview.sha256 -ne $result.sha256 -or $preview.width -ne $pageImage.Width -or $preview.height -ne $pageImage.Height) {
            throw 'Signed page preview does not match its source or output dimensions'
        }
    } finally { $pageImage.Dispose() }
    $prefix = [IO.MemoryStream]::new([IO.File]::ReadAllBytes($signed), 0, [int](Get-Item $area).Length, $false)
    try {
        if ((Get-FileHash -InputStream $prefix).Hash -ne $first.sha256) { throw 'Second signer rewrote the first revision' }
    } finally { $prefix.Dispose() }
    $complete = Invoke-SignCli @('--inspect',$signed)
    if (@($complete.fields).Count -ne 0) { throw 'An occupied field is still writable' }
    Invoke-SignCli -Arguments ($signArgs + $first.sha256) -ExpectError
    if ((Get-FileHash $signed).Hash -ne $result.sha256 -or (Get-FileHash $area).Hash -ne $first.sha256 -or (Get-FileHash $inputPdf).Hash -ne $info.sha256) {
        throw 'An existing PDF was overwritten'
    }
    if ($SignedGuiFixture) { [IO.File]::Copy($area, $SignedGuiFixture) }
    Write-Host "Signed first from CurrentUser\My and then from $reviewStore."
    Write-Host 'CLI verified two different Windows signers, both placements, field discovery, preview, exact revision preservation, source digest, custom PNG and no overwrite.'
} finally {
    if ($null -ne $reviewer) { Remove-Item -LiteralPath ("Cert:\$reviewStore\" + $reviewer.Thumbprint) -DeleteKey -Force }
    if ($null -ne $cert) { Remove-Item -LiteralPath ("Cert:\CurrentUser\My\" + $cert.Thumbprint) -DeleteKey -Force }
    [IO.Directory]::Delete($folder, $true)
}
