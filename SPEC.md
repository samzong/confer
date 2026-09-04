# Confer v0.1 Specification

## Product definition

Confer is a local MCP server that lets the current coding agent consult, coordinate, and resume other installed coding agents without copying text between terminal windows. It is a standalone Rust binary and has no dependency on Recall, Orca, or a daemon.

The binary owns transport, native session recovery, local room metadata, host installation, and bundled Skill installation. The bundled Skill teaches the current agent when and how to use the MCP tools. It owns task interpretation and collaboration policy, not transport or persistence.

Typical requests include:

- “Ask Claude to think this through with you.”
- “Have several agents research this independently, then give me one conclusion.”
- “Let Codex plan, Grok implement, and Cursor review.”
- “Give the same problem to two agents without showing either one the other answer.”

## Goals

- Expose a small MCP surface for temporary multi-agent rooms.
- Support Claude Code, Codex, Cursor Agent, Grok Build, and Antigravity CLI.
- Let the current host choose participants by agent, model, reasoning effort, name, and private instructions.
- Preserve each participant’s native session across MCP host restarts.
- Support direct, multicast, and broadcast messages without a Confer-owned durable queue.
- Keep independent participants blind to one another unless the current host explicitly relays a message.
- Install the MCP server and Skill into supported local hosts with predictable, separate commands.
- Remain usable from any subdirectory of the current Git worktree.

## Non-goals

- Confer does not provide a daemon, scheduler, autonomous discussion loop, or background worker.
- Confer does not manage worktrees, branches, commits, merges, permissions, or sandboxes.
- Confer does not inspect provider quotas or make model calls during readiness checks.
- Confer does not persist messages, outputs, pending deliveries, tool events, thinking, or code snapshots.
- Confer does not expose agent discovery as a public MCP tool.
- Confer does not support OpenCode, Gemini CLI, DeepSeek, Kimi, or other agents in v0.1.
- Confer does not duplicate room operations as ordinary CLI subcommands.

## Supported participants and hosts

The supported products can act as external room participants and as MCP hosts:

| ID | Product | Participant command | MCP registration |
| --- | --- | --- | --- |
| `claude` | Claude Code | `claude` | native `claude mcp` command |
| `codex` | Codex | `codex exec` | native `codex mcp` command |
| `cursor` | Cursor Agent | `agent` or `cursor-agent` | `~/.cursor/mcp.json` |
| `grok` | Grok Build | `grok` | native `grok mcp` command |
| `agy` | Antigravity CLI | `agy` | native `agy mcp` command |

Confer may use different transport details for each adapter. MCP is the public protocol. Headless commands and any native streaming protocol remain adapter internals.

## Room model

A room belongs to one normalized workspace. In a Git repository, the workspace is the canonical result of `git rev-parse --show-toplevel`. Different Git worktrees are different workspaces. Outside Git, the canonical current directory is the workspace.

The current MCP host is a room member and moderator. The default room size is three members including the current host, so the usual default is two external seats. A caller may request another size or explicit seats. The same agent type may occupy multiple seats, with the same or different models.

Each external seat has these optional selection fields:

```json
{
  "agent": "codex",
  "model": "gpt-5.6-sol",
  "reasoning_effort": "high",
  "name": "planner",
  "instructions": "Design the change. Do not edit files."
}
```

`name` is a room address, not a built-in role. `instructions` are visible only to that seat and remain part of its native session context. Confer generates a unique seat name when none is supplied.

Rooms are `active` or `inactive`. Closing a room marks it inactive. It does not delete room metadata or native sessions. Resuming an inactive room makes it active again. There is no automatic expiration or garbage collection.

## Local state

Confer stores one disposable cache file:

```text
~/.confer/rooms.json
```

The file contains a schema version and room records with:

- room ID, name, workspace root, status, and timestamps;
- current host identity when known;
- external seat identity and selection fields;
- native agent session ID and adapter recovery fields when a session has started.

The file does not contain message bodies, agent replies, pending delivery state, full transcripts, tool calls, thinking, or code snapshots. Native agent stores remain the source of truth for conversation history.

Writes use a short advisory lock and atomic replacement. `rooms.json` is the only durable Confer state. Deleting it resets Confer room discovery without deleting any native agent session.

## Readiness and selection

Readiness checks are local and run every time a room is created or resumed. They inspect the executable and local authentication or configuration state without calling a model or checking quota. A positive result means `locally_ready`; it does not guarantee provider availability, model access, or remaining quota.

The current host, guided by the Skill, normally selects seat specifications from the task. Explicit user choices take precedence. When the host supplies no seats, Confer fills the requested size from locally ready supported agents.

If a requested participant is unavailable, Confer may replace it with another locally ready supported agent. The response must report the requested seat, replacement, and reason. A logical seat survives replacement and keeps its name and authorized room view. A replacement never receives another seat’s private messages or replies.

## Session lifecycle

`create_room` creates logical seats only. It performs no model call and does not start empty agent sessions.

The first message to an external seat creates its native session and records the native session ID. Concurrent sends during that one-time initialization wait only until the native session is addressable so they cannot split into different sessions. Later messages target that same native session without waiting for earlier results. Confer does not impose a busy check, durable queue, rate limit, ordering layer, or automatic retry. Native rejection is returned as an error for that delivery.

Delivery tracking exists only in the live MCP process. If that process exits, unfinished outputs and delivery IDs are lost. Resuming the room restores native session addressing, not in-flight work. Confer never automatically redelivers an uncertain message because the first execution may have changed code.

## Message visibility

Each seat has a private native session and receives only messages addressed to it. A multicast sends the same message independently to selected seats. A broadcast sends it to all external seats. Replies return only to the current host.

Seats do not see the room transcript or one another’s replies by default. The current host can explicitly forward all or part of a reply to one or more seats. Blind parallel research, implementation, and review therefore remain independent unless the host chooses to create a critique round.

Confer never starts an unbounded agent-to-agent loop. The current host decides every relay and follow-up.

## MCP tools

The v0.1 public MCP surface contains six tools.

### `create_room`

Creates a room for the current workspace.

Input:

- `name`: optional human-readable room name;
- `target_size`: optional total member count including the current host, default `3`;
- `host_agent`: optional current host ID when automatic detection is unavailable;
- `seats`: optional external seat specifications.

Output includes the room ID, normalized workspace, roster, readiness results, and any replacements. It never sends a task.

### `list_rooms`

Lists active and inactive rooms for the current normalized workspace only. It accepts no cross-workspace option.

Output includes room ID, name, status, participants, native-session availability, and timestamps. It never returns message content.

### `send_message`

Sends one message to one seat, selected seats, or all external seats.

Input:

- `room_id`;
- `recipients`: one or more seat names or IDs, or `*` for broadcast;
- `message`.

Output returns one `delivery_id` per recipient plus immediate adapter acceptance or error details. `session_pending` means the agent process is live but first-session addressing exceeded the readiness window; the caller must use `wait_output` for that delivery. Calls are non-blocking after native session addressing is established.

### `wait_output`

Waits for specified deliveries or the room’s current live deliveries, with an optional timeout.

Output contains each delivery’s status, final assistant answer, and error. It does not expose thinking, token deltas, or intermediate tool events. A timeout returns completed results and marks the remaining deliveries as running without cancelling them.

### `resume_room`

Reactivates one room by `room_id`, rechecks local readiness, and restores adapter addressing from native session metadata. It returns the current roster, unavailable sessions, and replacements. It does not make a model call.

### `close_room`

Marks one room inactive. It does not kill dispatched agent work, delete native sessions, delete cached room metadata, or revert code changes.

## CLI surface

The CLI exists to serve and install the MCP and bundled Skill:

```text
confer mcp
confer mcp capabilities
confer mcp install [--agent <id>]... [--dry-run] [--bin <path>]
confer mcp uninstall [--agent <id>]... [--dry-run]
confer skill install [--scope user|project] [--agent <id>]... [--dry-run] [--yes]
```

`confer mcp` serves stdio MCP. Room operations are not exposed as ordinary CLI commands.

MCP and Skill installation are deliberately independent. `confer mcp install` never installs the Skill, and `confer skill install` never changes MCP configuration. MCP installation auto-detects Claude Code, Codex, Cursor, Grok, and Antigravity CLI. Skill installation auto-detects only Claude Code, Codex, Cursor, and Grok.

`confer skill install` embeds the canonical Skill bundle and delegates ownership, conflict protection, update behavior, target paths, scope handling, and dry-run reporting to Kitup. User scope is the default.

`confer mcp install` follows each host’s supported registration mechanism. Repeated installation updates the Confer-owned registration without deleting unrelated MCP entries. Uninstall removes only the `confer` entry.

## Skill behavior

The bundled `confer` Skill is eligible for implicit invocation. It recognizes natural requests to consult another agent, ask several agents independently, divide planning and implementation, or request an independent review.

The Skill:

- decides whether another agent or a room is useful;
- honors explicit agent, model, effort, count, role, and independence requests;
- chooses `agent`, `model`, `reasoning_effort`, `name`, and private `instructions` when the user leaves them open;
- counts the current host in the default room size of three;
- preserves blind independence by sending separate messages and withholding peer answers;
- uses explicit relays for critique or handoff;
- waits only for the outputs needed for the user’s request;
- synthesizes one answer and reports unavailable, replaced, failed, or timed-out seats;
- closes the room when the immediate task is complete while leaving it resumable;
- never expands mutation authority, creates worktrees, commits, merges, or changes permissions on its own.

The Skill always uses the six MCP tools. It does not invoke participant CLIs directly or implement its own room state.

## Adapter contract

Every adapter must:

- resolve its supported executable without scanning unrelated agents;
- perform a local readiness check without a model call;
- create a native session on first send and capture its stable ID;
- resume exactly that session for later messages;
- pass the selected model and reasoning effort when supported;
- run in the room’s stored workspace root;
- extract the final assistant answer from machine-readable output;
- preserve stderr for actionable error reporting without leaking credentials;
- return native failures without hidden retry or fallback.

Model and reasoning fields are requests to the native CLI. An unsupported value must produce a clear adapter error rather than silently selecting another model.

## Permissions and filesystem

Confer uses the room workspace as each child process working directory. It does not create filesystem isolation. Independent seats may therefore read or modify the same files even when their messages are isolated.

Confer launches every seat with that agent's full-permission flag so a non-interactive process is never blocked on an approval prompt it cannot answer: Claude and Antigravity receive `--dangerously-skip-permissions`, Codex `--dangerously-bypass-approvals-and-sandbox`, Cursor `--trust --force`, and Grok `--permission-mode bypassPermissions`. Seats therefore run with the same authority as the current host and without sandbox isolation. Explicit task instructions remain the only limit on what a seat is asked to do.

## Errors

Tool errors must identify the room, seat, agent, and failing operation when those values exist. Expected error classes include:

- unsupported or locally unavailable agent;
- missing or stale native session;
- invalid room for the current workspace;
- duplicate or unknown seat address;
- native CLI launch, parse, authentication, model, permission, or concurrency failure;
- unknown or expired in-memory delivery ID;
- timeout with partial results.

Confer must not reinterpret a failed send as success, silently create a replacement session for a stale native session, or retry a message that may have executed.

## v0.1 acceptance

The release is acceptable when all of the following are proven from a clean checkout:

- `make check` passes formatting, lint, tests, and dependency audit.
- `make install` installs the `confer` binary used by subsequent checks.
- `confer mcp capabilities` reports exactly the six room tools.
- `confer mcp install` installs or updates `confer` in detected MCP hosts without disturbing unrelated entries.
- `confer skill install` installs the Kitup-owned Skill into detected Claude Code, Codex, Cursor, and Grok hosts.
- Local readiness finds Claude Code, Codex, Cursor Agent, Grok, and Antigravity CLI without a model call.
- Real adapter smoke calls create and resume native sessions with final-output parsing.
- A newly initialized Git repository can use an installed host’s Confer MCP to create a room, send a real request to at least one external agent, wait for the answer, list and resume the room, and close it.
- Two external seats can receive the same prompt without seeing one another’s answers.
- Deleting `~/.confer/rooms.json` resets Confer discovery without deleting native sessions.
- The private GitHub repository contains the verified source, a signed-off v0.1.0 release commit and tag, and a GitHub v0.1.0 release.

## Release boundary

v0.1.0 ships one Rust application binary and the embedded Skill bundle. It does not publish a Rust library API or a crates.io package. The GitHub repository remains private until the owner chooses otherwise.

The first release prioritizes the local macOS environment used for acceptance. CI must still run the repository gate, and the release workflow must produce versioned binary archives for supported build targets configured by the project.
