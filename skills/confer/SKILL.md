---
name: confer
description: Coordinate Claude Code, Codex, Cursor Agent, Grok, and Antigravity CLI through private local MCP rooms. Trigger when the user asks to consult another agent, have multiple agents investigate independently, divide planning and implementation, compare answers, or request an independent review without copying between terminals.
---

# Confer

Use Confer when another coding agent can materially improve the current task. Do not create a room for a routine question that the current agent can answer and verify directly.

Confer MCP owns room metadata, native session routing, and message delivery. Never invoke participant CLIs directly or create another room state file.

## Create the Room

Honor every explicit user choice for agent, model, reasoning effort, participant count, role, and independence. When choices are open, select seats from the task:

- planning and architecture need a strong reasoning model and instructions that forbid edits;
- implementation needs the agent best suited to the current codebase and explicit authority boundaries;
- review needs a private session that has not seen the authoring answer;
- research comparisons should use blind seats before any critique round.

The default target size is three including the current host. Give seats short unique names and private instructions. create_room does not send the task, so send the actual request afterward.

## Keep Seats Private

Each seat sees only messages addressed to it. For independent work, send the same prompt separately or by multicast and wait for both answers before sharing either answer.

Do not include another seat's answer in a follow-up unless the user asked for critique, debate, handoff, or collaboration. When forwarding, say whose result is being forwarded and what the recipient should do with it.

The current host owns every relay. Do not create an automatic discussion loop.

## Use the Tools

Use this sequence when starting new work:

1. Call create_room with explicit seat specifications when the task implies them.
2. Call send_message for the task. Use selected recipients for private work and * only for a deliberate broadcast.
3. Call wait_output for the returned delivery IDs.
4. Send targeted follow-ups or relays only when needed.
5. Synthesize one answer for the user.
6. Call close_room when the immediate task is complete. The room remains resumable.

Use list_rooms when the user refers to an earlier room in the current repository. Use resume_room with the selected room ID before sending more messages.

Multiple messages may target the same seat without waiting. Confer passes them to the same native session and reports native failures without retry.

When send_message returns session_pending, the delivery is still live. Call wait_output instead of reporting the seat as rejected.

## Report the Result

Return a single conclusion, not a transcript dump. Preserve important disagreements and identify which evidence resolves them.

Report unavailable or replaced participants, failed deliveries, and timeouts. Never imply that an agent contributed when its delivery did not complete.

Confer returns final answers only. Verify code, tests, commands, and repository state directly before presenting an agent claim as fact.

## Preserve User Authority

Room creation does not expand permission to edit files, create worktrees, commit, push, release, or contact external systems. Give every seat instructions consistent with the user's current authorization.

Confer does not isolate filesystems. Blind seats can still inspect the same repository, so message independence does not prove independent code state.

If the Confer MCP tools are unavailable, say that the MCP registration is missing and offer confer mcp install. Do not substitute copied terminal commands as if a room existed.
