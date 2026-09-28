# windows amd64 — libnode 动态库构建
$ErrorActionPreference = "Stop"

function prepare() {
  # GitHub runner 自带 VS 构建链（MSVC），无需额外安装
  Write-Host "prepare: using bundled MSVC toolchain"
}

function clone() {
  git clone "$env:NODEJS_GIT" --branch "$env:NODEJS_BRANCH" --depth=1 .\node
}

function build() {
  Set-Location .\node
  .\vcbuild.bat x64 dll openssl-no-asm
  Set-Location ..
}

function copy-release() {
  if (Test-Path .\release\libnode-windows-amd64) {
    Remove-Item -Recurse -Force .\release\libnode-windows-amd64
  }
  New-Item -ItemType "Directory" -Force -Path .\release\libnode-windows-amd64 | Out-Null
  # Node Windows shared 构建产物名为 node.dll，统一改为 libnode.dll
  if (Test-Path .\node\out\Release\libnode.dll) {
    Copy-Item .\node\out\Release\libnode.dll .\release\libnode-windows-amd64\
  } elseif (Test-Path .\node\out\Release\node.dll) {
    Copy-Item .\node\out\Release\node.dll .\release\libnode-windows-amd64\libnode.dll
  } else {
    throw "未找到构建产物（libnode.dll / node.dll）"
  }
  if (Test-Path .\node\out\Release\node.exe) {
    Copy-Item .\node\out\Release\node.exe .\release\libnode-windows-amd64\
  }
  Get-ChildItem .\release\libnode-windows-amd64\
}

prepare
clone
build
copy-release
