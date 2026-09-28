# Toma MVP Tasks

This backlog is ordered by dependency. Ownership is exclusive while an item is in progress so parallel work does not overlap.

## Foundation

- [x] Define the workspace dependency graph and crate ownership boundaries. Owner: coordinator
- [x] Define shared IDs, entities, commands, events, lifecycle states, and transition rules. Owner: coordinator
- [x] Define public contracts for persistence, runners, worktrees, and orchestration. Owner: coordinator
- [x] Compile and test the shared foundation before parallel implementation. Owner: coordinator

## Parallel Implementation

- [ ] Implement SQLite migrations, WAL configuration, repositories, seed data, restart recovery, and persistence tests. Owner: storage agent
- [ ] Implement the provider-neutral runner, a functional Claude CLI adapter, a transparent Codex availability adapter, cancellation, and event streaming tests. Owner: runner agent
- [ ] Implement lazy Git worktree creation, branch ownership, a single-writer lock, conservative cleanup decisions, and tests using temporary repositories. Owner: worktree agent
- [ ] Implement application orchestration for posting messages, structured agent mentions, thread/session/run creation, collaborator attachment, lifecycle events, and recovery. Owner: core agent
- [ ] Implement the GPUI workspace shell, channel navigation, conversation and thread panes, status indicators, multiline composer, drafts, and mention palette. Owner: UI agent

## Integration And Verification

- [ ] Wire the desktop binary to storage, orchestration, runners, worktrees, and GPUI. Owner: coordinator
- [ ] Add an app bundle build script and local development launch command. Owner: coordinator
- [ ] Run formatting, unit tests, integration tests, Clippy, and a release build. Owner: coordinator
- [ ] Launch the macOS app and verify the primary workflow visually and interactively. Owner: coordinator
- [ ] Document how to build, launch, and exercise the first MVP. Owner: coordinator

## First-Build Acceptance

- [ ] Open a repository-backed workspace and see seeded channels and agents.
- [ ] Send multiline messages with `Enter` to send and `Shift+Enter` for a newline.
- [ ] Mention an agent and receive a durable task thread, session, and run.
- [ ] Open a task thread, add a collaborator, and see both sessions separately.
- [ ] Restart the app and recover messages, drafts, threads, and nonterminal state.
- [ ] Inspect runner availability and worktree state without hidden terminal ownership.
