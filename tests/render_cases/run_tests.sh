#!/usr/bin/env bash
# 渲染回归测试：把本目录 14 个特性用例逐个交给 yunzai --render-test 渲染成 PNG，
# 校验退出码为 0 且 PNG 体积 > 500 字节（空白图/崩溃/读盘失败都会被拦下）。
#
# 用法:  bash tests/render_cases/run_tests.sh <yunzai二进制路径>
# 产物:  /tmp/render_test_out/<用例名>.png 与 <用例名>.log（渲染日志，失败时看这里）
# 退出:  全部通过 → 0；任一失败 → 1；用法错误 → 2
# 约束:  本脚本不做任何编译（无 cargo/rustc 调用），只消费已构建好的二进制。
set -u

DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="${1:-}"
OUT="${RENDER_TEST_OUT:-/tmp/render_test_out}"
WIDTH="${RENDER_TEST_WIDTH:-600}"

if [ -z "$BIN" ]; then
  echo "用法: $0 <yunzai二进制路径>" >&2
  exit 2
fi
if [ ! -x "$BIN" ]; then
  echo "错误: 二进制不存在或不可执行: $BIN" >&2
  exit 2
fi

mkdir -p "$OUT"

pass=0
fail=0
failed_cases=""

for html in "$DIR"/t*.html; do
  name="$(basename "$html" .html)"
  png="$OUT/$name.png"
  log="$OUT/$name.log"
  rm -f "$png"

  if "$BIN" --render-test "$html" "$png" "$WIDTH" >"$log" 2>&1; then
    size="$(wc -c <"$png" 2>/dev/null || echo 0)"
    if [ "${size:-0}" -gt 500 ]; then
      echo "PASS $name (${size} bytes)"
      pass=$((pass + 1))
      continue
    fi
    echo "FAIL $name (PNG 缺失或 ≤500 字节: ${size}B, 日志: $log)"
  else
    echo "FAIL $name (渲染退出码非 0, 日志: $log)"
  fi
  fail=$((fail + 1))
  failed_cases="$failed_cases $name"
done

echo "----------------------------------------"
echo "渲染回归总计: $((pass + fail))  通过: $pass  失败: $fail"
if [ "$fail" -gt 0 ]; then
  echo "失败用例:$failed_cases"
  exit 1
fi
exit 0
