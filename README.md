<p align="center">
  <img src="./etc/banner.svg" alt="CodexPotter banner" />
</p>

<p align="center">
  <img src="./etc/screenshot.png" alt="CodexPotter screenshot" width="80%" />
</p>

## 💡 Why CodexPotter

**CodexPotter** continuously **reconciles** code base toward your instructed state ([Ralph Wiggum pattern](https://ghuntley.com/ralph/)):

- 🤖 **Codex-first** — Codex subscription is all you need; no extra LLM needed.

- 🧭 **Auto-review / reconcile** — Review and polish until fully aligned with your instruction.

- 🚀 **Never worse than Codex** — Drive Codex, nothing more; no business prompts which may not suit you.

- 🧩 **Seamless integration** — AGENTS.md, skills & MCPs just work™ ; opt in to improve plan / review.

- 🧠 **File system as memory** — Store instructions in files to resist compaction and preserve all details.

- 🪶 **Tiny footprint** — Use [<1k tokens](./cli/prompts/developer_prompt.md), ensuring LLM context fully serves your business logic.

- 📚 **Built-in knowledge base** — Keep a local KB as index so Codex learns project fast in clean contexts.

## ⚡️ Getting started

### Install (recommended)

```sh
npm install -g codex-potter
```

Then run:

```sh
codex-potter --yolo
```

Supported platforms (via prebuilt native binaries):

- macOS: Apple Silicon + Intel
- Linux: x86_64 + aarch64
- Windows: x86_64 + aarch64 (ARM64)
- Android: treated as Linux (uses the bundled Linux musl binaries)

### Build from source

```sh
cargo build
```

Then, run CodexPotter CLI (available in `target/debug/codex-potter`) in your project directory, just like `codex`:

```sh
codex-potter --yolo
```

⚠️ **Note:** Unlike codex, follow up prompts will become a **new** task assigned to CodexPotter, **without sharing contexts**.

## Roadmap

- [x] Skill popup
- [ ] Resume / project management
- [ ] Better sandbox support
- [ ] Interoperability with codex CLI sessions (for follow-up prompts)
- [ ] Allow opting out knowledge base
- [ ] Recommended skills for PRD and code review

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

## License

This project is community-driven fork of [openai/codex](https://github.com/openai/codex) repository, licensed under the same Apache-2.0 License.
