#!/usr/bin/env bash
set -Eeuo pipefail

root=${1:?usage: bundle-smoke.sh BUNDLE_ROOT [REFERENCE_ROOT]}
reference=${2:-}
fail=0

need_file() {
  local path="$root/$1"
  if [[ ! -f "$path" ]]; then
    printf 'missing file: %s\n' "$1" >&2
    fail=1
  fi
}

need_nonempty_file() {
  need_file "$1"
  local path="$root/$1"
  if [[ -f "$path" && ! -s "$path" ]]; then
    printf 'empty file: %s\n' "$1" >&2
    fail=1
  fi
}

need_dir() {
  if [[ ! -d "$root/$1" ]]; then
    printf 'missing directory: %s\n' "$1" >&2
    fail=1
  fi
}

need_file bin/yunzai
if [[ -f "$root/bin/yunzai" && ! -x "$root/bin/yunzai" ]]; then
  printf 'not executable: bin/yunzai\n' >&2
  fail=1
fi
need_nonempty_file lib/yz_bridge.node
need_file lib/util.js
need_file lib/config/init.js
need_file lib/plugins/runtime.js
need_dir config/default_config
need_dir plugins
need_dir plugins/adapter
need_file package.json
need_file pnpm-lock.yaml
need_dir node_modules
for package in ws file-type level; do
  need_dir "node_modules/$package"
  need_file "node_modules/$package/package.json"
done
for package in body-parser token-types; do
  need_dir "node_modules/$package"
  need_file "node_modules/$package/package.json"
done
libnode=$(find "$root/data/libnode" -maxdepth 1 -type f -print -quit 2>/dev/null || true)
if [[ -z "$libnode" ]]; then
  printf 'missing libnode file under: data/libnode\n' >&2
  fail=1
elif [[ ! -s "$libnode" ]]; then
  printf 'empty libnode file: %s\n' "${libnode#"$root/"}" >&2
  fail=1
fi
need_file host.mjs

if [[ -n "$reference" && -f "$reference/src/nodejs/host.mjs" && -f "$root/host.mjs" ]]; then
  if ! cmp -s "$root/host.mjs" "$reference/src/nodejs/host.mjs"; then
    printf 'host.mjs differs from embedded source: %s\n' "$reference/src/nodejs/host.mjs" >&2
    fail=1
  fi
fi

if [[ "$fail" -ne 0 ]]; then
  printf 'bundle smoke test failed: %s\n' "$root" >&2
  exit 1
fi
printf 'bundle smoke test passed: %s\n' "$root"
