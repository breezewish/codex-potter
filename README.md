![codex-potter](./etc/banner.svg)

<p align="center"><strong>CodexPotter</strong> continuously <strong>reconciles</strong> codebase toward your instructed state</p>

<p align="center"><em>(the <a href="https://ghuntley.com/ralph/">Ralph Wiggum</a> pattern)</em></p>

## 💡 Why CodexPotter

- 🤖 **Codex-first** — Codex subscription is all you need; no extra LLM needed.

- 🚀 **Never worse than Codex** — Drive Codex, nothing more; no business prompts which may not suit you.

- 🧩 **Seamless integration** — AGENTS.md and skills just work™ ; utilize local skills to plan, review, etc.

- 🪶 **Tiny footprint** — Only use [<1k tokens](./cli/prompts/developer_prompt.md), ensuring LLM context fully serves your business logic.

- 🧠 **File system as memory** — Store instructions in files to resist compaction and preserve all details.

- 📚 **Built-in knowledge base** — Keep a local KB as index so Codex learns project fast in clean contexts.

## ⚡️ Getting started

```sh
cargo build
```

Then, run CodexPotter CLI (available in `target/debug/codex-potter`) in your project directory, just like `codex`:

```sh
codex-potter --yolo
```

⚠️ **Note:** Unlike codex, follow up prompts will become a **new** task assigned to CodexPotter, **without sharing contexts**.

## Roadmap

- [ ] Skill popup
- [ ] Resume / project management
- [ ] Better sandbox support
- [ ] Interoperability with codex CLI sessions (for follow-up prompts)
- [ ] Allow opting out knowledge base
- [ ] Recommended skills for PRD and code review

## Development

Our GitHub Actions CI runs the following checks on every PR and on pushes to `main`.
You can run the same commands locally:

```sh
# Formatting
cargo fmt --all -- --check

# Lints
cargo clippy --workspace --all-targets --locked -- -D warnings

# Tests (uses the repo's `ci-test` profile for faster CI-style builds)
cargo test --workspace --locked --profile ci-test

# Build
cargo build --workspace --all-targets --locked
```
