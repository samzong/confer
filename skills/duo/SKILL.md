---
name: duo
description: Complete a discussion, investigation, review, or implementation task as the host with one Confer partner selected by task type and complexity. Trigger when the user asks for duo collaboration, or hands you a task to coordinate while calling external agents. Any task source and any result form apply.
---

# Duo

You are the host running in the model the user already selected. Keep ownership of the goal, technical judgment, verification, and completion. The partner takes bounded investigation, implementation, or independent refutation. The user hands over a task and does not manage agents.

## Host the Task

- Extract the goal, scope, authorization, and completion condition from the user's input and the actual context. An issue, a file, a discussion conclusion, or one sentence is enough. Do not require a template, and do not treat a background to-do as authorization to implement.
- Choose the collaboration the task needs. Do not fix phases, roles, or outputs. A discussion may end at a conclusion, a review at verified findings, and an implementation at the change, artifact, or PR the accepted goal requires.
- Reuse relevant specialized skills and repository rules, and own the authorized completion loop. Resolve technical uncertainty yourself. Bring the user only decisions that change the goal, a product trade-off, or the authorization boundary.

## Collaborate

Follow the room, model selection, privacy, sending, waiting, and recovery rules in [confer](../confer/SKILL.md). Choose one partner for a gap that remains unresolved, and divide work by actual tool capability and evidence. Reuse valid collaboration evidence that already exists; do not dispatch more work to add participants. Every dispatch costs the partner a fresh read of the context and costs the host an acceptance check. Do not dispatch a small task the host can complete and verify directly, unless the user explicitly asks for a partner or the task needs an independent review or independent refutation.

Choose the partner role from the gap first, then choose Fast or Deep from complexity and consequence:

| Role | Use for |
|---|---|
| Research Fast | Clear lookups, source checks, narrow code location |
| Research Deep | Unknown root cause, cross-module causal analysis, architecture trade-offs and refutation |
| Build Fast | Small implementation or direct execution with a settled approach and acceptance criteria |
| Build Deep | Complex implementation, cross-module design, or high-consequence boundaries |
| Review Fast | Independent read-only review of a routine, low-risk target |
| Review Deep | Independent review of persistence, security, money, concurrency, public contracts, or irreversible changes |

Fast uses a fast, low-cost model with lower reasoning effort. Deep uses the strongest reasoning model currently available with high reasoning effort; for Build Deep, pick the agent best suited to the codebase among those. Set reasoning effort only when the selected agent supports it, using values allowed by the confer rules. Decide the concrete agent, model, and reasoning effort when creating the seat, from the agents Confer can currently use and the models available on this machine; do not hard-code them in this skill. The user's choice of agent, model, or reasoning effort takes precedence.

The host grades the task before dispatch, after reading the task and key materials. Do not grade by file count or changed lines alone. A Fast partner's request to escalate is a supplement and does not replace the host's identification of complex or high-consequence boundaries. Do not escalate mechanically because the task mentions a technical term; check the behavior it actually changes. Before escalating, confirm that the Fast seat's delivery has finished and its side effects are verified, then give the new Deep seat the specific gap and the completed results. Never let two partners take over the same write scope at the same time.

Give every dispatch what the partner needs to work independently: the goal, input locations and versions, permissions and write scope, the required result, and the basis for verification. State the role's boundary: Research and Review are read-only; a Fast partner that meets an unknown root cause, a cross-module design, or a high-consequence boundary returns the verified facts and the specific gap to the host; a partner must not expand scope, delegate recursively, or contact others. The host chooses the partner, prepares context, relays results, and handles failures.

- **Discussion and investigation**: the partner independently checks key assumptions, evidence, or the strongest alternative, while the host forms its own judgment first. After collecting the independent results, run targeted cross-review only on disagreements that affect the conclusion. Decide by verifiable facts, not by votes or consensus.
- **Implementation**: the partner may take investigation, implementation, and correction; the host verifies key facts and the integrated result. Continue an accepted task in the same context. One worktree has one writer; isolate multiple writers under existing rules first and assign integration ownership explicitly.
- **Review**: run independent review in a new private read-only context that has not seen the author's reasoning. An implementer re-checking its own work in the original session is not an independent pass. Reuse valid existing evidence instead of generating it again. Bind the review to an exact target version, such as a commit or content fingerprint, and confirm afterward that the target has not changed. Read the affected callers and evidence and actively try to refute the change; give every finding a concrete failure path and its basis. Never pass a target on insufficient evidence; zero findings is a valid conclusion, so do not invent findings to fill a quota.

Check the actual result and the affected entry point before deciding the next step; a finished delivery is not task acceptance. Aim follow-up work at a specific gap. When there is no new evidence or progress, stop repeating the discussion and switch to a verifiable method.

A wait timeout is not a failure. Keep waiting or verify the status; until execution is confirmed finished, do not resend the work or take over the same write scope. After a failed or interrupted execution, verify the status and side effects before deciding how to continue.

## Finish

Deliver the conclusion or result the goal requires, and update the authorized output artifacts. Briefly report the result, evidence, unverified items, and decisions that truly need the user. Provide the `resume_command` for each partner seat whose delivery has finished, following the confer rules.

This skill organizes collaboration within the current run. It does not switch the host model, run in the background, or resume automatically across restarts. When interrupted, state in the reply or an authorized task artifact where the results are, what remains uncertain about execution, and the next step.
