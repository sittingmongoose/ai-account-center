## Summary

- 

## Testing

Use what applies. If you skipped something, add a short note instead of forcing it.

- [ ] `bun run format && bun run lint:fix && bun run validate`
- [ ] `bun run validate:ci-parity` before requesting review
- [ ] `bun test tests/unit/commands tests/npm` if CLI commands or local package inputs changed
- [ ] `bun run ui:build && bun run ui:validate` if the Slint dashboard changed
- [ ] Relevant offline native-app checks if macOS or Windows app behavior changed
- [ ] Not run

## Checklist

Check what applies. Not every item is relevant for every PR.

- [ ] Base branch matches the intended fork branch, normally the repository's default branch
- [ ] Branch name follows `feat/*`, `fix/*`, `docs/*`, or approved hotfix naming
- [ ] Relevant `--help` output updated if CLI behavior changed
- [ ] Tests added or updated if behavior changed
- [ ] README or local docs updated if user-facing behavior changed
- [ ] If a check failed, the PR body explains what failed and what changed to fix it
- [ ] No secrets, tokens, or private config data are included

## Docs Impact

Docs impact: `none | minor | major`

Action: `no update needed` or describe what doc was updated
