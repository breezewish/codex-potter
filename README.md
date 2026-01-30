# codex-potter

codex + ralph loop + (knowledge) = codex-potter

Designing philosophy:

- File system as memory
- Your prompt is always better than us -- potter only helps you run ralph loop, nothing more

## Getting Started

```sh
cargo build
```

Then, run codex-potter CLI (available in `target/debug/codex-potter`) in your project directory:

```sh
codex-potter --yolo
```

Your prompt will become a task assigned to CodexPotter, and CodexPotter will help you run ralph loop to complete it.

Note: During running, you can send more prompts, and all of these prompts will become a **new** task assigned to CodexPotter. Unlike codex, they will not share context.
