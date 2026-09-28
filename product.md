# Toma

## Product Summary

Toma is a macOS desktop application for coordinating coding agents through a familiar, Slack-like chat interface. It gives people one durable place to assign work, collaborate with multiple agents, inspect progress, preserve decisions, and connect agent activity to a Git repository.

Each Toma workspace maps to one Git repository. Channels provide shared context, while task threads contain focused work. Agents participate through isolated sessions and provider-neutral runners, initially supporting Codex CLI and Claude Code CLI.

Toma owns the conversation, context, permissions, persistence, task lifecycle, and event history. External coding agents remain execution backends rather than the source of truth.

## Product Principles

1. **Chat is the control surface.** Starting, guiding, reviewing, and handing off work should feel like messaging a capable teammate.
2. **The user stays oriented.** Every agent action belongs to a visible workspace, channel, task thread, session, and run.
3. **Agent work is durable.** Messages, raw history, task state, attachments, and important decisions survive restarts and failures.
4. **Isolation is the default.** Agents have separate sessions. Code tasks use temporary Git worktrees and branches when mutations begin.
5. **Collaboration is explicit.** Agents may join the same task thread, but they never silently share sessions or write concurrently without coordination.
6. **Context should get smaller without losing truth.** Toma promotes durable facts into compact memory while retaining complete raw history and provenance.
7. **The interface stays quiet and dense.** Toma is an operational tool for repeated daily use, not a dashboard or marketing surface.

## Target User

The initial user is a software developer on macOS who regularly uses coding agents and wants to coordinate more than one task or agent without juggling terminal sessions, scattered transcripts, and manually managed worktrees.

## Core Concepts

### Workspace

A workspace maps to exactly one local Git repository. It contains channels, people, agent definitions, repository rules, skills, and durable memory.

### Channel

A channel is the shared conversation and dispatch surface for a workspace. It holds messages, attachments, agent mentions, and compact channel memory. A channel is not itself an agent session.

### Agent Definition

An agent definition describes a reusable participant: name, role, instructions, capabilities, permissions, preferred runner, and available skills. It is configuration, not a running process.

### Task Thread

A task thread is the durable unit of focused work created when an agent is assigned a task. It contains the full task conversation, collaborators, attachments, lifecycle state, and eventually its worktree and branch.

### Session

A session is one agent's isolated working context attached to a task thread. Adding a second agent creates a second session with access to the shared thread context; it does not expose or merge another agent's private session state.

### Run

A run is one execution attempt within a session. Sessions may have multiple runs because work can be resumed, retried, cancelled, or continued after user input.

### Message And Attachment

Messages are immutable conversation records authored by a person, agent, or system. Attachments are structured references rather than pasted strings. Initial attachment types are agents, repository files, symbols, skills, and task threads.

### Channel Memory

Channel memory is a compact set of durable facts promoted through an impact gate. Every memory item records provenance, status, timestamps, and supersession. Raw messages and execution history remain available and are never destructively replaced by summaries.

## Primary Workflows

### Start Work From A Channel

1. The user writes a channel message and mentions an agent.
2. Toma stores the message and its structured attachments.
3. Toma creates a task thread linked to the channel message.
4. Toma creates or resumes an isolated session for the mentioned agent.
5. Toma assembles context and starts a run through the selected runner.
6. Progress and agent messages appear live in the task thread.

### Add A Collaborating Agent

1. The user or an authorized agent mentions another agent inside a task thread.
2. Toma attaches a new isolated session to that thread.
3. The collaborator receives shared task context, not another session's private state.
4. Agent-to-agent discussion remains visible in the thread.
5. Toma enforces concurrency, handoff, permission, and write-lock rules.

### Begin Code Work

1. A task may begin with reading, planning, or discussion against the main checkout.
2. Before the first repository mutation, Toma creates a temporary worktree and branch owned by the task thread.
3. All agent sessions in that thread use the same task worktree.
4. A single-writer lock prevents simultaneous repository mutations.
5. Read-only agent activity may continue concurrently when safe.

### Complete And Preserve Work

1. The agent reports completion and Toma records the final run state.
2. The user can inspect the conversation, diff, branch, commits, and unresolved items.
3. Toma cleans up a worktree only after the work is safely preserved according to policy.
4. Dirty, failed, blocked, cancelled, uncommitted, unmerged, or unapproved work is retained.

### Promote Durable Context

1. Toma evaluates completed or materially changed work through an impact gate.
2. Durable decisions, constraints, conventions, and repository facts may be proposed for channel memory.
3. Promoted facts retain links to their source messages and task threads.
4. Newer facts supersede older facts without deleting history.
5. Secrets and sensitive execution data are never summarized into memory.

## Task Lifecycle

Threads, sessions, and runs use explicit persisted states:

- `queued`
- `reading`
- `working`
- `waiting_for_input`
- `blocked`
- `completed`
- `failed`
- `cancelled`

State changes are durable events. Terminal runs do not restart implicitly; a retry or continuation creates a new run or an explicit resume transition.

## Context Assembly

Before a run starts or resumes, Toma assembles context in this order:

1. Workspace rules and repository constraints
2. The selected agent definition and permissions
3. Compact channel memory
4. Relevant channel messages and structured attachments
5. The complete current task thread
6. Run-specific instructions and current repository state

Context assembly must be inspectable. The user should be able to understand why a fact or instruction was included.

## Interface

Toma is a persistent macOS application with a keyboard-first, Slack-like shell:

- A narrow workspace rail
- A channel and direct-context sidebar
- A central channel conversation pane
- A collapsible task-thread pane
- A multiline composer with structured mentions and an attachment tray
- Visible task, session, and run status near the relevant conversation

The first useful screen opens directly into the active workspace rather than a landing page. Timelines should support large histories without becoming sluggish.

### Composer Behavior

- `Enter` sends the message.
- `Shift+Enter` inserts a newline.
- Drafts are preserved separately for every channel and task thread.
- Mention autocomplete searches agents, repository files, symbols, skills, and task threads through one palette.
- Sending, opening a thread, and returning to a draft should preserve predictable keyboard focus.

## System Boundaries

The UI invokes application commands and observes domain events. It does not call agent CLIs, Git, or SQLite directly.

The product is divided conceptually into:

- Application bootstrap and lifecycle
- Native UI and design system
- Domain models, commands, events, and state transitions
- SQLite persistence and migrations
- Background jobs, cancellation, recovery, and event delivery
- Context assembly and durable memory
- Git worktree and branch lifecycle
- Provider-neutral runner contract
- Codex CLI adapter
- Claude Code CLI adapter

The UI framework is an implementation detail behind an isolated boundary because the preferred GPUI ecosystem is pre-1.0 and may change.

## Persistence

SQLite is the local source of truth and should use write-ahead logging. Toma persists at least:

- Workspaces and repository identity
- Channels and people
- Agent definitions and versions
- Messages and structured attachments
- Task threads and lifecycle events
- Sessions and runs
- Raw provider input, output, and event history
- Worktree and branch ownership
- Skills and skill versions
- Channel memory, provenance, status, and supersession
- Drafts and lightweight UI state

Background work must be cancellable and recoverable after an application restart. Toma never destructively compresses or deletes raw conversation or execution history as part of normal summarization.

## Runner Contract

Toma uses a provider-neutral runner contract so execution backends can change without leaking provider behavior into the domain or UI. A runner must support:

- Capability discovery
- Starting and resuming an isolated session
- Streaming structured events and text output
- Receiving follow-up input
- Requesting user input or permission
- Cancellation
- Recoverable failure reporting
- Exporting raw history for durable storage

The first runner adapters target Codex CLI and Claude Code CLI. Missing capabilities should be represented explicitly rather than silently emulated.

## Git And Worktree Safety

- One task thread owns at most one active worktree and branch.
- Multiple sessions may use the same task worktree.
- Repository mutations require a single-writer lock owned by a session or operation.
- Worktree creation is lazy and occurs before the first mutation.
- Cleanup requires proof that valuable work is preserved.
- Toma retains dirty or ambiguous work and tells the user why it was retained.
- Failure recovery must prefer extra retained state over accidental data loss.

## Skills And Permissions

Skills are reviewed, versioned `SKILL.md` resources attached to a workspace or agent definition. A run records the exact skill version it received.

Permissions are enforced by Toma and surfaced at the point of action. Agents may request broader access, but they cannot grant it to themselves. Agent-to-agent delegation follows the same permission and concurrency rules as user dispatch.

## MVP Scope

The first runnable milestone should prove the product model rather than every integration. It includes:

- Creating or opening one repository-backed workspace
- Navigating channels and task threads
- Sending multiline channel and thread messages
- Mentioning a predefined agent to create a durable task thread
- Distinct persisted thread, session, and run records
- Visible lifecycle states and transitions
- A provider-neutral runner boundary with one useful adapter path and placeholders for the other
- Structured mentions for agents and repository files
- SQLite persistence for the core records and raw history
- Lazy task worktree ownership with safe retention rules
- A quiet, dense native shell with the workspace rail, channel sidebar, conversation pane, and collapsible thread pane
- Restart recovery for persisted conversations and nonterminal work

## Not In The First Milestone

- Cloud synchronization or multi-user collaboration
- Mobile or web clients
- A hosted agent execution service
- Marketplace distribution
- Automatic merging into the user's primary branch
- Fully autonomous agent-to-agent task graphs
- Destructive history pruning
- Broad plugin or skill marketplaces

## Success Criteria

The MVP is successful when a developer can open a repository, assign a task to an agent from a channel, follow the work in a durable thread, add a second agent as a visible collaborator, restart Toma without losing state, and inspect or recover every repository change without relying on hidden terminal sessions.

The product should make simultaneous agent work feel understandable and controlled. The user should always know who is working, in which context, against which branch, with what permissions, and what requires attention next.

## Distribution

The initial distribution target is a personal Homebrew Cask backed by GitHub releases for Apple Silicon and Intel Macs. Early builds may be unsigned and unnotarized, with clear documentation for the one-time Gatekeeper approval required to launch them. Publishing or updating an external tap is a separate release action and is not part of ordinary development work.
