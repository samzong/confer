---
name: duo
description: Complete a discussion, investigation, review, or implementation task as the host with one Confer partner selected by task type and complexity. Trigger when the user asks for duo collaboration, or hands you a task to coordinate while calling external agents. Any task source and any result form apply.
---

# Duo

You are the host running in the model the user already selected. Keep ownership of the goal, technical judgment, verification, and completion. The partner takes bounded investigation, implementation, or independent refutation. The user hands over a task and does not manage agents.

## Host the Task

- Extract the goal, scope, authorization, and completion condition from the user's input and the actual context. An issue, a file, a discussion conclusion, or one sentence is enough. Do not require a template, and do not treat a background to-do as authorization to implement.
- Choose the collaboration the task needs. Do not fix phases, roles, or outputs. A discussion can end at a conclusion. A review can end with findings after you make sure that the evidence supports them. An implementation delivers the change, artifact, or pull request that the accepted goal requires.
- Assume that the user is not continuously present. Reuse relevant specialized skills and repository rules. Independently advance authorized investigation, implementation, verification, correction, and delivery. Resolve routine technical choices and failures yourself.
- Bring the user only decisions that change the goal, scope, product semantics, authorization, or material risk. Complete authorized work that does not depend on these decisions. Report pending decisions in the final result. Wait for an explicit answer before actions that depend on them. Absence, timeout, and silence grant no new authority.
- A seat's full access or bypass mode provides tool capability, not business authority. Follow the runtime's approval rules. Do not bypass required checks or approve requests for the user to keep unattended work moving. Do not extend authority to merge, release, deploy, or send external messages.

## Collaborate

Follow the room, model selection, privacy, sending, waiting, and recovery rules in [confer](../confer/SKILL.md). Choose one partner for a gap that remains unresolved, and divide work by actual tool capability and evidence. Reuse valid collaboration evidence that already exists. Do not dispatch more work to add participants. Every dispatch costs the partner a fresh read of the context and costs the host an acceptance check. If the host can complete a small task and make sure that the result is correct, do not dispatch it. This rule does not apply when the user explicitly asks for a partner or the task needs an independent review or independent refutation.

Choose the partner role from the gap first. Use Build Standard for development by default. For other tasks, choose Fast or Deep from complexity and consequence:

| Role | Use for |
|---|---|
| Research Fast | Clear lookups, source checks, narrow code location |
| Research Deep | Unknown root cause, cross-module causal analysis, architecture trade-offs and refutation |
| Build Fast | Small implementation or direct execution with a settled approach and acceptance criteria |
| Build Standard | Development ownership: investigation, approach selection, main implementation, integration, and verification |
| Build Deep | Read-only design consultation as needed, or bounded implementation when explicitly assigned |
| Review Fast | Independent read-only review of a routine, low-risk target |
| Review Deep | Independent review of persistence, security, money, concurrency, public contracts, or irreversible changes |

For Standard, select the agent best suited to the codebase with high reasoning effort. Do not default to the strongest reasoning model. For Fast, select a fast, low-cost model with reasoning effort of at least `high`. Tests showed that Fast models at low reasoning effort asserted facts without tool execution. If you cannot establish that Fast meets this minimum, report the limitation. Do not dispatch Fast at lower reasoning effort.

For Deep, select the strongest reasoning model currently available with high reasoning effort.

Set reasoning effort only when the selected agent supports it. Use values allowed by the confer rules. Select the concrete agent, model, and reasoning effort when creating the seat. Use agents Confer can currently use and models available on this machine. Do not hard-code them in this skill. The user's choice of agent, model, or reasoning effort takes precedence.

If a specified Standard agent or model is unavailable, report the limitation. Do not silently replace it with Deep.

Grade the task before dispatch, after reading the task and key materials. Do not grade by file count or changed lines alone. A Fast partner's escalation request supplements the host's assessment. It does not replace the host's identification of complex or high-consequence boundaries. Do not escalate because the task mentions a technical term. Make sure that the grade reflects the behavior that the task actually changes.

Complexity alone does not transfer the whole development task to Build Deep.

Before a handoff, make sure that the previous partner finished and has no related queued deliveries. Make sure that you established its side effects and that its results match the evidence. Give the next partner the specific gap and completed results. Never let two partners take over the same write scope at the same time.

Build Standard first reads the code, callers, and actual state. It compares the existing path with the strongest simpler alternative and decides routine technical questions. It retains the main implementation, cross-module boundaries, high-consequence work, integration, and final verification.

Consult Build Deep only for one specific question: a design question that affects the choice, a contradiction that remains after investigation, or a proposal that needs independent refutation. The host adds a Build Deep seat in the same room. Give it the specific question, necessary original materials and versions, constraints, and candidates. Include unknown facts that decide the choice. By default, Build Deep is read-only.

Request a recommendation, the strongest alternative, evidence, failure paths, and verification methods. The host relays the advice to Build Standard. Build Standard makes sure that the advice fits the evidence before implementation. A consultant that participated in design cannot provide the independent review pass for that design.

If Build Deep is explicitly assigned implementation, define its write scope and acceptance criteria. Apply the handoff requirements before transferring writes. Build Standard retains development ownership.

Give every dispatch the goal, input locations and versions, permissions, write scope, required result, and basis for verification. State the role's boundary in every dispatch: Research and Review are read-only. State that partners must not expand scope, delegate recursively, or contact others. The host chooses partners, prepares context, relays results, and handles failures.

Use Build Fast only for small tasks with a clear goal, scope, approach, and acceptance criteria. Dispatch only when the handoff and verification provide a concrete benefit. Do not fragment work to increase dispatch counts. If one command settles a question, run it yourself.

Require Fast reports to include the actual commands executed and their key output. Treat conclusions without execution evidence as incomplete. Confer returns final answers without tool call records. Treat reported commands and output as leads. Rerun the checks that decide the conclusion yourself, and make sure that their output supports it. Inspect every diff and cited location. Negative claims, such as "not found", "cannot access", and "no tools", also require execution evidence.

If Fast meets an unknown root cause, cross-module design, or high-consequence boundary, require facts supported by execution evidence and the specific gap. Require the same evidence when Fast reports that the task exceeds its capability. Route the work by role:

- Build Fast returns to the host for takeover or transfer to Build Standard, including when the approach is unsettled.
- Research Fast returns to the host for transfer to Research Deep.
- Review Fast returns to the host for transfer to Review Deep.

Use the collaboration mode that fits the task:

- Discussion and investigation: Form your own judgment first. The partner independently examines key assumptions, evidence, or the strongest alternative. After collecting the independent results, run targeted cross-review only on disagreements that affect the conclusion. Decide by verifiable facts, not by votes or consensus.
- Implementation: The partner can take investigation, implementation, and correction. Make sure that key facts and the integrated result match the evidence. Continue an accepted task in the same context. Keep one writer per worktree. Before multiple writers start, isolate them under existing rules and assign integration ownership explicitly.
- Review: Use a new private read-only context that did not see the author's reasoning or participate in design or drafting. An implementer re-checking its own work in the original session is not an independent pass. Self-tests, consultation, and Fast reports cannot replace independent review. Reuse an independent review that meets these requirements for the same fingerprint. Bind the review to an exact commit or content fingerprint. Afterward, make sure that the target did not change. Confer does not isolate filesystems. A review seat can still write files. Compare the target fingerprints and workspace state before and after review. Accidental writes invalidate the review. Establish the damage before correction. Read the affected callers and evidence and actively try to refute the change. Give every finding a concrete failure path and its basis. Never pass a target on insufficient evidence. Zero findings is a valid conclusion, so do not invent findings to fill a quota.

Before deciding the next step, make sure that the actual result at the affected entry point meets the goal. A finished delivery is not task acceptance. Aim follow-up work at a specific gap. When there is no new evidence or progress, stop repeating the discussion and switch to a verifiable method.

When new information changes an active task, use `cancel_delivery` under the confer rules. Wait for `cancelled` before a revised prompt or write handoff. Other deliveries queued before cancellation still run in FIFO order, even when cancellation reports uncertainty. To inspect uncertain work first, cancel those queued deliveries before checking it. The uncertainty flag clears when the next delivery starts processing on that seat, even if readiness or seat validation fails before dispatch. Cancel each superseded delivery. A `cancelled` result only confirms that the process tree Confer can track stopped. Detached or reparented processes, such as background daemons or development servers, can continue running. The host must inspect these processes separately. Cancellation does not undo side effects. Inspect the actual changes before transferring write ownership. Do not retire a seat to interrupt it.

A wait timeout is not a failure. Keep waiting, or make sure that you know the current execution status. Until you know that execution finished, do not resend the work or take over the same write scope. After a failed or interrupted execution, make sure that you established its status and side effects. Then decide how to continue.

For tasks with multiple phases or possible context changes, maintain and update one lightweight task artifact. Record the room ID, workspace root, seat names, delivery IDs, write ownership, result locations, acceptance evidence, and pending decisions. Respect artifact locations and read-only boundaries. Keep review records outside the repository. Do not copy full conversations or create a separate scheduling database.

Dispatch batches to multiple seats only for independent work with a clear benefit. Accept each result separately. One successful result does not establish that the whole batch succeeded.

## Finish

Deliver the conclusion or result the goal requires, and update the authorized output artifacts. Briefly report the result, evidence, unverified items, and decisions that truly need the user. Follow the confer rules for session takeover. Provide each finished partner seat's `resume_command` from `wait_output`. Do not provide a command while that seat has queued or running deliveries. If no command is available, state that the session cannot be resumed from the CLI.

This skill organizes collaboration within the current run. It does not switch the host model, run in the background, or resume automatically across restarts. When interrupted, state in the reply or an authorized task artifact where the results are, what remains uncertain about execution, and the next step.
