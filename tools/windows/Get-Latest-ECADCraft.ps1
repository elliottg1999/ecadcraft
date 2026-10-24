# Get the latest ECADCraft for Windows and start it. GitHub builds it after every change
# (release "ecad-latest" of Elliottg1999/ecadcraft), so nothing is compiled on this PC.
# The app lives in %LOCALAPPDATA%\ECADCraft; only a newer build is downloaded.
$ErrorActionPreference = 'Stop'
$base = 'https://github.com/Elliottg1999/ecadcraft/releases/download/ecad-latest'
$dir = Join-Path $env:LOCALAPPDATA 'ECADCraft'
$exe = Join-Path $dir 'ECADCraft\cadcraft.exe'
$have = Join-Path $dir 'version.txt'
New-Item -ItemType Directory -Force -Path $dir | Out-Null
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

try {
    $latest = (Invoke-WebRequest -UseBasicParsing "$base/version.txt").Content.Trim()
} catch {
    Write-Host "Could not reach GitHub ($($_.Exception.Message))." -ForegroundColor Yellow
    $latest = $null
}
$current = if (Test-Path $have) { (Get-Content $have -Raw).Trim() } else { '' }

if ($latest -and $latest -ne $current) {
    if (Get-Process cadcraft -ErrorAction SilentlyContinue) {
        Write-Host "A new ECADCraft ($latest) is ready, but ECADCraft is open. Save your work, close it, and run this again." -ForegroundColor Yellow
        Read-Host 'Press Enter to close'
        exit 1
    }
    Write-Host "Downloading ECADCraft $latest ..." -ForegroundColor Cyan
    $zip = Join-Path $dir 'ecadcraft-windows.zip'
    Invoke-WebRequest -UseBasicParsing "$base/ecadcraft-windows.zip" -OutFile $zip
    if (Test-Path (Join-Path $dir 'ECADCraft')) { Remove-Item -Recurse -Force (Join-Path $dir 'ECADCraft') }
    Expand-Archive -Force $zip $dir
    Get-ChildItem -Recurse (Join-Path $dir 'ECADCraft') | Unblock-File
    Remove-Item $zip
    Set-Content $have $latest
    Write-Host "Updated to $latest." -ForegroundColor Green
} elseif ($latest) {
    Write-Host "ECADCraft is up to date ($current)."
}

if (-not (Test-Path $exe)) {
    Write-Host 'ECADCraft is not downloaded yet and GitHub could not be reached.' -ForegroundColor Red
    Read-Host 'Press Enter to close'
    exit 1
}
$info = Join-Path $dir 'ECADCraft\version.txt'
if (Test-Path $info) { Write-Host ("Build: " + (Get-Content $info -Raw).Trim()) }
Start-Process $exe
