/** Write-once corrections resolve over the complete history, before any view filter. */
import type { RunDoc } from '../status/run-schema';
import type { RunsState } from './runs-types';

export function resolveRunDocs<T extends { doc: RunDoc }>(records: readonly T[]) {
  const known = new Map(records.map((r) => [r.doc.id, r]));
  const refused = new Map<string, string>();
  const checked = new Set<string>();
  // Iterative walks also handle a writer with a chain longer than the JS call stack.
  for (const id of known.keys()) {
    const walk: string[] = [];
    const positions = new Map<string, number>();
    let at: string | undefined = id;
    while (at && !checked.has(at)) {
      if (positions.has(at)) {
        for (const member of walk.slice(positions.get(at)))
          refused.set(member, 'corrects forms a cycle');
        break;
      }
      positions.set(at, walk.length);
      walk.push(at);
      const doc: RunDoc | undefined = known.get(at)?.doc;
      if (!doc) break;
      const target: string | undefined = doc.corrects;
      if (!target) break;
      if (!target.startsWith(`${doc.writer}--`)) {
        refused.set(at, "corrects names another writer's run");
        break;
      }
      if (!known.has(target)) {
        refused.set(at, 'corrects names a missing run');
        break;
      }
      at = target;
    }
    for (const member of walk.reverse()) {
      const target = known.get(member)?.doc.corrects;
      if (!refused.has(member) && target && refused.has(target))
        refused.set(member, 'corrects names a refused correction');
      checked.add(member);
    }
  }
  const successors = new Map<string, string>();
  const newer = (a: RunDoc, b: RunDoc) =>
    Date.parse(a.writtenAt) - Date.parse(b.writtenAt) ||
    a.seq - b.seq ||
    (a.id > b.id ? 1 : a.id < b.id ? -1 : 0);
  for (const { doc } of records) {
    if (!doc.corrects || refused.has(doc.id)) continue;
    const held = successors.get(doc.corrects);
    if (!held) successors.set(doc.corrects, doc.id);
    else {
      const winner = newer(doc, known.get(held)?.doc ?? doc) > 0 ? doc.id : held;
      refused.set(winner === held ? doc.id : held, 'corrects conflict: a later correction wins');
      successors.set(doc.corrects, winner);
    }
  }
  checked.clear();
  for (const id of known.keys()) {
    const walk: string[] = [];
    let at: string | undefined = id;
    while (at && !checked.has(at) && !refused.has(at)) {
      walk.push(at);
      at = known.get(at)?.doc.corrects;
    }
    for (const member of walk.reverse()) {
      const target = known.get(member)?.doc.corrects;
      if (target && refused.has(target)) refused.set(member, 'corrects names a refused correction');
      checked.add(member);
    }
  }
  const latest = new Map<string, string>();
  for (const id of known.keys()) {
    if (refused.has(id)) continue;
    const walk: string[] = [];
    let at = id;
    while (!latest.has(at)) {
      walk.push(at);
      const next = successors.get(at);
      if (!next || refused.has(next)) break;
      at = next;
    }
    const leaf = latest.get(at) ?? at;
    for (const member of walk) latest.set(member, leaf);
  }
  const voided = new Set(
    [...latest].filter(([, leaf]) => known.get(leaf)?.doc.void).map(([id]) => id)
  );
  // A review of a voided review must not reintroduce that chain either.
  const reviewing = new Map<string, string[]>();
  for (const { doc } of records) {
    if (!doc.review || latest.get(doc.id) !== doc.id) continue;
    const target = latest.get(doc.review.of) ?? doc.review.of;
    const held = reviewing.get(target);
    if (held) held.push(doc.id);
    else reviewing.set(target, [doc.id]);
  }
  const queue = [...voided];
  for (let i = 0; i < queue.length; i++) {
    for (const id of reviewing.get(queue[i]) ?? []) {
      if (!voided.has(id)) {
        voided.add(id);
        queue.push(id);
      }
    }
  }
  const runs = records.flatMap((record) => {
    const { doc } = record;
    if (latest.get(doc.id) !== doc.id || voided.has(doc.id)) return [];
    if (!doc.review) return [record];
    const of = latest.get(doc.review.of) ?? doc.review.of;
    return [{ ...record, doc: { ...doc, review: { ...doc.review, of } } }];
  });
  return { runs, refused };
}

export function resolveRunsState(state: RunsState): RunsState {
  if (state.correctionsResolved) return state;
  const resolved = resolveRunDocs(state.runs);
  return {
    ...state,
    correctionsResolved: true,
    runs: resolved.runs,
    accepted: state.runs.filter((r) => !resolved.refused.has(r.doc.id)),
    problems: [
      ...state.problems,
      ...state.runs.flatMap((r) => {
        const reason = resolved.refused.get(r.doc.id);
        return reason ? [{ file: r.file, reason, since: r.firstSeenAt }] : [];
      }),
    ],
  };
}
