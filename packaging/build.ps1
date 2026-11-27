param([switch]$PrepareOnly)
$ErrorActionPreference='Stop'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Push-Location $projectRoot
try {
    $toolDir = Join-Path $projectRoot 'target\installer-tools'
    New-Item -ItemType Directory -Force $toolDir,dist | Out-Null
    $zip = Join-Path $toolDir 'nsis-3.11.zip'
    if (-not (Test-Path $zip)) {
        Invoke-WebRequest -UseBasicParsing 'https://github.com/tauri-apps/binary-releases/releases/download/nsis-3.11/nsis-3.11.zip' -OutFile $zip
    }
    if ((Get-FileHash $zip).Hash -ne 'c7d27f780ddb6cffb4730138cd1591e841f4b7edb155856901cdf5f214394fa1') {throw 'NSIS checksum mismatch'}
    $compiler = Get-ChildItem $toolDir -Recurse -Filter makensis.exe | Select-Object -First 1
    if (-not $compiler) {
        Expand-Archive -LiteralPath $zip -DestinationPath $toolDir -Force
        $compiler = Get-ChildItem $toolDir -Recurse -Filter makensis.exe | Select-Object -First 1
    }
    $runtime = Join-Path $toolDir 'MicrosoftEdgeWebview2Setup.exe'
    if (-not (Test-Path $runtime)) {
        Invoke-WebRequest -UseBasicParsing 'https://go.microsoft.com/fwlink/p/?LinkId=2124703' -OutFile $runtime
    }
    $signature = Get-AuthenticodeSignature $runtime
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') {throw 'Invalid Microsoft WebView2 bootstrapper signature'}
    if ($PrepareOnly) {return}
    cargo build --locked --release --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) {throw 'Rust release build failed'}
    & $compiler.FullName /V2 packaging\setup.nsi
    if ($LASTEXITCODE -ne 0) {throw 'NSIS failed'}
    $artifact=Get-Item 'dist\Quota-1.1.6-windows-x64-setup.exe'
    ((Get-FileHash $artifact.FullName).Hash.ToLower()+'  '+$artifact.Name) | Set-Content -Encoding ascii "$($artifact.FullName).sha256"
} finally {Pop-Location}

