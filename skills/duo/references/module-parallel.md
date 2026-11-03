# Parallel Modules

The lead Build Standard owns the contract, main implementation, integration, and final verification. The host creates seats, relays results, arranges independent review, and accepts the result. Seats do not recruit other agents.

Use module Standard seats when at least two modules require separate investigation and implementation judgment. Assign every file to exactly one writer, including lockfiles, generated files, and shared configuration. The lead owns shared files. If ownership cannot be separated, keep one writer. Use Fast for small tasks with a settled approach.

## Contract and Dispatch

1. If commits are authorized, the lead prepares a shared interface in integration worktree A. Make sure that it passes applicable repository checks, and identify the contract version. Follow repository commit gates and independent review requirements before committing. Use `git commit -s` when the repository requires sign-off. Do not skip review to enable parallel work. Without commit authority, keep one writer and parallelize only read-only investigation.
2. Use the repository's worktree tool to create module worktrees from the contract commit on A's branch. The lead owns their cleanup. Give the host each module's absolute directory, exclusive files, contract version, runtime coupling, integration order, and acceptance commands.
3. The host uses `add_seat` to add a Build Standard seat for each module in the same room. Use targeted `send_message` calls with separate briefs. Keep the verified host workspace on Confer calls. Put module paths in briefs and require each partner to inspect its actual directory. Do not change the room root to represent a module path.
4. Each module changes only its own worktree and assigned files. Run the [pre-review checks](pre-review-checks.md), stage results, and report fingerprints and evidence. Do not commit, push, publish PRs, merge, or recruit partners. Return interface or scope problems through the host for the lead's decision. The host also supplies requested consultants or Fast partners. The lead retains responsibility for the whole goal.

## Interface Changes and Integration

If the interface changes, the lead first makes sure that A's index is empty. Commit the new contract under the same gates, changing only files that the lead owns. The host continues each affected module in its original seat. Each module runs `git merge --ff-only <integration-branch>` in its own worktree and reruns affected checks. Module authors adapt their own files. If fast-forward fails, investigate first. Do not discard staged work.

Before handoff to the lead, the host makes sure that module deliveries finish and no related messages remain queued. The lead inspects each module's contract version, exclusive file scope, staged diff, and cited locations. Require both commands below to produce no paths. Otherwise, return the module to its author for completion.

```sh
git -C <module-worktree> diff --name-only
git -C <module-worktree> ls-files --others --exclude-standard
```

In integration worktree A, apply modules in the declared order:

```sh
git -C <module-worktree> diff --cached --binary HEAD | git apply --index --3way
```

The lead resolves conflicts and exercises affected entry points. Before consolidation, make sure that the baseline and scope contain only this task's work. Use one PR by default. Where authorized, use `git reset --soft <verified-merge-base-with-default-branch>` to consolidate this task's contract commits and module changes into the staged target. Preserve unrelated commits and changes. Use dependent PRs only when module dependencies create useful boundaries for separate review. Each layer needs its own branch, worktree, and evidence. Do not rewrite unrelated history.

Run the full repository gates and actual affected entry points in A. Run the pre-review checks against the final staged target. Give the host the binary diff fingerprint and evidence for the target or each dependent layer. Do not push yet. The host adds a new private Review seat that did not participate in division, design, or implementation. Review each dependent layer and the integrated result. Return corrections to the original lead. After independent review passes, continue the lead under the authorized PR and merge workflow.

## Cleanup

After merge, the lead uses the repository's worktree tool and safety rules to clean up module worktrees. Make sure that the integrated result contains every staged module change. A path appearing in a PR does not prove that its contents were integrated. Make sure that no writer or related queued delivery remains active. Preserve retained task artifacts before removing a worktree.

The host retires a module seat only after worktree cleanup succeeds. If inclusion, activity, or cleanup is uncertain, retain both the worktree and seat. Report the path, reason, and cleanup owner. Never delete a module result solely because its branch has no commits.
