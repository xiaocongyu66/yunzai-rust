# windows arm64 — 在 windows runner 上构建 libnode 动态库（需要 VS 的 ARM64 组件）
$ErrorActionPreference = "Stop"

function prepare() {
  Get-Command clang
  clang --version
}

function clone() {
  git clone "$env:NODEJS_GIT" --branch "$env:NODEJS_BRANCH" --depth=1 .\node
}

function build() {
  Set-Location .\node
  .\vcbuild.bat arm64 dll openssl-no-asm
  Set-Location ..
}

function copy-release() {
  if (Test-Path .\release\libnode-windows-arm64) {
    Remove-Item -Recurse -Force .\release\libnode-windows-arm64
  }
  New-Item -ItemType "Directory" -Force -Path .\release\libnode-windows-arm64
  Copy-Item -Path .\node\out\Release\libnode.dll -Destination .\release\libnode-windows-arm64 -ErrorAction SilentlyContinue
  Copy-Item -Path .\node\out\Release\node.exe -Destination .\release\libnode-windows-arm64 -ErrorAction SilentlyContinue
}

&$args[0]
