#!/usr/bin/env bash
set -Eeuo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
host="$root/src/nodejs/host.mjs"

cd "$root"
grep -q "return rawOp('bot_get', { prop: String(prop) })" "$host"
grep -q "return rawOp('cfg_get', { name: String(prop) })" "$host"
grep -q 'setInterval(() => {}, 2 \*\* 31 - 1)' "$host"
! grep -q 'setInterval(() => {}, 2 \*\* 31)' "$host"

printf 'host facade regression passed\n'
