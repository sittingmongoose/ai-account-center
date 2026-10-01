# AI Account Center Project Direction

Current work belongs in
[AI Account Center issues](https://github.com/sittingmongoose/ai-account-center/issues)
and [pull requests](https://github.com/sittingmongoose/ai-account-center/pulls).
The reviewed product source branch is `feat/activate-in-place`. Verify which
revision contains the current product before basing new work on another branch;
the repository default and source revisions are independently managed.

The supported direction is account usage, safe existing-profile controls,
truthful Analytics, explicit app updates and native Mac/Windows bars. Keep the
TypeScript backend and pinned Slint 1.18.1 web UI. Preserve private storage,
session aliases, auth/switch guards and rollback.

Before implementing a change, verify current source/tests, identify the owner,
state the compatibility effects and run focused offline checks. Do not recreate
retired runtime routing, React/Vite or original publication/deployment automation.

Use [hardening measurements](hardening-debt-burndown.md) as investigation queues,
not automatic refactor mandates. Keep the generated inventory tied to current
source. [Source map](codebase-summary.md), [standards](code-standards.md) and
[architecture](system-architecture/index.md) describe the retained boundaries.
