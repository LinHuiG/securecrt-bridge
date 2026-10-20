param(
  [string]$NsisPath=$env:SCB_NSIS_PATH,
  [string]$Payload=(Join-Path $PSScriptRoot 'target/release/securecrt-bridge.exe'),
  [string]$OutputDirectory=(Join-Path $PSScriptRoot 'dist')
)
$ErrorActionPreference='Stop'
if(!$NsisPath) { $NsisPath=(Get-Command makensis.exe -ErrorAction SilentlyContinue).Source }
if(!$NsisPath) {
  $scbInstalledNsis=Join-Path ${env:ProgramFiles(x86)} 'NSIS/makensis.exe'
  if(Test-Path -LiteralPath $scbInstalledNsis){ $NsisPath=$scbInstalledNsis }
}
if(!$NsisPath -or !(Test-Path -LiteralPath $NsisPath)) { throw 'Provide -NsisPath pointing to an existing NSIS 3 makensis.exe (the same compiler used by shishi).' }
if(!(Test-Path -LiteralPath $Payload)) { throw 'Build the release payload first with build.ps1 release.' }
$version=((Get-Content -LiteralPath (Join-Path $PSScriptRoot 'Cargo.toml') -Raw) | Select-String 'version = "([^"]+)"').Matches[0].Groups[1].Value
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
& $NsisPath /V3 /INPUTCHARSET UTF8 "/DVERSION=$version" "/DPAYLOAD=$Payload" "/DOUTPUT=$OutputDirectory" (Join-Path $PSScriptRoot 'installer/setup.nsi')
if($LASTEXITCODE -ne 0){throw 'NSIS packaging failed'}
$scbSetup=Join-Path $OutputDirectory "SecureCRTBridge-$version-Setup.exe"
Get-FileHash -LiteralPath $scbSetup -Algorithm SHA256 | Format-List | Out-String | Set-Content -LiteralPath (Join-Path $OutputDirectory "SHA256-$version.txt")
Get-Item -LiteralPath $scbSetup | Select-Object FullName,Length
