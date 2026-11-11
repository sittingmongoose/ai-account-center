import { count, fail, list, obj, oneOf, optIso, text } from './schema-helpers';

/** Additive report data, retained for the next version of the Status view. */
export interface ReportV2 {
  title?: string;
  now?: string;
  next?: string;
  stages?: Record<string, unknown>[];
  deliverables?: Record<string, unknown>[];
  readmeQuestions?: boolean;
  progress?: { done: number; total: number; unit: string | null } | null;
}

export function reportV2(doc: Record<string, unknown>): ReportV2 {
  const result: ReportV2 = {};
  for (const [key, cap] of [
    ['title', 60],
    ['now', 140],
    ['next', 100],
  ] as const)
    if (doc[key] !== undefined) result[key] = text(doc[key], key, cap);
  if (doc.readmeQuestions !== undefined) {
    if (typeof doc.readmeQuestions !== 'boolean') fail('readmeQuestions');
    result.readmeQuestions = doc.readmeQuestions;
  }
  if (doc.progress !== undefined) {
    if (doc.progress === null) result.progress = null;
    else {
      const row = obj(doc.progress, 'progress');
      result.progress = {
        done: count(row.done, 'progress.done'),
        total: count(row.total, 'progress.total'),
        unit:
          row.unit === null || row.unit === undefined ? null : text(row.unit, 'progress.unit', 40),
      };
    }
  }
  if (doc.stages !== undefined) {
    result.stages = list(doc.stages, 'stages', 7, (value, field) => {
      const row = obj(value, field);
      text(row.label, `${field}.label`, 24);
      oneOf(row.state, `${field}.state`, ['done', 'now', 'waiting', 'next', 'skipped']);
      if (row.by !== undefined)
        oneOf(row.by, `${field}.by`, ['thread', 'owner', 'reviewer', 'other']);
      for (const key of ['round', 'of'])
        if (row[key] !== null && row[key] !== undefined) {
          const value = count(row[key], `${field}.${key}`);
          if (value < 1 || value > 99) fail(`${field}.${key}`);
        }
      if (row.note !== null && row.note !== undefined) text(row.note, `${field}.note`, 40);
      if (row.waitingOn !== null && row.waitingOn !== undefined)
        oneOf(row.waitingOn, `${field}.waitingOn`, [
          'agent',
          'review',
          'lock',
          'resource',
          'quota',
        ]);
      return Object.fromEntries(
        ['label', 'state', 'by', 'waitingOn', 'round', 'of', 'note']
          .filter((key) => row[key] !== undefined)
          .map((key) => [key, row[key]])
      );
    });
    if (result.stages.length < 2 || result.stages.filter((row) => row.state === 'now').length > 1)
      fail('stages');
  }
  if (doc.deliverables !== undefined) {
    result.deliverables = list(doc.deliverables, 'deliverables', 12, (value, field) => {
      const row = obj(value, field);
      text(row.id, `${field}.id`, 64);
      text(row.name, `${field}.name`, 48);
      text(row.version, `${field}.version`, 12);
      optIso(row.updatedAt, `${field}.updatedAt`);
      oneOf(row.kind, `${field}.kind`, [
        'concept',
        'report',
        'document',
        'build',
        'decisions',
        'video',
        'screenshots',
        'other',
      ]);
      for (const [key, cap] of [
        ['path', 400],
        ['url', 400],
        ['supersedes', 12],
        ['note', 160],
        ['unlocks', 160],
      ] as const)
        if (row[key] !== null && row[key] !== undefined) text(row[key], `${field}.${key}`, cap);
      if (row.main !== undefined && typeof row.main !== 'boolean') fail(`${field}.main`);
      if (row.cards !== undefined)
        list(row.cards, `${field}.cards`, 100, (value, at) => text(value, at, 200));
      if (row.media !== undefined) row.media = reportMedia(row.media);
      if (row.versions !== undefined)
        row.versions = list(row.versions, `${field}.versions`, 8, (value, at) => {
          const version = obj(value, at);
          text(version.version, `${at}.version`, 12);
          optIso(version.updatedAt, `${at}.updatedAt`);
          return Object.fromEntries(
            ['version', 'updatedAt', 'path', 'url']
              .filter((key) => version[key] !== undefined)
              .map((key) => [key, version[key]])
          );
        });
      return Object.fromEntries(
        [
          'id',
          'name',
          'kind',
          'state',
          'for',
          'want',
          'main',
          'version',
          'updatedAt',
          'supersedes',
          'path',
          'url',
          'note',
          'unlocks',
          'media',
          'versions',
          'cards',
          'readyAt',
          'builtAt',
        ]
          .filter((key) => row[key] !== undefined)
          .map((key) => [key, row[key]])
      );
    });
  }
  return result;
}

export function reportMedia(value: unknown): Record<string, unknown>[] {
  return list(value, 'media', 6, (value, field) => {
    const row = obj(value, field);
    oneOf(row.kind, `${field}.kind`, ['image', 'video']);
    if (
      (row.path === null || row.path === undefined) &&
      (row.url === null || row.url === undefined)
    )
      fail(field);
    for (const key of ['path', 'url', 'poster'])
      if (row[key] !== null && row[key] !== undefined) text(row[key], `${field}.${key}`, 400);
    if (row.caption !== null && row.caption !== undefined)
      text(row.caption, `${field}.caption`, 60);
    if (row.label !== null && row.label !== undefined)
      oneOf(row.label, `${field}.label`, ['built', 'mockup']);
    if (row.seconds !== null && row.seconds !== undefined) count(row.seconds, `${field}.seconds`);
    return Object.fromEntries(
      ['kind', 'path', 'url', 'poster', 'caption', 'label', 'seconds']
        .filter((key) => row[key] !== undefined)
        .map((key) => [key, row[key]])
    );
  });
}
