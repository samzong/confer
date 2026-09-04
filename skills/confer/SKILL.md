---
name: confer
description: Coordinate Claude Code, Codex, Cursor Agent, Grok, and Antigravity CLI through private local MCP rooms. Trigger when the user asks to consult another agent, have multiple agents investigate independently, divide planning and implementation, compare answers, or request an independent review without copying between terminals.
---

# Confer

Create a room only when another coding agent can materially improve the task, not for a routine question the current agent can answer and verify directly.

Confer MCP owns room metadata, native session routing, and message delivery. Never invoke participant CLIs directly or create another room state file.

## Create the Room

Honor explicit choices for agent, model, reasoning effort, participant count, role, and independence. When choices are open, select seats from the task:

- planning and architecture need a strong reasoning model and instructions that forbid edits;
- implementation needs the agent best suited to the codebase and explicit authority boundaries;
- review needs a private session that has not seen the authoring answer, and research comparisons need blind seats before any critique round.

The default target size is three including the host. Give seats short unique names and private instructions. Treat one room as one task: add a new seat when a later phase needs another role, and retire a seat only after its delivery finishes and its role is complete.

## Keep Seats Private

For independent work, send the same prompt separately or by multicast and wait for every required answer before sharing any of them.

Do not include another seat's answer in a follow-up unless the user asked for critique, debate, handoff, or collaboration. When forwarding, say whose result is being forwarded and what the recipient should do with it.

The host owns every relay. Do not create an automatic discussion loop.

## Use the Tools

Use this sequence when starting new work:

1. Call create_room with explicit seat specifications when the task implies them.
2. Call send_message for the task. Use selected recipients for private work and * only for a deliberate broadcast.
3. Call wait_output for the returned delivery IDs.
4. Send targeted follow-ups or relays only when needed. Follow-ups to a busy seat join that seat's queue.
5. Add or retire seats as task roles change without creating a replacement room.
6. Synthesize one answer for the user.
7. Call close_room when the task is complete. The room remains resumable.

Use list_rooms with current scope when the user refers to an earlier room in the current repository, and all scope only when cross-workspace discovery is necessary. Use resume_room with the selected room ID before sending more messages.

Messages may run concurrently across different seats. Each seat has one FIFO queue. Confer reports native failures without retry.

After an MCP restart, pending Queue messages are lost and unfinished deliveries are uncertain. Verify native work before sending another message; never infer that the released lease means the previous agent stopped.

## Report the Result

Return a single conclusion, not a transcript dump. Preserve important disagreements and identify which evidence resolves them.

Report unavailable or replaced participants, failed deliveries, and timeouts. Do not credit an incomplete delivery.

Confer returns final answers only. Verify code, tests, commands, and repository state directly before presenting an agent claim as fact.

## Preserve User Authority

Room creation does not expand permission to edit files, create worktrees, commit, push, release, or contact external systems. Give every seat instructions consistent with the user's current authorization.

Confer does not isolate filesystems. Blind seats can still inspect the same repository, so message independence does not prove independent code state.

If the Confer MCP tools are unavailable, say that the MCP registration is missing and offer confer mcp install. Do not substitute copied terminal commands as if a room existed.
