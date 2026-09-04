# Confer v0.1 Specification

## Product definition

Confer is a local MCP server that lets the current coding agent consult, coordinate, and resume other installed coding agents without copying text between terminal windows. It is a standalone Rust binary and has no dependency on Recall, Orca, or a daemon.

![Confer architecture](confer-architecture.svg)

## Supported participants and hosts

The supported products can act as external room participants and as MCP hosts:

| ID | Product | Participant command | MCP registration |
| --- | --- | --- | --- |
| `claude` | Claude Code | `claude` | native `claude mcp` command |
| `codex` | Codex | `codex exec` | native `codex mcp` command |
| `cursor` | Cursor Agent | `agent` or `cursor-agent` | `~/.cursor/mcp.json` |
| `grok` | Grok Build | `grok` | native `grok mcp` command |
| `agy` | Antigravity CLI | `agy` | native `agy mcp` command |

MCP is the public protocol. Headless commands and native streaming protocols remain adapter internals.

## Room model

A room belongs to one normalized workspace. In a Git repository, the workspace is the canonical result of `git rev-parse --show-toplevel`. Different Git worktrees are different workspaces. Outside Git, the canonical current directory is the workspace.

The current MCP host is a room member and moderator. The default room size is three members including the current host, so the usual default is two external seats. A caller may request another size or explicit seats. The same agent type may occupy multiple seats, with the same or different models.

A room is the task container. Active rooms may add seats as new roles become useful and retire seats whose role is complete. Retiring a seat preserves its metadata and native session mapping but permanently removes it from direct, multicast, and broadcast addressing.

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

Confer stores disposable room metadata and advisory seat lease files:

```text
~/.confer/rooms.json
~/.confer/seat-locks/*.lock
```

`rooms.json` contains a schema version and room records with:

- room ID, name, workspace root, status, and timestamps;
- current host identity when known;
- external seat identity and selection fields;
- external seat active or retired status;
- native agent session ID and adapter recovery fields when a session has started.

These files do not contain message bodies, agent replies, pending delivery state, full transcripts, tool calls, thinking, or code snapshots. Seat lease files contain no semantic state. Native agent stores remain the source of truth for conversation history.

Room metadata writes use a short advisory lock and atomic replacement. Current writes use schema version 2; version 1 remains readable and upgrades on the next mutation, while unknown newer versions fail closed. Removing the disposable room cache resets Confer discovery without deleting native agent sessions.

## Readiness and selection

Readiness checks are local and run every time a room is created or resumed. They inspect the executable and local authentication or configuration state without calling a model or checking quota. A positive result means `locally_ready`; it does not guarantee provider availability, model access, or remaining quota.

The current host, guided by the Skill, normally selects seat specifications from the task. Explicit user choices take precedence. When the host supplies no seats, Confer fills the requested size from locally ready supported agents.

If a requested participant is unavailable, Confer may replace it with another locally ready supported agent. The response must report the requested seat, replacement, and reason. A logical seat survives replacement and keeps its name and authorized room view. A replacement never receives another seat’s private messages or replies.

## Session lifecycle

`create_room` creates logical seats only. It performs no model call and does not start empty agent sessions.

The first queued message to an external seat creates its native session and records the native session ID. Every seat has one in-process FIFO worker. A sent message enters that worker, starts promptly when the seat is idle, and remains `queued` while an earlier delivery runs. Different seats may run concurrently. A cross-process file lease serializes workers from separate MCP processes, but FIFO ordering is guaranteed only within one live MCP process. Queue processing does not retry a native message after dispatch may have begun.

Delivery tracking, pending Queue messages, and workers exist only in the live MCP process. If that process exits, queued messages that have not started are lost, unfinished outputs and delivery IDs are lost, its lease is released, and native work may have continued. Resuming the room restores native session addressing, not pending messages or in-flight certainty. The caller must verify uncertain native work before sending that seat another message. Confer never automatically redelivers an uncertain message because the first execution may have changed code.

## Message visibility

Each seat has a private native session, sees only addressed messages, and returns replies to the current host. Multicast and broadcast send independent copies to selected or all external seats; seats see neither the room transcript nor peer replies. Only the host can relay, so Confer never starts an unbounded agent-to-agent loop.

## MCP tools

The public MCP surface contains eight tools.

### `create_room`

Creates a room for the current workspace.

Input:

- `name`: optional human-readable room name;
- `target_size`: optional total member count including the current host, default `3`;
- `host_agent`: optional current host ID when automatic detection is unavailable;
- `seats`: optional external seat specifications.

Output includes the room ID, normalized workspace, roster, readiness results, and any replacements. It never sends a task.

### `add_seat`

Adds one private seat to an active room in the current workspace. The input uses the same agent, model, effort, name, and instruction fields as room creation. Existing and retired seat names remain reserved. Output includes the updated room, readiness results, and any replacement.

### `retire_seat`

Retires one seat by name or ID in an active room in the current workspace. A seat with a known running delivery returns `seat_busy`. Retirement preserves the native session mapping but is irreversible.

### `list_rooms`

Lists active and inactive rooms. `scope` defaults to `current`; `all` returns metadata for every recorded workspace. Discovery does not allow `send_message`, `resume_room`, `add_seat`, or `retire_seat` to cross workspace boundaries.

Output includes room ID, name, status, participants, native-session availability, and timestamps. It never returns message content.

### `send_message`

Sends one message to one seat, selected seats, or all external seats.

Input:

- `room_id`;
- `recipients`: one or more seat names or IDs, or `*` for broadcast;
- `message`.

The send returns one new `delivery_id` per recipient plus immediate acceptance or readiness errors. Accepted deliveries may be `queued` or `running`, and the caller uses `wait_output` for completion.

### `wait_output`

Waits for specified deliveries or the room’s current live deliveries, with an optional timeout.

Output contains each delivery’s `queued`, `running`, `completed`, or `failed` status, final assistant answer, and error. It does not expose thinking, token deltas, or intermediate tool events. A timeout returns completed results and current non-terminal statuses without cancelling them.

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

`confer skill install` embeds the [canonical Skill](../skills/confer/SKILL.md) and delegates target paths, conflict protection, updates, scope, and dry-run reporting to Kitup. User scope is the default.

`confer mcp install` follows each host’s supported registration mechanism. Repeated installation updates the Confer-owned registration without deleting unrelated MCP entries. Uninstall removes only the `confer` entry.

## Adapter contract

Every adapter must:

- resolve its supported executable without scanning unrelated agents;
- perform a local readiness check without a model call;
- create a native session on first send and capture its stable ID;
- resume exactly that session for later messages;
- pass the selected model and reasoning effort when supported;
- run in the room’s stored workspace root;
- extract the final assistant answer from machine-readable output;
- preserve stderr for actionable errors without leaking credentials, and return native failures without retry or fallback.

Confer owns the FIFO Queue above every adapter. Each queued delivery runs one native agent process and resumes the seat's recorded native session when one exists.

Model and reasoning fields are requests to the native CLI. An unsupported value must produce a clear adapter error rather than silently selecting another model.

## Permissions and filesystem

Confer uses the room workspace as each child process working directory. It does not create filesystem isolation. Independent seats may therefore read or modify the same files even when their messages are isolated.

Confer launches every seat with that agent's full-permission setting so a non-interactive process is never blocked on an approval prompt it cannot answer: Claude and Antigravity receive `--dangerously-skip-permissions`; Codex receives `--dangerously-bypass-approvals-and-sandbox`; Cursor receives `--trust --force`; and Grok receives `--permission-mode bypassPermissions`. Seats therefore run with the same authority as the current host and without sandbox isolation. Explicit task instructions remain the only limit on what a seat is asked to do.

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
- `confer mcp capabilities` reports exactly the eight room tools.
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
