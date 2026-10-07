<div align="center">

# Yunzai Rust

### 用 Rust 重写的 TRSS-Yunzai 机器人框架

稳定的 Rust 核心，熟悉的 JavaScript 插件生态，开箱即用的 ARM64 与 x86_64 运行包。

[![Build](https://github.com/xiaocongyu66/yunzai-rust/actions/workflows/build.yml/badge.svg)](https://github.com/xiaocongyu66/yunzai-rust/actions/workflows/build.yml)
[![License: GPL-3.0](https://img.shields.io/badge/License-GPL--3.0-blue.svg)](LICENSE)
[![Rust Edition](https://img.shields.io/badge/Rust-2024-orange.svg)](Cargo.toml)

</div>

---

## 项目介绍

Yunzai Rust 是对 [TRSS-Yunzai](https://github.com/TimeRainStarSky/Yunzai) 的 Rust 重写。项目保留 Yunzai 的机器人事件与 JavaScript 插件使用方式，以 Rust 承担核心服务、OneBot v11 适配和运行时管理，并在进程内嵌入 Node.js 执行原有 JavaScript 插件。

目标不是另造一套插件生态，而是在轻量、易部署的 Rust 核心上继续运行熟悉的 Yunzai 插件。

## 特性

- **Rust 核心服务**：配置、日志、事件分发、插件加载与 HTTP 服务由 Rust 实现。
- **OneBot v11**：支持 WebSocket 连接、事件接收、消息发送及常用机器人 API。
- **原生 JavaScript 插件**：通过内嵌 Node.js 与 N-API bridge，在同一进程内加载 JavaScript 插件，无需单独维护 Node 服务。
- **完整运行时包**：发布包包含匹配架构的 `libnode`、N-API bridge、JavaScript 依赖和 Yunzai runtime 文件。
- **多架构发布**：GitHub Actions 构建并验证 `aarch64-unknown-linux-gnu` 与 `x86_64-unknown-linux-gnu`。
- **Rust 渲染器**：基于 ps-blitz 与 Vello CPU 渲染栈，提供 HTML/CSS 截图能力。
- **可选 Redis**：支持配置 Redis，也可通过 `YZ_NO_REDIS=1` 在无 Redis 环境启动。

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

从 GitHub Actions 的 **Build** 工作流下载与你的 Linux 架构匹配的 `release-*` artifact，解压后在运行目录启动：

```bash
./bin/yunzai
```

无 Redis 环境可使用：

```bash
YZ_NO_REDIS=1 ./bin/yunzai
```

配置文件位于 `config/config/`；首次启动会从 `config/default_config/` 初始化缺失配置。OneBot v11 默认 WebSocket 端点为：

```text
ws://<机器人主机>:2536/OneBotv11
```

## 构建与验证

项目提供 GitHub Actions 构建工作流，分别生成 ARM64 与 x86_64 Linux 运行包。每个架构的工作流会构建 Rust 主程序和 N-API bridge、组装完整 JavaScript runtime bundle、运行 bundle smoke 与 E2E 测试，并上传对应 release artifact。

本地开发需要 Rust stable toolchain、目标平台对应的 linker，以及仓库配置的 JavaScript runtime 源码和依赖。正式运行建议直接使用 CI 生成的完整 artifact，避免混用不同架构的 `libnode` 或 bridge。

## 许可证

本项目以 GPL-3.0 发布，详见 [LICENSE](LICENSE)。项目集成的 TRSS-Yunzai 与第三方依赖保留各自原有许可证及版权声明。
