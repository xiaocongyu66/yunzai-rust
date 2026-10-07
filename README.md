<div align="center">

# Yunzai Rust

### 可直接替换 TRSS-Yunzai 的 Rust 运行核心

兼容原版 JavaScript 插件生态，支持 ARM64 与 x86_64，面向长期运行和直接部署。

[![Build](https://github.com/xiaocongyu66/yunzai-rust/actions/workflows/build.yml/badge.svg)](https://github.com/xiaocongyu66/yunzai-rust/actions/workflows/build.yml)
[![License: GPL-3.0](https://img.shields.io/badge/License-GPL--3.0-blue.svg)](LICENSE)
[![Rust Edition](https://img.shields.io/badge/Rust-2024-orange.svg)](Cargo.toml)

</div>

---

## 项目介绍

Yunzai Rust 是 [TRSS-Yunzai](https://github.com/TimeRainStarSky/Yunzai) 的完成版 Rust 运行核心，可直接作为原版 TRSS-Yunzai 的替代运行时使用。

它保留原版的配置目录、插件目录、JavaScript 扩展方式和 OneBot v11 接入习惯，同时将核心服务、事件分发、运行时管理和跨语言桥接迁移到 Rust。原有 TRSS-Yunzai 插件无需改造成 Rust 插件，直接放入发布包即可继续运行。

项目不是实验性 demo，也不是需要额外 Node 服务的半成品。CI 已对 ARM64 与 x86_64 发布包完成构建、bundle smoke、资源回归和 E2E 验证。

## 特性

- **Rust 核心服务**：配置、日志、事件分发、插件加载与 HTTP 服务由 Rust 实现。
- **OneBot v11**：支持 WebSocket 连接、事件接收、消息发送及常用机器人 API。
- **原生 JavaScript 插件**：通过内嵌 Node.js 与 N-API bridge，在同一进程内加载 JavaScript 插件，无需单独维护 Node 服务。
- **完整运行时包**：发布包包含匹配架构的 `libnode`、N-API bridge、JavaScript 依赖和 Yunzai runtime 文件。
- **多架构发布**：GitHub Actions 构建并验证 `aarch64-unknown-linux-gnu` 与 `x86_64-unknown-linux-gnu`。
- **Rust 渲染器**：基于 ps-blitz 与 Vello CPU 渲染栈，提供 HTML/CSS 截图能力。
- **原版 Redis 行为**：默认按 `config/config/redis.yaml` 连接 Redis；本机连接失败时按配置尝试启动 `redis-server`，仅在显式设置 `YZ_NO_REDIS=1` 时跳过。
- **可替换部署**：保留原版配置、插件和 JavaScript 运行方式，可直接替换 TRSS-Yunzai 的核心运行文件。

## 架构一览

```text
OneBot v11 WebSocket
          │
          ▼
┌──────────────────────────────┐
│        Yunzai Rust Core       │
│  Config · Events · Adapters   │
│  Plugin Loader · HTTP Server │
└──────────────┬───────────────┘
               │ N-API bridge
               ▼
┌──────────────────────────────┐
│     Embedded Node.js Runtime  │
│   TRSS-Yunzai JS Plugins      │
└──────────────────────────────┘
```

## 快速开始

从 GitHub Actions 的 **Build** 工作流下载与你的 Linux 架构匹配的 `release-*` artifact，解压后即可作为 TRSS-Yunzai 的运行目录启动：

```bash
./bin/yunzai
```

发布包已经包含匹配架构的 Node.js runtime、`yz_bridge.node`、JavaScript 依赖和插件运行文件，不需要再单独安装 Node.js 或复制旧版运行时。

配置文件位于 `config/config/`；首次启动会从 `config/default_config/` 初始化缺失配置。Redis 默认按配置文件连接或启动，只有无 Redis 环境时才显式使用：

```bash
YZ_NO_REDIS=1 ./bin/yunzai
```

OneBot v11 默认 WebSocket 端点为：

```text
ws://<机器人主机>:2536/OneBotv11
```

## 当前状态

### 已完成

- Rust 核心服务、配置、日志、插件加载和事件分发。
- OneBot v11 WebSocket 适配与消息收发。
- 进程内 Node.js runtime、N-API bridge 和原版 JavaScript 插件兼容层。
- 按原版配置文件工作的 Redis 连接与本地 `redis-server` 自动启动。
- ARM64 与 x86_64 完整运行包，以及双架构 CI 构建和 E2E 验证。

### 已知问题

渲染器目前仍存在与 Chromium/原版 Puppeteer 的行为差异，尤其是复杂 HTML/CSS、表格布局、绝对定位、部分字体与像素级截图场景。渲染器可以使用，但不能宣称已经完全替代原版渲染结果。

欢迎针对主项目和渲染器提交 Issue 或 Pull Request，尤其欢迎以下方向的贡献：

- CSS 与表格布局兼容性；
- 绝对定位、百分比高度和 overflow；
- 图片、字体、外部资源加载；
- Chromium 对照截图与像素级回归；
- OneBot 适配、插件兼容和部署体验。

## 构建与验证

项目提供 GitHub Actions 构建工作流，分别生成 ARM64 与 x86_64 Linux 运行包。每个架构的工作流会构建 Rust 主程序和 N-API bridge、组装完整 JavaScript runtime bundle、运行 bundle smoke 与 E2E 测试，并上传对应 release artifact。

本地开发需要 Rust stable toolchain、目标平台对应的 linker，以及仓库配置的 JavaScript runtime 源码和依赖。正式运行建议直接使用 CI 生成的完整 artifact，避免混用不同架构的 `libnode` 或 bridge。

## 参与贡献

欢迎提交 Issue、Pull Request 和可复现的测试用例。涉及渲染器的问题，最好同时提供：

- 输入 HTML/CSS 或最小复现模板；
- 原版 Chromium/Puppeteer 截图与当前截图；
- 使用的架构、Node.js/runtime 版本和运行日志；
- 预期行为与实际差异。

## 许可证

本项目以 GPL-3.0 发布，详见 [LICENSE](LICENSE)。项目集成的 TRSS-Yunzai 与第三方依赖保留各自原有许可证及版权声明。
