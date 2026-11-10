# Ichibou

[English](README.md) · [日本語](README.ja.md) · **简体中文**

在编码智能体构建的过程中看清它正在做什么，并确认它是否真的完成。

Ichibou 是一个本地工具，面向那些让 Claude Code 或 Codex 一次性构建整个产品、而不是分成许多小阶段逐步审批的人。这种做法会留下两个问题：*智能体现在在做什么？* 以及 *这真的做完了吗？* Ichibou 就是用来回答它们的。

名称取自日语的“一望”（ichibō），意思与中文相同：一眼望尽全貌。

> 项目仍处于早期阶段，使用前请先查看[目前可用的功能](#目前可用的功能)。

## 功能

- **需求清单。** 智能体首先列出最终产品的全部需求。每一项都会显示它是正在构建、需要验证、未通过还是已验证，以及排在它前面的是什么。
- **勾选代表证据，而不是声明。** 智能体无法把任何需求标记为完成。Ichibou 会复制工作区，在副本中构建并启动应用，然后运行你批准过的场景测试。只要改动任何文件，之前的通过结果就会被撤下，直到重新验证。
- **前端标签页。** 实时预览逐步成形的应用：Web 应用通过本地代理显示，iOS 模拟器、Android 模拟器和 macOS 窗口以定时刷新的截图显示。还可以浏览项目中的图片和字体。
- **标注。** 点击一个元素（或拖动框选区域），写下需要修改的地方或你想了解的问题。修改请求会进入智能体的队列，提问会在对话中得到回答。是否已解决由你决定。
- **后端标签页。** 从一开始就画出目标架构。计划中的部分颜色较淡，已有代码的部分为实线，已验证的部分带有标记。点击 API 可以查看 OpenAPI 操作并向正在运行的开发服务器发送请求；点击数据库可以查看表结构。
- **场景标签页。** 由另一个独立的智能体会话根据验收标准编写场景测试，由你批准。如果负责构建的智能体之后修改了已批准的测试，在你批准修改之前，仍按已批准的版本判定。

## 如何驱动智能体

Ichibou 以子进程的方式启动你电脑上已安装并已登录的官方命令行工具 `claude` 和 `codex`。它不读取、不保存、也不转发凭据，本身也不调用任何模型 API。用量取决于你自己的 Claude 或 ChatGPT 套餐。

Ichibou 绝不会在绕过权限检查的情况下启动智能体。会写入文件的 Claude 会话使用你配置的权限模式（默认 `acceptEdits`）；Codex 会话在其工作区沙箱中运行，需要批准的命令会通过 Ichibou 向你确认。

## 安装

环境要求：macOS、Rust 1.85+、Node.js 20+，以及已登录的 [Claude Code](https://claude.com/claude-code) 或 [Codex CLI](https://developers.openai.com/codex)（至少其一）。

```sh
git clone <本仓库> ichibou && cd ichibou
npm --prefix ui install && npm --prefix ui run build
cargo install --path crates/ichibou-cli
ichibou doctor
```

## 使用

```sh
cd your-project        # 空目录或已有仓库均可
ichibou               # 启动守护进程并打开界面
```

1. 描述你想构建的东西（如果是已有仓库，可以让智能体先梳理它）。
2. 查看并采纳需求、架构以及 Ichibou 将要运行的命令。
3. 让智能体编写场景测试，然后批准。
4. 点击 **全部构建**。观察清单、预览和架构图逐步填满，并随时添加标注。
5. 当所有需求都在当前版本上通过验证、人工确认项已勾选、反馈已处理时，这次请求就完成了。

Ichibou 对项目的全部了解都保存在项目内的 `.ichibou/` 中：`requirements.yaml`、`architecture.yaml`、`targets.yaml` 和 `scenarios/` 应当提交到版本库。Ichibou 自身的状态（批准记录、验证结果、标注、日志）保存在项目之外的 `~/Library/Application Support/Ichibou/` 中，因此在项目内工作的智能体无法改写它。

### 不消耗智能体用量进行体验

```sh
cp -R fixtures/web-demo /tmp/web-demo
ichibou --project /tmp/web-demo serve --agent fake --fake-script fixtures/web-demo-script.json
```

一个由脚本驱动的替身智能体会一步步构建一个小型笔记应用。

### 构建发布包

```sh
scripts/package.sh     # 生成 dist/ichibou-<version>-<os>-<arch>.tar.gz 及其 SHA-256
```

可执行文件内置了界面，发布时只需要这个压缩包。

## 目前可用的功能

| 范围 | 状态 |
|---|---|
| 需求、状态判定、快照、在干净副本中验证 | 可用，有测试覆盖 |
| Claude Code 与 Codex：通过守护进程列出需求 | 在 Claude Code 2.1 和 Codex CLI 0.167 上可用 |
| Claude Code：完整构建、验证、根据标注修改 | 可用（已构建示例应用，已批准的场景全部通过） |
| Codex：带验证反馈的完整构建 | 已实现；目前只运行过列出需求这一步 |
| Claude Code 与 Codex：梳理已有仓库、回答提问标注 | 可用（只读会话：仅有读取类工具和 Ichibou 自身的工具） |
| Web 预览、元素与区域标注 | 可用 |
| iOS 模拟器预览（轻触需要 `idb`） | 可用；安装 [serve-sim](https://github.com/EvanBacon/serve-sim) 后为实时视频，否则每一两秒刷新一次截图 |
| Android 模拟器预览（`adb`） | 可用；在支持 WebCodecs 的浏览器（Chrome、Edge、较新的 Safari）中为实时视频，否则每隔几秒刷新一次截图 |
| macOS 窗口预览 | 可用，仅查看（不转发操作）。启动 `ichibou` 的应用（终端，或其上常驻的多路复用器）需要屏幕录制权限 |
| OpenAPI 查看与请求发送、SQLite 表结构 | 可用 |
| PostgreSQL 表结构 | 可用 |
| 场景运行器：任何输出 JUnit XML 的命令 | 可用 |
| 场景运行器：Maestro | 在 iOS 模拟器和 Android 模拟器上可用（需要 Java） |
| 场景运行器：Playwright | 可用 |

## 开发

```sh
cargo test --workspace
npm --prefix ui run typecheck
npm --prefix ui run e2e      # 用真实的守护进程和脚本替身智能体跑通界面
```

目录结构：`crates/ichibou-core`（模型、状态判定、快照、存储）、`ichibou-agents`（Claude、Codex 与脚本驱动）、`ichibou-verify`（验证运行器）、`ichibou-server`（守护进程：API、MCP 工具、预览、检查器）、`ichibou-cli`、`ui/`（React）、`overlay/`（注入到 Web 预览中的脚本）。

## 许可证

Apache-2.0。Ichibou 与 Anthropic 和 OpenAI 没有任何关联。
