# Confer Design

## Product definition

Confer is a local MCP server for running and resuming installed coding agents. A coding agent or another application can integrate through MCP and manage task decomposition, relays, and result acceptance. It is a standalone Rust binary and has no dependency on Recall, Orca, or a daemon.

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

MCP is the public protocol. Every seat uses an ACP v1 lifecycle internally. Cursor and Grok use native ACP over stdio; Codex uses an in-process ACP bridge to its app-server; Claude and Antigravity use in-process ACP bridges to their native headless commands. Confer ships one Rust binary and requires no separate bridge runtime.

## Room model

A room belongs to one normalized workspace supplied explicitly by the host task, never inferred from the MCP process working directory. The input must be an absolute path to an existing directory. Creation and current-workspace discovery normalize a Git directory to the canonical result of `git rev-parse --show-toplevel`. Different Git worktrees are different workspaces. Outside Git, the canonical supplied directory is the workspace.

The host verifies the returned root against its own task before using it for subsequent room calls. Existing-room operations canonicalize the supplied root and require it to match the stored room workspace; they do not rediscover a Git root or retarget the room. Symlink aliases of the same root are accepted. Workspace matching prevents accidental cross-project routing, not filesystem access by a malicious caller.

Workspace discovery and participant processes both ignore inherited `GIT_DIR` and `GIT_WORK_TREE`. All participant transports start in the verified room root; changing the MCP server's launch environment cannot redirect these Git location variables into a different project.

The host manages the work and is not an execution seat. In coding-agent workflows, the host acts as the management agent; applications may implement that role in code. At creation, `target_size` requests execution seats only. It must be positive when supplied. There is no default count or fixed seat limit: provide `target_size`, at least one explicit seat, or both. Creation uses the larger of `target_size` and the explicit seat count, and `add_seat` may grow the room later.

A seat has an independent ID, room address, and native session mapping. Agent, model, and reasoning effort are configuration fields, not a unique identity. Multiple seats may use identical configurations, and the number of seats is not limited by the number of installed agent types.

A room is a coordination context whose reuse is decided by the caller. Workspace matching does not imply automatic reuse. The bundled Skill guides coding-agent hosts to reuse a room from their current session context and to discover historical rooms only when the user asks to continue one; applications may supply their own workflow.

Rooms may add seats as new roles become useful and retire seats whose role is complete. Retiring a seat preserves its metadata and native session mapping but permanently removes it from direct, multicast, and broadcast addressing.

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

`name` is a room address, not a built-in role. `instructions` are visible only to that seat. Every delivery includes those stable instructions and the current message, including when resuming a session whose first prompt failed. A recorded native session ID does not imply that instructions were delivered. Confer generates a unique seat name when none is supplied.

Rooms have no lifecycle status, automatic expiration, or garbage collection. A persisted room remains addressable by ID in its workspace. Starting a new room does not change or delete earlier rooms.

## Local state

Confer stores disposable room metadata and advisory seat lease files:

```text
$XDG_STATE_HOME/confer/rooms.json
$XDG_STATE_HOME/confer/rooms.json.lock
$XDG_STATE_HOME/confer/seat-locks/*.lock
```

On macOS and Linux, `XDG_STATE_HOME` must be an absolute path. An unset, empty, or relative value falls back to `~/.local/state`. The legacy `~/.confer` directory is neither read nor migrated; existing files remain untouched. Restart all Confer MCP processes after upgrading. Native agent sessions remain in their original stores.

`rooms.json` contains a schema version and room records with:

- room ID, name, workspace root, and timestamps;
- originating host identity when known;
- external seat identity and selection fields;
- external seat active or retired status;
- native agent session ID and adapter recovery fields when a session has started.

These files do not contain message bodies, agent replies, pending delivery state, full transcripts, tool calls, thinking, or code snapshots. Seat lease files contain no semantic state. Native agent stores remain the source of truth for conversation history.

Room metadata writes use a short advisory lock and atomic replacement. Current writes use schema version 3; versions 1 and 2 remain readable and normalize on the next mutation, while unknown newer versions fail closed. Removing the disposable room cache resets Confer discovery without deleting native agent sessions.

## Readiness and selection

Readiness checks are local and run when a room is created, when a seat is added, and before each delivery starts. They inspect the executable and local authentication or configuration state without calling a model or checking quota. A positive result means `locally_ready`; it does not guarantee provider availability, model access, or remaining quota.

Creation and seat addition validate the final selected agent's deterministic configuration before saving any room change. Delivery uses the same validation. Malformed Cursor model options, conflicting option sources, and unsupported local effort values fail immediately. Model availability and provider-specific capabilities remain native runtime checks. Cursor accepts `reasoning_effort` without `model` and applies it to its configured default model; a default that does not support that effort returns a native error.

The host may specify all, some, or none of the seats. For unspecified agents and remaining target positions, Confer cycles through locally ready supported agents, placing agent types other than the known host first. Agent types may repeat; the host's agent type remains eligible. Explicit agent choices are honored.

If an explicitly requested agent is unavailable, creation or seat addition fails with the agent, seat, and readiness reason. The operation does not modify room state. Confer does not choose a replacement or discard the requested model and effort. The caller decides whether to submit a new request with another agent; personal Skills may guide that decision.

## Session lifecycle

`create_room` creates logical seats only. It performs no model call and does not start empty agent sessions.

The first queued message to an external seat creates its native session and records the native session ID. Every seat has one in-process FIFO worker. A sent message enters that worker, starts promptly when the seat is idle, and remains `queued` while an earlier delivery runs. Different seats may run concurrently. A cross-process file lease serializes workers from separate MCP processes, but FIFO ordering is guaranteed only within one live MCP process. Queue processing does not retry a native message after dispatch may have begun.

Delivery tracking, pending Queue messages, and workers exist only in the live MCP process. If that process exits, queued messages that have not started are lost, unfinished outputs and delivery IDs are lost, its lease is released, and native work may have continued. Persisted seat metadata keeps native session addressing, but not pending messages or in-flight certainty. The caller must verify uncertain native work before sending that seat another message. Confer never automatically redelivers an uncertain message because the first execution may have changed code.

## Message visibility

Each seat has a private native session, sees only addressed messages, and returns replies to the current host. Multicast and broadcast send independent copies to selected or all external seats; seats see neither the room transcript nor peer replies. Only the host can relay, so Confer never starts an unbounded agent-to-agent loop.

## MCP tools

The public MCP surface contains six tools. Workspace-scoped calls require an explicit `workspace`; `list_rooms` with `scope: all` is the only exception. Callers must update their arguments and bundled Skill together, restart the MCP connection, and refresh its tool schemas. Older callers that counted the host in target_size must now pass the desired execution seat count. Creation without target_size or explicit seats is rejected, and create_room and add_seat no longer return replacements. The room cache format is unchanged, and existing rooms are not automatically moved to another workspace.

### `create_room`

Creates a room for the host task's supplied workspace.

Input:

- `workspace`: required absolute directory from the current host task;
- `name`: optional human-readable room name;
- `target_size`: optional positive execution seat count, excluding the host; no default or fixed upper bound;
- `host_agent`: optional current host ID when automatic detection is unavailable;
- `seats`: optional external seat specifications.

Output includes the room ID, normalized workspace, roster, and readiness results. It never sends a task.

### `add_seat`

Adds one private seat using `room_id`, the host-verified workspace root in `workspace`, and `seat`. The seat uses the same agent, model, effort, name, and instruction fields as room creation. Existing and retired seat names remain reserved. Output includes the updated room and readiness results.

### `retire_seat`

Retires one seat by name or ID using `room_id`, `seat`, and the host-verified workspace root in `workspace`. A busy seat returns `seat_busy`. Retirement preserves the native session mapping but is irreversible.

### `list_rooms`

Lists rooms. `scope` defaults to `current`, which requires the host task's absolute directory in `workspace` and normalizes it like creation. `all` returns metadata for every recorded workspace without requiring a workspace or inspecting the MCP process working directory. The bundled Skill uses this when the user explicitly asks to recover an earlier room; other callers decide their own discovery workflow. Discovery does not authorize operating on rooms outside the host task's workspace.

Output includes room ID, name, participants, native-session availability, and timestamps. It never returns message content.

### `send_message`

Sends one message to one seat, selected seats, or all external seats.

Input:

- `room_id`;
- `workspace`: the host-verified workspace root;
- `recipients`: one or more seat names or IDs, or `*` for broadcast;
- `message`.

The send returns one receipt and new `delivery_id` per recipient plus immediate acceptance or readiness errors. The receipt does not include delivery status; the caller uses `wait_output` to observe `queued`, `running`, `completed`, or `failed` state.

### `wait_output`

Waits for specified deliveries or the room’s current live deliveries. Requires `room_id` and the host-verified workspace root in `workspace`. `timeout_ms` defaults to `120000`, accepts `0` for an immediate snapshot, and is capped at `600000`.

Output contains each delivery’s `queued`, `running`, `completed`, or `failed` status, final assistant answer, and error. It does not expose thinking, token deltas, or intermediate tool events. A timeout returns completed results and current non-terminal statuses without cancelling them.

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

MCP and Skill installation are deliberately independent. `confer mcp install` never installs the Skill, and `confer skill install` never changes MCP configuration. Both installation commands support Claude Code, Codex, Cursor, Grok, and Antigravity CLI.

`confer skill install` embeds the [canonical Skill](../skills/confer/SKILL.md) and delegates target paths, conflict protection, updates, scope, and dry-run reporting to Kitup. User scope is the default.

`confer mcp install` follows each host’s supported registration mechanism. Repeated installation updates the Confer-owned registration without deleting unrelated MCP entries. Uninstall removes only the `confer` entry.

Explicit `--agent cursor` registration and removal edit Cursor's MCP configuration without requiring the participant CLI on `PATH`. Default selection and `--agent '*'` still discover installed host executables; they do not create Cursor configuration on machines without its CLI. Other hosts require their native registration command. Registration does not establish participant readiness or authentication.

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

Confer owns the FIFO Queue above every adapter. Each queued delivery opens one ACP connection, runs one native agent process, and resumes the seat's recorded native session when one exists. Session history replay is excluded from the current answer. After a terminal response, Confer closes the connection and reaps its child; a child that remains alive after three seconds is terminated. There is no idle process pool.

Cursor seats use its ACP session store. Old headless Cursor session IDs are not migrated and require new seats. Other native session stores are not rewritten. An observed session ID remains available when the prompt fails; a missing or stale ID never triggers silent replacement.

Model and reasoning fields are requests to the native CLI. An unsupported value must produce a clear adapter error rather than silently selecting another model.

## Permissions and filesystem

Confer uses the room workspace as each child process working directory. It does not create filesystem isolation. Independent seats may therefore read or modify the same files even when their messages are isolated.

Confer launches every seat with that agent's full-permission setting so a non-interactive process is never blocked on an approval prompt it cannot answer: Claude and Antigravity receive `--dangerously-skip-permissions`; Codex receives app-server `approvalPolicy: never` and the full-access sandbox policy; Cursor receives `--trust --force`; and Grok receives `--always-approve` and ACP `yoloMode`. Seats therefore run with the authority of the local Confer process and without sandbox isolation. Explicit task instructions remain the only limit on what a seat is asked to do.

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
