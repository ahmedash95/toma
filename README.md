# Toma

Toma is a native macOS workspace for coordinating coding agents through durable channels and task threads. The current repository contains the first MVP implementation described in [product.md](product.md), with progress tracked in [TODO.md](TODO.md).

## Prerequisites

- macOS with Xcode and the command-line tools
- Xcode's optional Metal Toolchain (`xcodebuild -downloadComponent MetalToolchain`)
- Rust stable through `rustup`
- Git
- Claude Code CLI and/or Codex CLI for live agent runs

## Development

```sh
cargo test --workspace
cargo run -p toma -- /path/to/git/repo   # defaults to the current directory
```

Toma stores local development state under `.toma/` in the selected repository. That directory is ignored by Git.

## Build The App

```sh
./scripts/build-app.sh debug
open target/debug/Toma.app --args /path/to/git/repo
```

Mention `@Claude` or `@Codex` in a channel to start a task thread; mention another agent inside that thread to add a collaborator. Agent runs execute in `.toma/worktrees/<thread-id>` on branch `toma/<thread-id>`. Runs that were still executing when the app quit are marked failed on the next launch.

Use `./scripts/build-app.sh release` for an optimized bundle at `target/release/Toma.app`. Early local bundles are unsigned and unnotarized, so macOS may require a one-time approval in System Settings under Privacy & Security.

