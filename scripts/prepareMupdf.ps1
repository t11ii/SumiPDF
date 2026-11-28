[CmdletBinding()]
param([string] $SourceArchive)

$ErrorActionPreference = 'Stop'
$version = '1.28.7'
$archiveName = "mupdf-$version-source.tar.gz"
$sha256 = '4052527c5064dc15033f8c2d842665afd409be520c9f52945ef703e5dec9d3ea'
$root = Split-Path -Parent $PSScriptRoot
$cache = Join-Path $root 'target/mupdf'
$source = Join-Path $cache "mupdf-$version-source"
$library = Join-Path $source 'platform/win32/x64/Release/libmupdf.lib'

New-Item -ItemType Directory -Force $cache | Out-Null
if (!$SourceArchive) {
    $SourceArchive = Join-Path $root "vendor/$archiveName"
    if (!(Test-Path -LiteralPath $SourceArchive)) {
        $SourceArchive = Join-Path $cache $archiveName
        if (!(Test-Path -LiteralPath $SourceArchive)) {
            $download = "$SourceArchive.download"
            Invoke-WebRequest -Uri "https://mupdf.com/downloads/archive/$archiveName" -OutFile $download
            if ((Get-FileHash -Algorithm SHA256 $download).Hash -ne $sha256) {
                throw "MuPDF $version download failed SHA-256 verification."
            }
            Move-Item -LiteralPath $download -Destination $SourceArchive -Force
        }
    }
}
if ((Get-FileHash -Algorithm SHA256 -LiteralPath $SourceArchive).Hash -ne $sha256) {
    throw "MuPDF $version source archive failed SHA-256 verification."
}
if (!(Test-Path -LiteralPath $library)) {
    tar.exe -xf $SourceArchive -C $cache
    if ($LASTEXITCODE) { throw 'Could not extract the MuPDF source archive.' }

    # Match the static CRT used by the Rust application and ship one executable.
    Get-ChildItem -LiteralPath (Join-Path $source 'platform/win32') -Filter '*.vcxproj' |
        ForEach-Object {
            $text = [IO.File]::ReadAllText($_.FullName)
            $updated = $text.Replace('<RuntimeLibrary>MultiThreadedDLL</RuntimeLibrary>',
                '<RuntimeLibrary>MultiThreaded</RuntimeLibrary>')
            if ($_.Name -eq 'libmupdf.vcxproj') {
                $updated = [regex]::Replace($updated,
                    '(?m)^    <ProjectReference Include="lib(?:tesseract|mubarcode|extract)\.vcxproj">[\s\S]*?^    </ProjectReference>\r?\n', '')
            }
            if ($updated -ne $text) { [IO.File]::WriteAllText($_.FullName, $updated) }
        }

    $msbuild = Get-Command msbuild.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source
    if (!$msbuild) {
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
        $msbuild = & $vswhere -latest -products '*' -requires Microsoft.Component.MSBuild -find 'MSBuild/**/Bin/MSBuild.exe' |
            Select-Object -First 1
    }
    if (!$msbuild) { throw 'Install Visual Studio Build Tools with the Desktop development with C++ workload.' }

    $previous = $env:CL
    try {
        # Keep the PDF renderer and fonts. Disable document JavaScript and unused OCR/output features.
        $env:CL = '/MP /DFZ_ENABLE_JS#0 /DOCR_DISABLED /DFZ_ENABLE_OCR_OUTPUT#0 /DFZ_ENABLE_DOCX_OUTPUT#0 /DFZ_ENABLE_BARCODE#0 /DTOFU_CJK_LANG#1'
        $solution = Join-Path $source 'platform/win32/mupdf.sln'
        & $msbuild $solution /t:libmupdf /p:Configuration=Release /p:Platform=x64 /p:PlatformToolset=v143 /m /nologo /v:minimal |
            Write-Host
        if ($LASTEXITCODE -or !(Test-Path -LiteralPath $library)) { throw 'MuPDF build failed.' }
    } finally {
        $env:CL = $previous
    }
}

$env:SUMIPDF_MUPDF_DIR = $source
$env:SUMIPDF_MUPDF_SOURCE_ARCHIVE = (Resolve-Path -LiteralPath $SourceArchive).Path
if ($env:GITHUB_ENV) {
    Add-Content -LiteralPath $env:GITHUB_ENV -Value "SUMIPDF_MUPDF_DIR=$source" -Encoding utf8
    Add-Content -LiteralPath $env:GITHUB_ENV -Value "SUMIPDF_MUPDF_SOURCE_ARCHIVE=$env:SUMIPDF_MUPDF_SOURCE_ARCHIVE" -Encoding utf8
}
Write-Host "MuPDF $version ready at $source"
