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

  # ARM mac 交叉编 x64：zlib SIMD 全降级纯 C（python 正则跨平台一致，BSD sed 不支持 \| 交替）
  python3 - <<'EOF'
import re, pathlib
for p in pathlib.Path('./node/deps').rglob('zlib.gyp'):
    s = p.read_text()
    s = re.sub(r'(ADLER32_SIMD|DEFLATE_SLIDE_HASH|INFLATE_CHUNK_SIMD)_[A-Z0-9_]*', 'INFLATE_CHUNK_GENERIC', s)
    # x86 专用编译选项（-mssse3 等）在 ARM host 交叉时被 clang 拒绝
    s = re.sub(r"'-mssse3'\s*,?\s*", '', s)
    s = re.sub(r"'-msse4\.2'\s*,?\s*", '', s)
    s = re.sub(r"'-mpclmul'\s*,?\s*", '', s)
    p.write_text(s)
EOF
}

function build() {
  cd ./node
  # ARM host 交叉编 x64：显式 -arch（V8 裸 asm 需 x64 汇编器，configure 不会自动加）
  export CC="clang -arch x86_64"
  export CXX="clang++ -arch x86_64"
  # CPUID 内联 asm 跨架构不可生成——全部 zlib 副本（deps/zlib + V8 副本）跳过运行时 SIMD 检测
  python3 - <<'EOF'
import pathlib
# cwd 为 node/ 源码根（build 已 cd），全库递归
for p in pathlib.Path('.').rglob('cpu_features.c'):
    body = p.read_text()
    body = body.replace('#elif defined(ADLER32_SIMD_SSSE3)', '#elif 0')
    p.write_text('#define CPU_NO_SIMD\n' + body)
EOF

  ./configure \
    --shared \
    --dest-cpu x64 \
    --dest-os mac \
    --openssl-no-asm

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