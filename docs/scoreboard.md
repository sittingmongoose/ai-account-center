# Model Scoreboard: contract (v1)

The Model Scoreboard says which models to use for which work, from two kinds of data that are always labelled apart:
**planned benchmarks** and **what is observed in real agent work**. It is the second Leptos page (`/app/scoreboard`,
the shell's Scoreboard tab) and reads the same agent reports as the Status tab (`docs/status-tab.md`).

It is built to grow: new models, task types, measures and uses are data in the catalog, never a page rebuild. It is
built to be honest: every number shows its sample size and evidence, ranks only on enough data, and compares models
only within one task type.

## Identity (shared with Puppet Master)
A row is one **model on one runtime at one effort** (and, where the catalog says so, one process version), the way
Puppet Master's Models System names them:
- `model`: canonical `provider_id/model_id` (for example `anthropic/claude-opus-5-5`, `zai/glm-5.3`). Display labels
  never change it.
- `runtime`: the surface that ran it (`claude-code`, `codex`, `zcode`, `muse`, `omp`, `cursor`, `antigravity`, ...).
- `effort`: the effort or thinking level as the runtime names it (`high`, `xhigh`, `max`), or null.
Accounts are not part of the identity (they change cost, not the model); they stay on the run for cost attribution.

## Inputs
1. **Observed runs:** `<reports>/v1/writers/*/runs/<date>/*.json` with `source: "observed"` (agent-report `run@1`).
2. **Benchmark runs:** the same files with `source: "benchmark"` and `benchmark: "<id>"`; benchmark definitions live in
   the catalog.
3. **Imported history:** earlier evidence (for example the closure re-audit, where the same rows were judged by
   different models) imported once as observed runs with `imported: { from: "<path>", at }`, so the scoreboard does
   not start empty. Imports are reviewed like any other data change.
4. **Catalog** (`<reports>/v1/catalog/`, owned by AAC, edited as data). The seeds are in
   `docs/agent-reports/catalog/` (packaged with the server); at server start AAC writes a seed only where the share
   lists no such file (temporary dot file, then a hard link, which fails on a file that appeared meanwhile), never over one, so edits on the share stay. A catalog file
   keeps its last good copy when it later fails; with no good copy it shows as a Source problem and its kind stays
   empty (`minRuns` falls back to 5). Refusals are fixed words naming the field, never the file's text.
   Each file is `{ "schema": "catalog-<kind>@1", "<kind>": [...] }`:
   - `models.json`: `models: [{ model, label, family, runtimes: [], efforts: [], prices: { input, output, cachedInput,
     unit, asOf } | null, notes }]`. `model` is `<run.model.provider>/<run.model.id>`, or `run.model.id` alone when
     it already starts with `<provider>/` or the run names no provider. Prices stay null until someone enters real ones.
   - `task-types.json`: `taskTypes: [{ id, label, description, unit: "record" | "file" | "item" | "word" | "task",
     measures: [ids], byProcess }]`. `byProcess: true` keys rows by `process.id` + `process.version` too. A run's
     size in that unit is `task.size.<unit>s` (`records`, `files`, `items`, `words`, `tasks`).
   - `measures.json`: `minRuns` (runs a row needs in a task type before it is ranked) and `measures: [{ id, label,
     better: "lower" | "higher", unit, description, metric? }]`. `metric: { of, per }` names the run `metrics` a
     task-specific measure reads (Scout's `factsKept` per `factsTotal`).
     `preliminaryUnless: "<metric>"` labels a value "preliminary" unless every run behind it has that metric at 1
     (planning's answer key is final only once the owner's cards are answered).
   - `uses.json`: the views, `uses: [{ id, label, description, taskTypes: [ids] | ["*"], weights: { measureId: number }
     }]`.
   - `benchmarks.json`: `benchmarks: [{ id, label, taskType, description, items, scoring }]`.

## Reading runs
Same share rules as the Status reader (`docs/status-tab.md`, Reading rules): every level is listed before it is read,
ENOENT and ESTALE keep the last good copy and retry, a file absent from its listing stays for 2 minutes, temporary (dot)
files and anything not `.json` are ignored, NUL bytes are refused, 32 KB cap, only changed files are read. The highest
`seq` per run id wins, across date folders. A file that does not read is counted and named in the runs Source, never
dropped silently. One scan reads at most 1,000 writer folders, 5,000 run files and 64 MB (over a limit the newest date
folders are kept); the rest is named in the runs Source (`limits/run-writers`, `limits/runs`). More writers than the
Status reader: an old writer's runs are still history. A review with no boolean `review.overturned`, or a negative
`wallSeconds`, `cost.credits` or `cost.usd`, makes the file unreadable, by name. A run with no `model.id` or no `task.type` is not scored and is counted there too; a task type the
catalog does not list is not scored and is named in the catalog Source (a model it does not list is scored, labelled by
its id, `inCatalog: false`, and named there).

## Measures (v1)
Computed per column (see the view) within one task type, from that column's runs only. `done` is the only success.
"Failed" means an outcome of `failed`, `failure`, `cancelled`, `canceled`, `did-not-start`, `not-started`, `error`,
`crashed`, `timed-out`, `timeout`, `aborted`, `killed`, `interrupted` or `abandoned` (any case, `_` reads as `-`); the
format has no reason field, so a failure cannot be told apart from an owner's cancel. Any other outcome (`partial`,
`blocked`, a word this server does not know) is **unknown**: it says neither, so the run is left out of speed, cost,
reliability and overclaim, and the runs Source names each such outcome with its count.
- **speed**: median wall seconds per unit (`wallSeconds / task.size.<unit>s`) with its range, and `perTask`, the median
  wall seconds per run. Only `done` runs with a known size count. A run with no `task.size.tasks` is one
  task in a task type counted in `task`; other units stay unknown.
- **quality**: share of items whose latest decisive blind review did not overturn them
  (`review.overturned == false`), 90% Wilson interval. An item is one `project` + `task.id`, so fix rounds on it count
  once. `rounds` is the median reviews until the first pass. A latest review that is a disputed overturn leaves the item
  out of the share and adds to `disputed`; `cannot-verify` decides nothing and adds to `cannotVerify`. `overturns` lists
  every overturn: run, review, reviewer model (`review.by`, else the review run's model), disputed, first evidence link.
  `overturned` (the reviewer found the work wrong) decides, not `verdict`: a `fail` with `overturned: false` is not an
  overturn. `overturned` is required: a review without it is refused rather than read as a pass.
- **Review rule:** a review is a run with `task.type: "review"` whose `review.of` names a known run and which did not
  end before that run. The format has no "blind" field: every such review counts as blind, so a reporter must not
  report a non-blind check as a review. Others are ignored and counted in the runs Source.
- **thoroughness**: per run the lower of files read / assigned and records judged / assigned (each capped at 1); the
  median with its range, plus `files` and `records` apart. "Findings a later reviewer confirmed" is not built: runs do
  not record it.
- **laziness**: share of runs flagged `incomplete-read` or `scope-cut`, or whose read ratio is under 0.9. A run with
  neither fact is not evidence and is left out, so a batch that reports no scope shows unknown, not 0.
- **cheating**: share of all runs flagged `shortcut`, `cheating`, `fabricated`, `test-weakened` or `false-claim`; every
  flag of every kind is listed in the column's `flags` with its run and evidence, never only counted.
- **cost**: measured credits over the runs with a known cost, size and outcome (`done` or failed), per accepted unit
  (accepted: `done` and not overturned, disputes aside; an unreviewed run counts as accepted). Quota moves per `window` and `usd` are shown apart,
  never mixed in. Estimates (`cost.estimate`) sit in `estimate`, `label: "estimate"`, with their methods and sources.
- **reliability**: of the runs that are `done` or failed, the share that are `done`.
- **overclaim**: of the runs with outcome `done` and a standing decisive review, the share overturned; disputed overturns are
  left out. The format has no field for claimed checks, so outcome `done` is the claim, and no other outcome counts.
- **Metric measures** (catalog `metric: { of, per }`, such as Scout's **facts kept** and **verdict drift**): Σof / Σper
  over the runs where both are known and `of >= 0`, `per > 0`, with a Wilson interval over the per-sum; `n` is runs.
  Items in a run are not independent, so read the interval as a lower bound on the uncertainty.
- **preliminaryUnless**: `preliminary: true` unless every run behind the value has that metric at 1. A run with no value
  is not behind it.
- A measure a task type lists with no built-in reading and no `metric` is null. A listed id the catalog does not define
  is left out. A Scout row is keyed by `process.id` + `process.version` as well as the model, because the process moved
  quality more than the model did.

## Honesty rules
- Every value carries `n` (runs behind it; items for quality). Rates carry a 90% Wilson interval; medians carry the
  range. Unknown stays null, never 0.
- A column is **ranked** in a task type only with at least `minRuns` runs there (catalog setting, default 5). An observed
  column also needs at least one decisive blind review. Below that its `why` is "too little data (n = 2)" or "no blind
  review yet (n = 7)", with its runs, unranked. A benchmark column needs no review.
- Observed and benchmark data are separate columns; they are never averaged together. Benchmark columns are ranked only
  against other columns of the same benchmark.
- Models are compared only within a task type: harder work goes to stronger models, so a pooled pass rate flatters the
  weaker ones.
- Reviewers are not ground truth: an overturn records which reviewer and model overturned, and a disputed overturn
  (`review.disputed`) is shown as disputed.
- Fix rounds on the same item are not independent samples: they count once per item for quality.

## Uses (the three views)
1. **Planning:** which models for Scout, the planning wizard and WorkNode creation (`scout`, `plan`,
   `worknode-create`).
2. **Building:** which models for building WorkNodes (`worknode-build`, `build`, `fix`, `test`, `review`).
3. **Puppet Master users:** advice for each kind of task a user runs (`taskTypes: ["*"]`: every catalog task type with
   data), in plain words: "Recommended", "Fine", "Avoid for this", "Not enough data yet", each with the one-line reason
   and its n.
Named task types are always listed, with `rows: []` when they have no data. A use's recommendation is computed by the
server from the catalog weights; the page only draws it.

## Verdicts
Applied per use to each comparison group of one task type: the observed columns, or the columns of one benchmark.
1. A column's weighted measures are those the task type lists and the use weighs above 0, with a known value. Each gets
   a mark from 0 to 1. A measure in `share`: its value when higher is better, else 1 - value. Any other unit (speed,
   cost): the group's best / its value when lower is better, its value / the best otherwise.
2. `score` = Σ(weight x mark) / Σ(weight over known measures). `coverage` = known weight / the weight of every weighted
   measure the task type lists. With no known weight the column is unranked: "None of this use's measures is known yet".
3. `rank` is by score, then n. 1 is best.
4. **avoid** when cheating is weighted and at least 0.2; or a share weighted 3 or more marks under 0.5; or the score is
   under 0.6.
5. **recommended** when not avoided, with at least one other ranked column in the group, coverage at least 0.5, score at
   least 0.8 and within 0.05 of the top. So a column alone is never recommended.
6. **fine** otherwise. **not-enough-data** when unranked.
The reason is one line and always says n: "Strongest on Quality, with no cheating flags (n = 7).", "Flagged for cheating
in 2 of 5 runs (n = 5).", "Behind the top row, weakest on Speed (n = 7).", "The only ranked row here, so nothing to
compare it with (n = 96)."; " Preliminary." is added when a weighted value is preliminary.

## GET /api/scoreboard (view v1)
Same rules as `/api/status`: a browser session (`authKind(req) === 'session'`; no session `401 auth_required`, a device
token `403 device_scope`), `Cache-Control: no-store`, no query (`400 unexpected_query`), errors `{ error, code }` with
fixed sentences (`500 scoreboard_failed`).

**Refresh.** A read older than 10 s rescans the share (stat first, read only changed files) and waits at most 8 s; a hung
share answers with the last good data and its Source turns stale after 180 s. Nothing runs while nobody reads. The page
re-reads every `refresh.pollSeconds` while visible. There is no `/ws` hint: scans happen only on reads, so there is
nothing to push between them.

A generated example is `docs/agent-reports/examples/scoreboard-view.example.json` (from `scripts/scoreboard-fixture.mjs`;
a test keeps it equal to what the server builds; refresh with `UPDATE_SCOREBOARD_EXAMPLE=1 bun test
tests/unit/web-server/scoreboard/scoreboard-view.test.ts`).

```jsonc
{
  "version": 1,
  "generatedAt": "ISO",
  "refresh": { "pollSeconds": 60 },
  "sources": [ Source ],     // always ["scoreboard-runs", "scoreboard-catalog"], in that order
  "catalog": {
    "minRuns": 5,
    "models": [ { "model", "label", "family", "runtimes": [], "efforts": [], "prices": { "input", "output",
      "cachedInput", "unit", "asOf" } | null, "notes" } ],
    "taskTypes": [ { "id", "label", "description", "unit", "measures": [], "byProcess" } ],
    "measures": [ { "id", "label", "better", "unit", "description", "metric": { "of", "per" } | null,
      "preliminaryUnless": "<metric>" | null } ],
    "uses": [ { "id", "label", "description", "taskTypes": [], "weights": {} } ],
    "benchmarks": [ { "id", "label", "taskType", "description", "items", "scoring" } ]
  },
  "uses": [ {
    "id": "planning", "label": "...", "description": "...",
    "taskTypes": [ {
      "id": "scout", "label": "Scout", "description": "...", "unit": "item", "measures": ["speed", "..."],
      "byProcess": true,
      "rows": [ Row ]       // ranked observed by rank, then ranked benchmark by rank, then unranked by n
    } ]
  } ],
  "recent": [ RunSummary ]  // the latest 20 runs, newest first, scored or not
}
```

**Source**, as in `docs/status-tab.md`: `{ id, label, state, asOf, ageSeconds, staleAfterSeconds, detail }`.
`scoreboard-runs` is `missing` (no folder, or before the first scan), `error`, `stale`, `unreadable` (the detail counts
and names up to 5 files with their reasons) or `ok`. `scoreboard-catalog` is `missing` (no folder, a file absent),
`error`, `stale`, `unreadable` or `ok`.

**Row** (rows are rebuilt per use: the weights change rank and verdict):
```jsonc
{
  "key": "anthropic/claude-opus-5-5|claude-code|xhigh",   // with "|<process id>|<version>" when byProcess
  "model": "anthropic/claude-opus-5-5", "label": "Opus 5.5", "inCatalog": true,
  "runtime": "claude-code", "effort": "xhigh",            // null when unknown
  "process": { "id": "...", "version": "..." } | null,    // only when byProcess
  "n": { "observed": 7, "benchmark": 0 },
  // The headline: the observed column when ranked, else the first ranked benchmark column, else the unranked column
  // with the most runs. A benchmark headline's why starts "In the benchmark <label>: ".
  "basis": "observed" | "benchmark" | null, "benchmark": "<id>" | null,
  "ranked": true, "rank": 1, "verdict": "recommended" | "fine" | "avoid" | "not-enough-data",
  "verdictLabel": "Recommended", "why": "...",
  "observed": Column | null,
  "benchmarks": [ Column ]
}
```

**Column** (one row's numbers from one kind of data):
```jsonc
{
  "source": "observed" | "benchmark", "benchmark": "<id>" | null, "benchmarkLabel": "..." | null,
  "n": 7, "imported": 0,        // imported: how many of the n came from earlier evidence
  "reviews": 8,                 // decisive blind reviews of these runs
  "ranked": true, "rank": 1, "score": 0.885, "verdict": "...", "verdictLabel": "...", "why": "...",
  "preliminary": false,         // any measure in the column is preliminary
  "measures": { "<measure id>": MeasureValue },   // every measure the task type lists that the catalog defines
  "flags": [ { "kind", "run", "detail", "evidence", "reportedBy" } ],   // newest first, every flag
  "runs": [ RunSummary ]        // newest first: at most 20 unranked, 5 ranked
}
```

**MeasureValue**: every measure has `value`, `unit` (the catalog's, with `s/<unit>` and `credits/<unit>` for speed and
cost), `n`, `low`, `high`, `interval` (`"wilson90"`, `"range"` or null), `of`, `per` (the counts behind a share or
metric, else null) and `preliminary`. Numbers are rounded to 4 significant digits. Some add:
| Measure | Extra fields |
|---|---|
| speed | `perTask: Stat` |
| quality | `disputed`, `cannotVerify`, `rounds: Stat`, `overturns: [ { run, review, by, disputed, evidence } ]` |
| thoroughness | `files: Stat`, `records: Stat` |
| cost | `quota: [ { window, value, unit, n } ]`, `usd: { value, unit, n } \| null`, `estimate: { label: "estimate", credits: { value, unit, n } \| null, quota: [...], methods: [], sources: [] } \| null` |

`Stat` is `{ value, n, low, high }`.

**RunSummary**: `{ id, writer, source, benchmark, project, stream, task: { id, type, title }, model, label, runtime,
effort, process: { id, version } | null, outcome, endedAt, wallSeconds, imported: { from, at } | null, review: { of,
verdict, overturned, disputed, by } | null, flags (a count), evidence: [ { label, path } ] }`. Accounts stay on the run
file and are not in the view.

## Puppet Master later
The run, catalog and view shapes avoid anything AAC-specific, so Puppet Master's Models System can read the same files
to rank models per persona and task. Nothing is added to Puppet Master's `Plans/`.
