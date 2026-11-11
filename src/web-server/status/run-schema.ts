/**
 * Finished runs and reviews (agent-reports/run@1, `writers/<writer>/runs/<YYYY-MM-DD>/<run>.json`).
 * The Status server does not read runs (they feed the Scoreboard); this is the one validator the
 * Scoreboard reader uses. Unknown values stay null, never 0; `cost.estimate` is kept apart from
 * measured cost.
 */
import {
  bool,
  count,
  fail,
  head,
  Invalid,
  isCardId,
  iso,
  link,
  list,
  obj,
  oneOf,
  optIso,
  optObj,
  optSlug,
  optText,
  RUN_SCHEMA,
  text,
  type DocHead,
  type ReportLink,
} from './schema-helpers';

export const RUN_FLAG_KINDS = [
  'shortcut',
  'cheating',
  'incomplete-read',
  'fabricated',
  'test-weakened',
  'scope-cut',
  'false-claim',
] as const;

export interface RunTokens {
  in: number | null;
  out: number | null;
  cached: number | null;
}

export interface RunQuota {
  account: string;
  window: string;
  deltaPct: number;
}

/** A run's size in each unit a task type may count (catalog `unit` + `s`). */
export type RunSize = Record<'records' | 'files' | 'items' | 'words' | 'tasks', number | null>;

export interface RunDoc extends DocHead {
  id: string;
  source: 'observed' | 'benchmark';
  benchmark: string | null;
  project: string | null;
  stream: string | null;
  task: { id: string; type: string | null; title: string; size: RunSize | null };
  model: {
    id: string | null;
    provider: string | null;
    harness: string | null;
    effort: string | null;
    account: string | null;
  };
  startedAt: string | null;
  endedAt: string;
  wallSeconds: number | null;
  outcome: string;
  cost: {
    usd: number | null;
    credits: number | null;
    quota: RunQuota | null;
    tokens: RunTokens | null;
    /** An estimate (such as quota-watch's token share), never mixed with measured values. */
    estimate: {
      source: string;
      method: string;
      credits: number | null;
      quota: RunQuota | null;
      tokens: RunTokens | null;
    } | null;
  } | null;
  scope: Record<
    'filesAssigned' | 'filesRead' | 'recordsAssigned' | 'recordsJudged',
    number | null
  > | null;
  review: {
    of: string;
    verdict: 'pass' | 'fail' | 'cannot-verify';
    findings: { high: number | null; medium: number | null; low: number | null };
    overturned: boolean;
    by: string | null;
    disputed: boolean;
  } | null;
  flags: Array<{
    kind: (typeof RUN_FLAG_KINDS)[number];
    detail: string;
    evidence: string;
    reportedBy: string | null;
  }>;
  evidence: ReportLink[];
  /** Results compare within one process version. */
  process: { id: string; version: string | null; sha256: string | null } | null;
  /** Task-specific numbers read by catalog measures; unknown stays null. */
  metrics: Record<string, number | null>;
  imported: { from: string; at: string } | null;
}

function num(value: unknown, field: string): number | null {
  if (value === null || value === undefined) return null;
  if (typeof value !== 'number' || !Number.isFinite(value)) fail(field);
  return value;
}

/** A time or an amount spent: known and at least 0, or null. */
function amount(value: unknown, field: string): number | null {
  const n = num(value, field);
  if (n !== null && n < 0) fail(field);
  return n;
}

function optCount(value: unknown, field: string): number | null {
  return value === null || value === undefined ? null : count(value, field);
}

function tokens(value: unknown, field: string): RunTokens | null {
  return optObj(value, field, (t) => ({
    in: optCount(t.in, `${field}.in`),
    out: optCount(t.out, `${field}.out`),
    cached: optCount(t.cached, `${field}.cached`),
  }));
}

function quota(value: unknown, field: string): RunQuota | null {
  return optObj(value, field, (q) => {
    const delta = num(q.deltaPct, `${field}.deltaPct`);
    if (delta === null) fail(`${field}.deltaPct`);
    return {
      account: text(q.account, `${field}.account`, 120),
      window: text(q.window, `${field}.window`, 40),
      deltaPct: delta,
    };
  });
}

const METRIC_NAME = /^[A-Za-z][A-Za-z0-9_]{0,63}$/;

export function run(doc: Record<string, unknown>, writer: string): RunDoc {
  const base = head(doc, RUN_SCHEMA, writer);
  if (!isCardId(doc.id) || !(doc.id as string).startsWith(`${writer}--`))
    throw new Invalid('id does not name its writer');
  const task = obj(doc.task, 'task');
  const model = obj(doc.model, 'model');
  const metricsRaw =
    doc.metrics === undefined || doc.metrics === null ? {} : obj(doc.metrics, 'metrics');
  const metrics: Record<string, number | null> = {};
  const names = Object.keys(metricsRaw);
  if (names.length > 50) fail('metrics');
  for (const name of names) {
    if (!METRIC_NAME.test(name)) fail('metrics');
    metrics[name] = num(metricsRaw[name], `metrics.${name}`);
  }
  return {
    ...base,
    id: doc.id as string,
    source: oneOf(doc.source, 'source', ['observed', 'benchmark'] as const),
    benchmark: optText(doc.benchmark, 'benchmark', 120),
    project: optSlug(doc.project, 'project'),
    stream: optSlug(doc.stream, 'stream'),
    task: {
      id: text(task.id, 'task.id', 120),
      type: optText(task.type, 'task.type', 64),
      title: text(task.title, 'task.title', 300),
      size: optObj(task.size, 'task.size', (s) => ({
        records: optCount(s.records, 'task.size.records'),
        files: optCount(s.files, 'task.size.files'),
        items: optCount(s.items, 'task.size.items'),
        words: optCount(s.words, 'task.size.words'),
        tasks: optCount(s.tasks, 'task.size.tasks'),
      })),
    },
    model: {
      id: optText(model.id, 'model.id', 120),
      provider: optText(model.provider, 'model.provider', 64),
      harness: optText(model.harness, 'model.harness', 64),
      effort: optText(model.effort, 'model.effort', 40),
      account: optText(model.account, 'model.account', 120),
    },
    startedAt: optIso(doc.startedAt, 'startedAt'),
    endedAt: iso(doc.endedAt, 'endedAt'),
    wallSeconds: amount(doc.wallSeconds, 'wallSeconds'),
    outcome: text(doc.outcome, 'outcome', 40),
    cost: optObj(doc.cost, 'cost', (c) => ({
      usd: amount(c.usd, 'cost.usd'),
      credits: amount(c.credits, 'cost.credits'),
      quota: quota(c.quota, 'cost.quota'),
      tokens: tokens(c.tokens, 'cost.tokens'),
      estimate: optObj(c.estimate, 'cost.estimate', (e) => ({
        source: text(e.source, 'cost.estimate.source', 80),
        method: text(e.method, 'cost.estimate.method', 200),
        credits: amount(e.credits, 'cost.estimate.credits'),
        quota: quota(e.quota, 'cost.estimate.quota'),
        tokens: tokens(e.tokens, 'cost.estimate.tokens'),
      })),
    })),
    scope: optObj(doc.scope, 'scope', (s) => ({
      filesAssigned: optCount(s.filesAssigned, 'scope.filesAssigned'),
      filesRead: optCount(s.filesRead, 'scope.filesRead'),
      recordsAssigned: optCount(s.recordsAssigned, 'scope.recordsAssigned'),
      recordsJudged: optCount(s.recordsJudged, 'scope.recordsJudged'),
    })),
    review: optObj(doc.review, 'review', (r) => {
      const findings = obj(r.findings, 'review.findings');
      // It decides quality, so a missing one must not read as "not overturned".
      if (typeof r.overturned !== 'boolean') fail('review.overturned');
      return {
        of: text(r.of, 'review.of', 200),
        verdict: oneOf(r.verdict, 'review.verdict', ['pass', 'fail', 'cannot-verify'] as const),
        findings: {
          high: findings.high === null ? null : count(findings.high, 'review.findings.high'),
          medium:
            findings.medium === null ? null : count(findings.medium, 'review.findings.medium'),
          low: findings.low === null ? null : count(findings.low, 'review.findings.low'),
        },
        overturned: r.overturned,
        by: optText(r.by, 'review.by', 120),
        disputed: bool(r.disputed, 'review.disputed', false),
      };
    }),
    flags: list(doc.flags, 'flags', 20, (value, at) => {
      const flag = obj(value, at);
      return {
        kind: oneOf(flag.kind, `${at}.kind`, RUN_FLAG_KINDS),
        detail: text(flag.detail, `${at}.detail`, 1000),
        evidence: text(flag.evidence, `${at}.evidence`, 600),
        reportedBy: optText(flag.reportedBy, `${at}.reportedBy`, 64),
      };
    }),
    evidence: list(doc.evidence, 'evidence', 20, link),
    process: optObj(doc.process, 'process', (p) => ({
      id: text(p.id, 'process.id', 120),
      version: optText(p.version, 'process.version', 60),
      sha256: optText(p.sha256, 'process.sha256', 64),
    })),
    metrics,
    imported: optObj(doc.imported, 'imported', (i) => ({
      from: text(i.from, 'imported.from', 300),
      at: iso(i.at, 'imported.at'),
    })),
  };
}
