<p align="center">
  <img src="./etc/banner.svg" alt="CodexPotter banner" />
</p>

<p align="center">
  <img src="./etc/screenshot.png" alt="CodexPotter screenshot" width="80%" />
</p>

&ensp;

## 💡 Why CodexPotter

**CodexPotter** continuously **reconciles** code base toward your instructed state ([Ralph Wiggum pattern](https://ghuntley.com/ralph/)):

- 🤖 **Codex-first** — Codex subscription is all you need; no extra LLM needed.
- 🧭 **Auto-review / reconcile** — Review and polish multi rounds until fully aligned with your instruction.
- 🚀 **Never worse than Codex** — Drive Codex, nothing more; no business prompts which may not suit you.
- 🧩 **Seamless integration** — AGENTS.md, skills & MCPs just work™ ; opt in to improve plan / review.
- 🧠 **File system as memory** — Store instructions in files to resist compaction and preserve all details.
- 🪶 **Tiny footprint** — Use [<1k tokens](./cli/prompts/developer_prompt.md), ensuring LLM context fully serves your business logic.
- 📚 **Built-in knowledge base** — Keep a local KB as index so Codex learns project fast in clean contexts.

&ensp;

## ⚡️ Getting started

**1. Prerequisites:** ensure you have [codex CLI](https://developers.openai.com/codex/quickstart?setup=cli) locally. CodexPotter drives your local codex to perform tasks.

**2. Install CodexPotter via npm or bun:**

```shell
# Install via npm
npm install -g codex-potter
```

```shell
# Install via bun
bun install -g codex-potter
```

**3. Run:** Start CodexPotter in your project directory, just like Codex:

```sh
# --yolo is recommended to be fully autonomous
codex-potter --yolo
```

⚠️ **Note:** Unlike Codex, every follow up prompt turns into a **new** task, **not sharing previous contexts**. Assign tasks to CodexPotter, instead of chat with it.

## 🔁 Resume a project

Replay history for an existing `.codexpotter` project and optionally continue iterating:

```sh
# Open the resume picker (no path)
codex-potter resume

# Or resume directly by project path
codex-potter resume 2026/02/01/1

# Continue in yolo mode (optional)
codex-potter resume 2026/02/01/1 --yolo
```

In the picker: type to search; `tab` toggles sort (Created/Updated); `enter` to resume, `esc` to start new, `ctrl + c` to quit.

Supported project path forms include `2026/02/01/1`, `.codexpotter/projects/2026/02/01/1`, and absolute paths (with or without `MAIN.md`).

&ensp;

## Roadmap

- [x] Skill popup
- [x] Resume (history replay + continue iterating)
- [x] Better handling of stream disconnect / similar network issues
- [ ] Better sandbox support
- [ ] Interoperability with codex CLI sessions (for follow-up prompts)
- [ ] Allow opting out knowledge base
- [ ] Recommended skills for PRD and code review

&ensp;

## Development

```sh
# Formatting
cargo fmt

# Lints
cargo clippy

# Tests
cargo nextest run

# Build
cargo build
```

&ensp;

## License

This project is community-driven fork of [openai/codex](https://github.com/openai/codex) repository, licensed under the same Apache-2.0 License.
