/** Resolve only catalog identities; unknown ids retain their reported spelling. */

export interface ModelAliasTable {
  aliases: Readonly<Record<string, string>>;
  effortSuffixes: readonly string[];
}

function exact(
  id: string,
  models: readonly { model: string; efforts?: readonly string[] }[]
): string | null {
  const found = models.find((model) => model.model === id);
  return found ? found.model : null;
}

/** One catalog model whose id is `provider/<id>` when the written id names no provider. */
function bare(
  id: string,
  models: readonly { model: string; efforts?: readonly string[] }[]
): string | null {
  if (id.includes('/') || id.includes('\\') || id.includes(':')) return null;
  const found = models.filter((model) => model.model.endsWith(`/${id}`));
  return found.length === 1 ? found[0].model : null;
}

/**
 * Every written spelling maps to a canonical catalog id by exactly one rule, in this order:
 * 1. the id itself is in the catalog;
 * 2. it is an explicit alias-table entry whose target is in the catalog;
 * 3. it carries one effort suffix (`-high`, ...) whose base resolves by rule 1, 2 or 4, and the
 *    suffix word moves to `effort` (an effort the run already names wins);
 * 4. it names no provider and exactly one catalog id ends with `/<id>`.
 * Nothing else resolves. The alias table itself is exact: a prefix (`codex/`, `zcode/`, ...) or a
 * harness form (`builtin:<plan>\<Model>`) resolves only when its full written string is an entry,
 * never by stripping the prefix. Effort suffixes come from the table's `effortSuffixes` and, for
 * callers without a table, from each model's catalog `efforts` (so `gemini-4-argon-high` still
 * moves its suffix). An alias whose target is not in the catalog resolves nothing.
 */
export function normalizeModel(
  id: string,
  effort: string | null,
  models: readonly { model: string; efforts?: readonly string[] }[],
  table?: ModelAliasTable | null
): { id: string; effort: string | null } {
  const known = new Set(models.map((model) => model.model));
  const aliased = (written: string): string | null => {
    const target = table?.aliases[written];
    return target !== undefined && known.has(target) ? target : null;
  };
  const baseOf = (written: string): string | null =>
    exact(written, models) ?? aliased(written) ?? bare(written, models);
  const direct = baseOf(id);
  if (direct) return { id: direct, effort };
  const suffixes = new Set(table?.effortSuffixes ?? []);
  for (const model of models) for (const suffix of model.efforts ?? []) suffixes.add(`-${suffix}`);
  const candidates: Array<{ id: string; effort: string | null }> = [];
  for (const suffix of [...suffixes].sort((a, b) => b.length - a.length)) {
    if (!id.endsWith(suffix) || id.length <= suffix.length) continue;
    const base = baseOf(id.slice(0, -suffix.length));
    if (base) candidates.push({ id: base, effort: effort ?? suffix.slice(1) });
  }
  return candidates.length === 1 ? candidates[0] : { id, effort };
}
