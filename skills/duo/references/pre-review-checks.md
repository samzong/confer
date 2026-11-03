# Pre-Review Checks

Every Build Standard seat, including module seats, runs these checks before requesting independent review. Self-tests establish only what they exercise. They do not replace independent review or commit gates.

## Prepare the Target

1. Read repository configuration, Git hooks, and commit tooling. List the gates that a commit will run.
2. Stage only the authorized changes. Run gates that do not depend on an independent review result against this final staged target. Include applicable size, format, secret, and generated-file checks. Use supported standalone check commands when available. Do not attempt a commit to test its hooks.
3. If a gate fails, correct the problem, stage the correction, and rerun affected checks. Report each command, working directory, version, exit code, and decisive output. Report an estimated size for every changed file and the staged fingerprint. If a check cannot run, state the missing capability and what remains unverified. Keep review-dependent receipts for after review. Never invent them.

Use the repository's size gate and estimation method when one exists. Do not invent a threshold or install a different tool to simulate a missing gate. If a gate blocks review, resolve it under repository rules. Silence does not waive it.

Capture the staged target with `git diff --cached --binary HEAD` and the repository's fingerprint command. Also record unstaged changes and untracked files. Bind the evidence to the exact target, rather than a branch name alone. Before submission or commit, make sure that the reviewed target still matches.

## Request Independent Review

After the prerequisite gates pass, the host adds a new private Review seat. Give it the goal, source materials, acceptance contract, exact target, and evidence locations. Do not give it the author's reasoning or consultation transcript. The reviewer must not participate in design or drafting.

Include the repository path, review mode, resolved refs, binary diff fingerprint, and relevant input hashes. State exclusions, accepted owner decisions, supported use, observed command results, unknown facts, and known blocking findings. Include the diagnosis and completion lists required by the repository's workflow. Mark each item complete, unverified, or not applicable with a reason. For a documentation-only change, do not invent a runtime diagnosis.

Require a review result that identifies the target fingerprint, inspected scope, commands, findings, and unverified items. Each finding must name a concrete failure path and supporting location. A result without sufficient evidence cannot pass. Zero findings is valid. Reuse a review that meets this contract for the same fingerprint.

Require the reviewer to challenge the diagnosis, completion claims, and each known blocking finding against current evidence. Include validity, consequence, exposure, recovery, and the reason for blocking. Require one disposition per known finding and each diagnosis or completion item. Ask for additional defects in the same pass. The host inspects citations and coverage before accepting the result. Do not schedule another identical review solely for a different workflow label.

Compare target fingerprints and workspace state before and after review. Confer provides no filesystem isolation, so a read-only brief does not prevent writes. If the reviewer writes, establish the damage and correct it before requesting another independent review. Return implementation corrections to the original Standard seat. If the target changes, run affected gates and obtain a review of the final target. Valid evidence for unchanged inputs remains reusable.
