/** Resolve only catalog identities; unknown ids retain their reported spelling. */
export function normalizeModel(
  id: string,
  effort: string | null,
  models: readonly { model: string; efforts?: readonly string[] }[]
): { id: string; effort: string | null } {
  const matches = (candidate: string, model: string) =>
    candidate === model ||
    (!candidate.includes('/') && candidate === model.split('/').slice(1).join('/'));
  const exact = models.filter((model) => matches(id, model.model));
  if (exact.length === 1) return { id: exact[0].model, effort };
  const suffixed = models.flatMap((model) =>
    (model.efforts ?? []).flatMap((suffix) =>
      id.endsWith(`-${suffix}`) && matches(id.slice(0, -suffix.length - 1), model.model)
        ? [{ id: model.model, effort: effort ?? suffix }]
        : []
    )
  );
  return suffixed.length === 1 ? suffixed[0] : { id, effort };
}
