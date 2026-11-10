# Aperture

**English** · [日本語](README.ja.md) · [简体中文](README.zh-CN.md)

See what your coding agent is building while it builds it, and know when it is really done.

Aperture is a local tool for people who let Claude Code or Codex build a whole product in one go instead of in small approved phases. It answers two questions that approach leaves open: *what is the agent working on right now?* and *is this actually finished?*

> Working name. Status: early. See [What works today](#what-works-today) before relying on it.

## What it does

- **Requirements checklist.** The agent lists every requirement of the finished product first. Each one shows whether it is being built, needs verification, is failing, or is verified, and what comes before it in the queue.
- **A check mark means evidence, not a claim.** The agent cannot mark anything done. Aperture copies the workspace, builds and starts it there, and runs the scenario tests you approved. Edit any file and earlier passes are demoted until they are verified again.
- **Frontend tab.** A live preview of the app as it takes shape: web apps through a local proxy, iOS Simulator, Android Emulator and macOS windows as refreshing screenshots. Browse the project's images and fonts.
- **Annotations.** Click an element (or drag a region), write what should change or what you want to know. Fix requests queue up for the agent; questions get an answer on the thread. You decide when one is resolved.
- **Backend tab.** The target architecture is drawn from the start. Planned parts are faint, parts whose code exists are solid, verified parts are marked. Click an API to see its OpenAPI operations and send requests to the running dev server; click a database to see its tables.
- **Scenarios tab.** A separate agent session writes scenario tests from the acceptance criteria. You approve them. If the building agent later edits an approved test, the approved version keeps being the judge until you approve the change.

## How it drives the agents

Aperture starts the official `claude` and `codex` command-line tools that are already installed and signed in on your machine, as child processes. It does not read, store or forward credentials, and it does not talk to any model API itself. Usage is whatever your own Claude or ChatGPT plan allows.

Aperture never starts an agent with permission checks bypassed. Claude sessions that write use the permission mode you configure (`acceptEdits` by default); Codex sessions run in its workspace sandbox and ask through Aperture before running commands that need approval.

## Install

Requirements: macOS, Rust 1.85+, Node.js 20+, and at least one of [Claude Code](https://claude.com/claude-code) or the [Codex CLI](https://developers.openai.com/codex), signed in.

```sh
git clone <this repository> aperture && cd aperture
npm --prefix ui install && npm --prefix ui run build
cargo install --path crates/aperture-cli
aperture doctor
```

## Use

```sh
cd your-project        # an empty directory or an existing repository
aperture               # starts the daemon and opens the UI
```

1. Describe what you want to build (or, for an existing repository, ask the agent to describe it).
2. Review and accept the requirements, the architecture and the commands Aperture will run.
3. Have the agent write scenario tests, then approve them.
4. Press **Build everything**. Watch the checklist, the preview and the diagram fill in; annotate anything.
5. The request is complete when every requirement is verified on the current version, your manual checks are ticked and your feedback is resolved.

Everything Aperture knows about a project lives in `.aperture/` inside it: `requirements.yaml`, `architecture.yaml`, `targets.yaml` and `scenarios/` are meant to be committed. Aperture's own state (approvals, verdicts, annotations, logs) is kept outside the project, in `~/Library/Application Support/Aperture/`, so an agent working in the project cannot rewrite it.

### Try it without spending agent usage

```sh
cp -R fixtures/web-demo /tmp/web-demo
aperture --project /tmp/web-demo serve --agent fake --fake-script fixtures/web-demo-script.json
```

A scripted stand-in agent builds a small notes app step by step.

## What works today

| Area | State |
|---|---|
| Requirements, status rules, snapshots, verification in a clean copy | Works; covered by tests |
| Claude Code and Codex: planning step through the daemon | Works on Claude Code 2.1 and Codex CLI 0.167 |
| Claude Code: full build, verification, fixing an annotation | Works (built the demo app; all approved scenarios passed) |
| Codex: full build with verification feedback | Implemented; only the planning step has been run so far |
| Claude Code and Codex: describing an existing repository, answering a question annotation | Works (read-only sessions: only reading tools and Aperture's own) |
| Web preview, element and region annotation | Works |
| iOS Simulator preview (needs `idb` for taps) | Works; live video with [serve-sim](https://github.com/EvanBacon/serve-sim) installed, screenshots every second or two without it |
| Android Emulator preview (`adb`) | Works; a screenshot every few seconds |
| macOS window preview | Works, view only (no input forwarding). The app that starts `aperture` — the terminal, or a multiplexer daemon it runs under — needs Screen Recording permission |
| OpenAPI view and request runner, SQLite tables | Works |
| PostgreSQL tables | Works |
| Scenario runners: any command that writes JUnit XML | Works |
| Scenario runner: Maestro | Works on the iOS Simulator and the Android Emulator (needs Java) |
| Scenario runner: Playwright | Works |

## Development

```sh
cargo test --workspace
npm --prefix ui run typecheck
```

Layout: `crates/aperture-core` (model, status rules, snapshots, store), `aperture-agents` (Claude, Codex and scripted drivers), `aperture-verify` (verification runner), `aperture-server` (daemon: API, MCP tools, previews, inspectors), `aperture-cli`, `ui/` (React), `overlay/` (script injected into web previews).

## License

Apache-2.0. Aperture is not affiliated with Anthropic or OpenAI.
