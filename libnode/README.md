# libnode — Node.js 嵌入式动态库构建

本目录整合自 [alshdavid-labs/libnode-prebuilt](https://github.com/alshdavid-labs/libnode-prebuilt)（Apache-2.0，原 LICENSE 保留于此），与主程序代码相互独立。

## 用途

为 yunzai-rust 提供自建 libnode 动态库的完整工程：给 Node.js 打上 C FFI 补丁后编译出 `libnode.so`，供 Rust 侧（libnode_rs 绑定）同进程嵌入完整 Node.js 运行时——原生模块、npm 依赖、postinstall 脚本全部原生可用。

- `patches/all/node_embedding.pr` — 核心：为 Node.js 增加 `node_embedding_main` C 接口（上游 [nodejs/node#58207](https://github.com/nodejs/node/pull/58207)），使 Rust/C# 等无法使用 C++ 绑定的语言可嵌入 Node.js
- `patches/v24.0.2/mefi-dll-fix.pr` — v24 平台修复（上游原样保留）
- `scripts/build-linux-arm64.bash` 等 — 各平台构建脚本（下载 Node 源码 → 应用补丁 → 编译）
- `scripts/patches-apply.bash` — 补丁应用辅助

## 构建

统一版本：`NODEJS_BRANCH=v22.22.0`（与 `.github/workflows/binaries.yml` 一致；v24.5.0 的 V8 与 macOS 新版 Xcode 不兼容，勿回退）。以 linux-arm64 为例（Ubuntu 22.04+）：

```bash
export NODEJS_GIT=https://github.com/nodejs/node
export NODEJS_BRANCH=v22.22.0
bash libnode/scripts/build-linux-arm64.bash
```

产物为 `libnode.so`，放置后通过环境变量指定：

```bash
export LIBNODE_PATH=/opt/libnode/libnode.so
```

## 许可

- 本目录及补丁：Apache-2.0（见本目录 LICENSE，来自上游原样保留）
- 主项目：GPL-3.0（见仓库根 LICENSE）
- Node.js 本体遵循其自身许可（MIT 系列，构建时由源码仓库携带）
