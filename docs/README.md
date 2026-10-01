# AI Account Center Maintainer Documentation

These guides describe the account dashboard, TypeScript services, native bars
and local source packaging in this continuing CCS fork. Current product source is
[sittingmongoose/ai-account-center](https://github.com/sittingmongoose/ai-account-center).
The original [CCS project](https://github.com/kaitranntt/ccs), authorship,
[MIT license](../LICENSE) and changelog remain historical attribution.

Source, tests and actual build/package inputs define implemented behavior.
Do not restore retired features to satisfy outdated prose.

| Need | Guide |
| --- | --- |
| Source ownership and current inputs | [Codebase summary](codebase-summary.md) |
| Engineering and validation | [Code standards](code-standards.md) |
| Data/credential boundaries | [System architecture](system-architecture/index.md) |
| Existing provider collectors | [Provider flows](system-architecture/provider-flows.md) |
| Codex profiles and guarded activation | [Codex account contract](codex-auth.md), [activation](activate-in-place.md) |
| Product scope | [Product overview](project-overview-pdr.md) |
| Current work | [Project direction](project-roadmap.md) |
| Local packaging and retained automation | [Release process](release-process.md) |
| Structured diagnostics | [Logging contract](logging-contract.md) |
| Source-tree measurements | [Hardening method](hardening-debt-burndown.md) |
| Slint UI and licensing | [Dashboard guide](../web-dashboard/README.md) |
| Native clients | [Mac](../macos-bar/README.md), [Windows](../windows-bar/README.md) |
| Container deployment | [Docker guide](../docker/README.md) |

The primary command is `ai-account-center dashboard`; `ccs config` remains a
compatibility alias. Private `~/.ccs/` data and session/profile aliases remain
unchanged. Retired runtime/proxy/tooling guides are available in Git history;
upstream docs are not the current product's deployment instructions.

Verify documentation with `bash tests/docs/quickstart-parity.sh` and
`node tests/docs/documentation-freshness.js`. Keep relative source links current.
Generated hardening reports must be regenerated from the current source using
their owning script; old validation totals are not proof of the current build.
