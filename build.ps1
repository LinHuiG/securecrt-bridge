param(
  [ValidateSet('test','check','release')][string]$Mode='test',
  [switch]$Offline,
  [string]$ToolchainRoot=$env:SCB_TOOLCHAIN_ROOT,
  [string]$BinutilsDirectory=$env:SCB_BINUTILS_DIR
)
$ErrorActionPreference='Stop'
if(Test-Path (Join-Path $PSScriptRoot '.build/cargo-home')) {
  $env:CARGO_HOME=Join-Path $PSScriptRoot '.build/cargo-home'
}
$cargo='cargo'
if($ToolchainRoot) {
  $cargo=Join-Path $ToolchainRoot 'bin/cargo.exe'
  $env:RUSTC=Join-Path $ToolchainRoot 'bin/rustc.exe'
  $env:PATH=(Join-Path $ToolchainRoot 'bin')+';'+$env:PATH
  $gnu=Join-Path $ToolchainRoot 'lib/rustlib/x86_64-pc-windows-gnu/bin/self-contained'
  if(Test-Path (Join-Path $gnu 'x86_64-w64-mingw32-gcc.exe')) {
    $env:CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=Join-Path $gnu 'x86_64-w64-mingw32-gcc.exe'
  }
}
if($BinutilsDirectory) {
  $env:PATH=$BinutilsDirectory+';'+$env:PATH
  $env:SCB_WINDRES=Join-Path $BinutilsDirectory 'windres.exe'
  $dlltool=Join-Path $BinutilsDirectory 'dlltool.exe'
  $env:CARGO_ENCODED_RUSTFLAGS=('-C','target-feature=+crt-static','-C',"dlltool=$dlltool") -join [char]31
}
Push-Location $PSScriptRoot
try {
  $common=@('--locked')
  if($Offline){$common+='--offline'}
  if($Mode -eq 'release') {
    & $cargo build @common --release -p scb-engine
    if($LASTEXITCODE -ne 0){throw 'Engine build failed'}
    $targetDir=if($env:CARGO_TARGET_DIR){$env:CARGO_TARGET_DIR}else{Join-Path $PSScriptRoot 'target'}
    $env:SCB_ENGINE_DLL=Join-Path $targetDir 'release/scb_engine.dll'
    & $cargo build @common --release -p securecrt-bridge
  } elseif($Mode -eq 'check') { & $cargo check @common --workspace }
  else { & $cargo test @common --workspace --lib --bins }
  if($LASTEXITCODE -ne 0){throw 'Cargo failed'}
} finally { Pop-Location }
