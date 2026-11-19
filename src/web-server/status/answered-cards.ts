import type { AgentReportsState } from './agent-reports-reader';
import { CLOSING_KINDS } from './answer-entries';
import type { SignedAnswer } from './answer-signing';
import type { BlockedOn, CardDoc } from './report-schema';

export type CardRow = AgentReportsState['cards'][number];

export function cardVersion(id: string, seq: number, sha256: string): string {
  return `${id}\n${seq}\n${sha256}`;
}

export function cardRowVersion(row: CardRow): string {
  return cardVersion(row.doc.id, row.doc.seq, row.sha256);
}

export function closesCard(answer: SignedAnswer, card: CardDoc, sha256: string): boolean {
  return (
    CLOSING_KINDS.has(answer.kind) &&
    answer.cardId === card.id &&
    answer.cardSeq === card.seq &&
    answer.cardSha256 === sha256
  );
}

export function closingCardVersions(answers: readonly { answer: SignedAnswer }[]): Set<string> {
  return new Set(
    answers
      .filter(({ answer }) => CLOSING_KINDS.has(answer.kind))
      .map(({ answer }) => cardVersion(answer.cardId, answer.cardSeq, answer.cardSha256))
  );
}

/** A wait explicitly linked to a resolved card can survive the writer's last update. */
export function resolvedCardWaits(
  reports: AgentReportsState,
  closed: ReadonlySet<string>
): (writer: string, reference: unknown, words: unknown) => boolean {
  const writers = new Map<string, Map<string, boolean>>();
  for (const row of reports.cards) {
    const resolved = row.doc.state !== 'open' || closed.has(cardRowVersion(row));
    const refs = writers.get(row.writer) ?? new Map<string, boolean>();
    const { id, label } = row.doc;
    for (const ref of [id, id.slice(id.indexOf('--') + 2), label]) {
      // If two cards share a label, keep any wait an unresolved card could explain.
      if (ref) refs.set(ref, resolved && refs.get(ref) !== false);
    }
    writers.set(row.writer, refs);
  }
  return (writer, reference, words) => {
    const named = typeof reference === 'string' ? reference.trim() : '';
    const prefix = typeof words === 'string' ? /^Card ([^:]+):/.exec(words)?.[1] : undefined;
    const ref = named || prefix;
    return ref !== undefined && writers.get(writer)?.get(ref) === true;
  };
}

export function activeBlocker(
  answered: ReturnType<typeof resolvedCardWaits>,
  writer: string,
  blocker: BlockedOn | null
): BlockedOn | null {
  return blocker?.kind === 'owner' && answered(writer, blocker.card, blocker.what) ? null : blocker;
}
