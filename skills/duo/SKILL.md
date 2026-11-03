---
name: duo
description: Complete a discussion, investigation, review, or implementation task as the host with Confer partners selected by task type and complexity. Trigger when the user asks for duo collaboration, or hands you a task to coordinate while calling external agents. Any task source and any result form apply.
---

# Duo

You are the host running in the model the user already selected. Own the goal, technical judgment, verification, and completion. Partners take bounded investigation, implementation, or independent refutation. The user hands over a task and does not manage agents.

## Host the Task

- Extract the goal, scope, authorization, and completion condition from the user's input and the actual context. An issue, a file, a discussion conclusion, or one sentence is enough. Do not require a template, and do not treat a background to-do as authorization to implement.
- Choose collaboration by the task. Do not fix phases, roles, or outputs. Discussions end at conclusions, reviews at supported findings, and implementations at the accepted change, artifact, or PR.
- Assign main development to Build Standard, investigation to Research, and runtime or production investigation to read-only Ops. First read the evidence package. Then personally inspect versions, write ownership, the affected entry point, and decisive diffs or citations. Reuse valid gate evidence. If evidence conflicts or risk is high, broaden the inspection. Return gaps to the original partner for correction. Do not take over its writes at the same time. Rerun decisive Fast checks yourself as required below.
- Assume that the user is not continuously present. Reuse relevant specialized skills and repository rules. Independently advance authorized investigation, implementation, verification, correction, and delivery. Resolve routine technical choices and failures yourself.
- Bring the user only decisions that change the goal, scope, product semantics, authorization, or material risk. Complete authorized work that does not depend on these decisions. Report pending decisions in the final result. Wait for an explicit answer before actions that depend on them. Absence, timeout, and silence grant no new authority.
- A seat's full access or bypass mode provides tool capability, not business authority. Follow the runtime's approval rules. Do not bypass required checks or approve requests for the user to keep unattended work moving. Do not extend authority to merge, release, deploy, or send external messages.

If a task tracker is in use, the host owns the item and maintains its status, decisions, blockers, and PR links. Creating a room does not require another item. Pass its ID and URL to partners. Seats only report progress, result URLs, and known item IDs. Before final reporting or retirement, read the original item again, including its project and links. A completed PR item does not replace this readback. Include a clickable task link.

## Partner Boundaries

Include these boundaries in every partner brief:

- Execute only the assigned task under the user's authority and the target repository's `AGENTS.md`. Permission modes add no scope or publication rights. Analysis, review, and planning remain read-only. Implementation and delivery require task authority.
- Do not expose or save credentials or unrelated private data. Do not contact others, force-push, release, deploy, or bypass required gates. Merge only under explicit authority or an applicable repository policy that grants it. Do not recruit other agents. Request partners through the host.
- Report result URLs and known task IDs to the host. The host owns task tracker updates and readback.
- Change only authorized paths. Preserve existing and unrelated changes. Follow repository conventions and preserve machine-required content and licenses.
- Investigate technical uncertainty and decide routine implementation choices yourself. Do not ask again for authority already granted. Return unresolved owner decisions to the host. Continue independent authorized work. Silence is not consent.
- Self-tests and tool success do not establish acceptance. Use an independent review bound to the final target. Consultation and Fast reports cannot replace it. Reuse a valid independent review of the same fingerprint.
- Report actual results, reproducible evidence, unverified items, and concrete owner decisions concisely. Follow the user's conversation language and the repository's artifact language.

## Collaborate

Follow [confer](../confer/SKILL.md) for rooms, model selection, privacy, delivery, and recovery. Keep one responsible partner per goal. Parallelize independent work when it provides a clear benefit. Reuse valid evidence. Dispatch costs context reading and acceptance work. Complete small tasks yourself when you can establish the result directly. Explicit partner requests and independent review or refutation still require partners.

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
| Ops | Read-only runtime and production investigation, logs, traces, metrics, impact assessment, and rollback advice |

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

Every brief must include the following information:

- Goal, input locations and versions, accepted decisions, and the actual absolute working directory.
- Permissions, exclusive write scope, required results, acceptance commands, and the Partner Boundaries above.
- A short evidence package: commands, working directory, version fingerprint, exit codes, decisive examples and counterexamples, key output, and omitted checks.

Research citations must support reported versions and numbers on the linked page itself. The host chooses partners, prepares context, relays results, and handles failures.

Use Build Fast only for small tasks with a clear goal, scope, approach, and acceptance criteria. Dispatch only when the handoff and verification provide a concrete benefit. Do not fragment work to increase dispatch counts. If one command settles a question, run it yourself.

Require Fast reports to include the actual commands executed and their key output. Treat conclusions without execution evidence as incomplete. Confer returns final answers without tool call records. Treat reported commands and output as leads. Rerun the checks that decide the conclusion yourself, and make sure that their output supports it. Inspect every diff and cited location. Negative claims, such as "not found", "cannot access", and "no tools", also require execution evidence.

If Fast meets an unknown root cause, cross-module design, or high-consequence boundary, require facts supported by execution evidence and the specific gap. Require the same evidence when Fast reports that the task exceeds its capability. Route the work by role:

- Build Fast returns to the host for takeover or transfer to Build Standard, including when the approach is unsettled.
- Research Fast returns to the host for transfer to Research Deep.
- Review Fast returns to the host for transfer to Review Deep.

When Standard needs Fast, a consultant, or module partners, it returns a bounded request to the host. Research Deep requests only Research Fast. The host uses `add_seat` in the same room, dispatches work, and relays results. Seats do not recruit partners themselves. Build Fast does not commit, push, or create PRs by default. Only an explicit assignment with commit authority permits a local commit in its isolated worktree. After the host inspects each result, the responsible partner integrates it.

Use the collaboration mode that fits the task:

- Discussion and investigation: Form your own judgment first. The partner independently examines key assumptions, evidence, or the strongest alternative. After collecting the independent results, run targeted cross-review only on disagreements that affect the conclusion. Decide by verifiable facts, not by votes or consensus.
- Implementation: The partner can take investigation, implementation, and correction. Make sure that key facts and the integrated result match the evidence. Continue accepted investigation, implementation, and correction in the same seat. Keep one writer per worktree. While a partner writes, the host must not modify that worktree. Before multiple writers start, use the repository's worktree tool and assign integration ownership explicitly. Put absolute paths and `git -C` commands in briefs. Keep the room's verified host workspace for MCP calls. A room does not isolate working directories.
- Review: Follow the independent review contract in [pre-review checks](references/pre-review-checks.md). Read affected callers and evidence. Actively try to refute the change. An implementer reviewing its own work is not an independent pass.

Before deciding the next step, make sure that the actual result at the affected entry point meets the goal. A finished delivery is not task acceptance. Aim follow-up work at a specific gap. When there is no new evidence or progress, stop repeating the discussion and switch to a verifiable method.

Before requesting independent review, every Build Standard seat must run the [pre-review checks](references/pre-review-checks.md), including module seats. Report commands, results, file sizes, and the final staged fingerprint. The host then adds a new private Review seat. Return corrections to the original Standard.

## Multiple Goals and Parallel Modules

For multiple independently deliverable goals, the host assigns one Build Standard seat and an isolated worktree to each goal. Targeted `send_message` calls can run different seats concurrently. For goals in one repository, the host sets the merge order. Continue each authorized PR and merge workflow in that order. Wait for the earlier workflow's accepted final state before continuing the next. The next Standard first updates from the latest default branch, accounts for its changes, and reruns affected checks. Combine the results into one report.

If one goal has two or more modules that need separate judgment and exclusive file ownership, use [parallel modules](references/module-parallel.md). The lead Standard defines the contract, division, and integration order. The host dispatches module Standard seats. Modules only stage changes. They do not commit, push, publish PRs, or merge. Consultant and Fast requests still pass through the host.

## Continue and Recover Results

Distinguish additions from revocations. If new input only adds material and leaves the original actions unchanged, queue it for the original seat. State that the original instructions still apply. If input revokes or narrows running actions, cancel the running delivery and every superseded queued delivery in one `cancel_delivery` call. Queued deliveries that you did not cancel run next in FIFO order and remain subject to uncertainty checks. Wait for a confirmed stop and inspect side effects. Then send a self-contained replacement that restates the retained instructions and the revoked actions.

A `cancelled` result only confirms that the process tree Confer can track stopped. Detached or reparented processes can continue running. Inspect them separately. Cancellation does not undo side effects. If cleanup leaves an uncertain marker, queued deliveries fail before dispatch. Inspect native work, then use `clear_uncertain_delivery` under the confer rules. This action sends no message and never retries work. Do not use `retire_seat` to interrupt work.

Lost responses, errors, timeouts, and interruptions do not prove that work did not execute. If the delivery ID is known to the current MCP process, use `wait_output` to inspect the original delivery. Compare native work and actual artifacts to establish results and side effects. Accept a complete reply only after inspecting its evidence. Treat `answer_incomplete` as partial evidence.

A wait timeout does not cancel work. Until execution ends, related queues are empty, and results are inspected, do not resend or transfer the write scope. If results cannot be recovered, preserve the uncertainty. After an MCP restart, queued messages and unfinished outputs are lost. Follow the confer rules for native inspection and uncertain markers. Do not assume that delivery outputs survive or send a probe that repeats the task.

For tasks with multiple phases or possible context changes, maintain and update one lightweight task artifact. Record the room ID, workspace root, seat names, delivery IDs, write ownership, result locations, acceptance evidence, and pending decisions. Respect artifact locations and read-only boundaries. Keep review records outside the repository. Do not copy full conversations or create a separate scheduling database.

Dispatch batches to multiple seats only for independent work with a clear benefit. Accept each result separately. One successful result does not establish that the whole batch succeeded.

## Finish

When the delivery loop is authorized, the original Standard seat follows the repository's PR and merge workflow. The host supplies new Review seats and relays. Modules do not execute delivery. Make sure that the observed PR head matches the reviewed and tested target. If the owner requires a draft, retain it. Otherwise, a green PR must be ready for review. Read back `headRefOid` and `isDraft=false`. A green draft alone does not establish completion.

Merge only under explicit authority or an applicable repository policy that grants it. When merge is authorized, make sure that merge, required commit trailers, and cleanup reached the repository's required final state. If merge is blocked, report the blocker and retain the ready state when permitted. Report other outcomes under the repository's workflow. Do not claim a merge from passing checks or a ready PR.

### Retire Partners

Call `retire_seat` only after all known deliveries finish and no queued or uncertain work remains. Results must be accepted and integrated, with no planned continuation. Retirement keeps metadata and the native session mapping, but the seat cannot receive more messages. Do not treat retirement as a reversible pause.

- Retire Fast and design consultants after their results are inspected and integrated. Retire Review after its findings are resolved. If the target fingerprint changes, use a new private Review seat. Retire Research and Ops after their conclusions enter the report.
- Retire the lead Standard after acceptance of the required delivery state. Without a delivery workflow, retire it after the final result is accepted and included in the report. Retire module seats only after merge, inclusion and activity checks, and successful worktree cleanup under the repository's rules.
- Retain seats needed for owner answers, UI iteration, review corrections, or delivery. If cleanup fails, retain the worktree and seat. Report the path, reason, and cleanup owner.

List retired seats and retained seats with reasons in the final report. State explicitly when no seats were retired. Follow the confer rules for available takeover commands.

Deliver the required result and update authorized artifacts. Report evidence, unverified items, and owner decisions. Follow the confer rules for takeover. Provide each finished seat's `resume_command` from `wait_output` only when it has no queued or running deliveries. If unavailable, state that CLI takeover is unavailable.

If review or acceptance exposes a recurring failure, report the failure class and a concrete improvement. Reuse existing repository rules and defect tracking. Do not add duplicate rules or change them without authority. If a required gate missed the defect, record it through the repository's process when authorized. Otherwise, report the gate gap and its consequence.

This skill organizes collaboration within the current run. It does not switch the host model, run in the background, or resume automatically across restarts. When interrupted, state in the reply or an authorized task artifact where the results are, what remains uncertain about execution, and the next step.
