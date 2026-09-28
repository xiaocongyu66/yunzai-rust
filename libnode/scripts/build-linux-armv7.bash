#!/usr/bin/env bash
set -e

# linux arm32 (armv7) — 在 x86_64 runner 上交叉编译
# Tested on Ubuntu 22.04

os="linux"
arch="armv7"

function prepare() {
  export DEBIAN_FRONTEND="noninteractive"
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
    linux-libc-dev \
    build-essential \
    wget \
    cmake \
    jq \
    gcc-arm-linux-gnueabihf \
    g++-arm-linux-gnueabihf \
    libc6-dev-armhf-cross \
    libstdc++-13-dev-armhf-cross \
    libgcc-13-dev-armhf-cross \
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
  # zlib.gyp 的 arm_fpu 默认即 neon，SIMD 宏在 32 位/交叉下必崩——全量清空
  sed -i "s/\(ADLER32_SIMD\|DEFLATE_SLIDE_HASH\|INFLATE_CHUNK_SIMD\)_[A-Z0-9_]*/NO_SIMD_DEFINED/g" ./node/deps/zlib/zlib.gyp
}

function build() {
  cd ./node
  export CC=arm-linux-gnueabihf-gcc
  export CXX=arm-linux-gnueabihf-g++
  export CC_host=gcc
  export CXX_host=g++
  ./configure \
    --shared \
    --dest-cpu arm \
    --dest-os linux \
    --cross-compiling \
    --with-arm-float-abi hard \
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
