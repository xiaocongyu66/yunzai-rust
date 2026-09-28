#!/usr/bin/env bash
set -e

# linux i386 (x86 32位) — 在 x86_64 runner 上用 multilib 编译
# Tested on Ubuntu 22.04

os="linux"
arch="i386"

function prepare() {
  export DEBIAN_FRONTEND="noninteractive"
  dpkg --add-architecture i386
  apt-get update --yes
  apt-get install --yes \
    ca-certificates \
    curl \
    gnupg \
    git \
    nodejs \
    python3 \
    python3-pip \
    make \
    ccache \
    build-essential \
    gcc-multilib \
    g++-multilib \
    libc6-dev-i386 \
    libssl-dev \
    wget \
    cmake \
    jq \
    pkg-config
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

  git config --global safe.directory '*'
  git clone "$NODEJS_GIT" --branch "$NODEJS_BRANCH" --depth=1 ./node

  # 32 位/交叉编译时 gyp 的 CPU 检测失效，zlib SIMD 代码编译崩溃——直接关闭
  # zlib SIMD 在 32 位/交叉下必崩；chunkcopy.h 类型必须三选一，统一降级为纯 C 实现（GENERIC）
  # 注意 V8 自带一份独立 zlib（deps/v8/third_party/zlib），同样处理
  find ./node/deps -name "zlib.gyp" | while read -r f; do
    sed -i "s/\(ADLER32_SIMD\|DEFLATE_SLIDE_HASH\|INFLATE_CHUNK_SIMD\)_[A-Z0-9_]*/INFLATE_CHUNK_GENERIC/g" "$f"
  done
}

function build() {
  cd ./node
  export CC="gcc -m32"
  export CXX="g++ -m32"
  ./configure \
    --shared \
    --dest-cpu x86 \
    --dest-os linux \
    --no-cross-compiling \
    --with-intl none \
    --openssl-no-asm
  make -j$(nproc)
  cd ../
}

function copy() {
  rm -rf release/libnode-$os-$arch
  mkdir -p release/libnode-$os-$arch
  cp ./node/out/Release/libnode.* ./release/libnode-$os-$arch
  cp ./node/out/Release/node ./release/libnode-$os-$arch || true
  ln ./release/libnode-$os-$arch/libnode.* ./release/libnode-$os-$arch/libnode.so
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
