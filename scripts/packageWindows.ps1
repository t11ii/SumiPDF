[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $Executable,
    [string] $OutputDirectory = 'target/distribution'
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    $exe = (Resolve-Path -LiteralPath $Executable).Path
    $mupdf = (Resolve-Path -LiteralPath $env:SUMIPDF_MUPDF_DIR).Path
    $archiveName = 'mupdf-1.28.7-source.tar.gz'
    $archive = Join-Path $root "vendor/$archiveName"
    if (!(Test-Path -LiteralPath $archive)) {
        $archive = Join-Path (Split-Path -Parent $mupdf) $archiveName
    }
    $out = (New-Item -ItemType Directory -Force $OutputDirectory).FullName
    $stage = New-Item -ItemType Directory (Join-Path $env:TEMP ("SumiPDF-" + [Guid]::NewGuid()))
    try {
        $source = (New-Item -ItemType Directory (Join-Path $stage.FullName 'source')).FullName
        $binary = (New-Item -ItemType Directory (Join-Path $stage.FullName 'binary')).FullName
        $gitArchive = Join-Path $stage.FullName 'repository.tar'
        git archive --format=tar -o $gitArchive HEAD
        if ($LASTEXITCODE) { throw 'Could not archive the SumiPDF source.' }
        tar.exe -xf $gitArchive -C $source
        if ($LASTEXITCODE) { throw 'Could not stage the SumiPDF source.' }
        Remove-Item -LiteralPath (Join-Path $source 'dist/SumiPDF.exe') -ErrorAction SilentlyContinue
        $vendor = (New-Item -ItemType Directory (Join-Path $source 'vendor')).FullName
        Copy-Item -LiteralPath $archive -Destination $vendor
        Push-Location $source
        try {
            $config = cargo vendor --locked --versioned-dirs vendor/rust
            if ($LASTEXITCODE) { throw 'Could not vendor the Rust dependencies.' }
            Add-Content -LiteralPath '.cargo/config.toml' -Value ('' + [Environment]::NewLine)
            Add-Content -LiteralPath '.cargo/config.toml' -Value $config
            cargo metadata --offline --locked --format-version 1 | Out-Null
            if ($LASTEXITCODE) { throw 'The source package cannot resolve the locked dependencies offline.' }
        } finally { Pop-Location }

        Copy-Item -LiteralPath $exe -Destination (Join-Path $binary 'SumiPDF.exe')
        Copy-Item -LiteralPath @('dist/watermarkDesigner.html', 'dist/signingGuide.md',
            'LICENSE', 'THIRD-PARTY-NOTICES.md') -Destination $binary

        $licenses = (New-Item -ItemType Directory (Join-Path $binary 'licenses')).FullName
        $muFiles = @(
            'COPYING', 'thirdparty/brotli/LICENSE', 'thirdparty/cmark-gfm/COPYING',
            'thirdparty/freetype/LICENSE.TXT', 'thirdparty/freetype/docs/FTL.TXT',
            'thirdparty/freetype/docs/GPLv2.TXT', 'thirdparty/gumbo-parser/COPYING',
            'thirdparty/harfbuzz/COPYING', 'thirdparty/jbig2dec/LICENSE', 'thirdparty/jbig2dec/COPYING',
            'thirdparty/libjpeg/README', 'thirdparty/lcms2/LICENSE', 'thirdparty/openjpeg/LICENSE',
            'thirdparty/zlib/LICENSE', 'resources/fonts/noto/COPYING',
            'resources/fonts/droid/NOTICE', 'resources/fonts/urw/OFL.txt', 'resources/fonts/sil/OFL.txt'
        )
        foreach ($relative in $muFiles) {
            $target = Join-Path $licenses "mupdf/$relative"
            New-Item -ItemType Directory -Force (Split-Path -Parent $target) | Out-Null
            Copy-Item -LiteralPath (Join-Path $mupdf $relative) -Destination $target
        }
        Get-ChildItem -LiteralPath (Join-Path $vendor 'rust') -Directory | ForEach-Object {
            $crate = $_.Name
            Get-ChildItem -LiteralPath $_.FullName -File |
                Where-Object { $_.Name -match '^(licen[cs]e|copying|notice)([._-].*)?$' } |
                ForEach-Object {
                    $folder = Join-Path $licenses "rust/$crate"
                    New-Item -ItemType Directory -Force $folder | Out-Null
                    Copy-Item -LiteralPath $_.FullName -Destination $folder
                }
        }

        foreach ($package in @(@('SumiPDFWindowsX64.zip', $binary), @('SumiPDFSource.zip', $source))) {
            $zip = Join-Path $out $package[0]
            tar.exe -a -cf $zip -C $package[1] .
            if ($LASTEXITCODE) { throw "Could not write $zip." }
        }
    } finally {
        Remove-Item -LiteralPath $stage.FullName -Recurse -Force
    }
} finally {
    Pop-Location
}
