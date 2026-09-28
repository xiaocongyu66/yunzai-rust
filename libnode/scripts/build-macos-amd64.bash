#!/usr/bin/env bash
set -e 

os="macos"
arch="arm64"

function prepare() {
  sudo chown -R $(whoami) $(brew --prefix)/*

  # homebrew fails to update python 3.9.1 to 3.9.1.1 due to unlinking failure
  sudo rm -f /usr/local/bin/2to3 || true
  # homebrew fails to update python from 3.9 to 3.10 due to another unlinking failure
  sudo rm -f /usr/local/bin/idle3 || true
  sudo rm -f /usr/local/bin/pydoc3 || true
  sudo rm -f /usr/local/bin/python3 || true
  sudo rm -f /usr/local/bin/python3-config || true

  brew install git node ninja nasm ccache
}

function clone() {
  if [ "$NODEJS_GIT" = "" ]; then
    echo "Missing \$NODEJS_GIT"
    exit 1
  fi

  if [ "$NODEJS_BRANCH" = "" ]; then
    echo "Missing \$NODEJS_BRANCH"
    exit 1
  fi

  git clone "$NODEJS_GIT" --branch "$NODEJS_BRANCH" --depth=1 ./node

  # ARM mac 交叉编 x64：zlib 的 cpuid 内联 asm 无法交叉生成——SIMD 统一降级纯 C 实现
  find ./node/deps -name "zlib.gyp" | while read -r f; do
    sed -i "" "s/\(ADLER32_SIMD\|DEFLATE_SLIDE_HASH\|INFLATE_CHUNK_SIMD\)_[A-Z0-9_]*/INFLATE_CHUNK_GENERIC/g" "$f"
  done
}

function build() {
  cd ./node
  # ARM host 交叉编 x64：CPUID 内联 asm 无法跨架构生成——跳过 zlib 运行时 SIMD 检测
  # （clang17 对 cpuid.h 报 invalid constraint；仅少 SSE 加速，功能不受影响）
  sed -i.bak '1i\
#define CPU_NO_SIMD' deps/zlib/cpu_features.c

  ./configure \
    --shared \
    --dest-cpu x64 \
    --dest-os mac

  make -j8
  cd ../
}

function copy() {
  rm -rf release/libnode-$os-$arch
  mkdir -p release/libnode-$os-$arch
  cp ./node/out/Release/libnode.* ./release/libnode-$os-$arch
  cp ./node/out/Release/node ./release/libnode-$os-$arch
  ln ./release/libnode-$os-$arch/libnode.* ./release/libnode-$os-$arch/libnode.dylib
}

function default() {
  prepare
  clone
  build
  copy
}

if [ "$1" = "" ]; then
  default
else
  $1
fi