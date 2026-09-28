# windows arm64 — libnode 动态库构建（需要 runner 的 VS ARM64 组件）
$ErrorActionPreference = "Stop"

function prepare() {
  Write-Host "prepare: using bundled MSVC ARM64 toolchain"
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
  New-Item -ItemType "Directory" -Force -Path .\release\libnode-windows-arm64 | Out-Null
  if (Test-Path .\node\out\Release\libnode.dll) {
    Copy-Item .\node\out\Release\libnode.dll .\release\libnode-windows-arm64\
  } elseif (Test-Path .\node\out\Release\node.dll) {
    Copy-Item .\node\out\Release\node.dll .\release\libnode-windows-arm64\libnode.dll
  } else {
    throw "未找到构建产物（libnode.dll / node.dll）"
  }
  if (Test-Path .\node\out\Release\node.exe) {
    Copy-Item .\node\out\Release\node.exe .\release\libnode-windows-arm64\
  }
  Get-ChildItem .\release\libnode-windows-arm64\
}

prepare
clone
build
copy-release
