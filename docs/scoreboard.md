# Model Scoreboard: contract (v1, draft)

The Model Scoreboard says which models to use for which work, from two kinds of data that are always labelled apart:
**planned benchmarks** and **what is observed in real agent work**. It is the second Leptos page (`/app/scoreboard`,
the shell's Scoreboard tab) and reads the same agent reports as the Status tab (`docs/status-tab.md`).

It is built to grow: new models, task types, measures and uses are data in the catalog, never a page rebuild. It is
built to be honest: every number shows its sample size and evidence, ranks only on enough data, and compares models
only within one task type.

## Identity (shared with Puppet Master)
A row is one **model on one runtime at one effort**, the way Puppet Master's Models System names them:
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
4. **Catalog** (`<reports>/v1/catalog/`, owned by AAC, edited as data):
   - `models.json`: `[{ model, label, family, runtimes: [], efforts: [], prices: { input, output, cachedInput, unit,
     asOf } | null, notes }]`
   - `task-types.json`: `[{ id, label, description, unit: "record" | "file" | "item" | "task", measures: [ids] }]`
   - `measures.json`: `[{ id, label, better: "lower" | "higher", unit, description }]`
   - `uses.json`: the views, `[{ id, label, description, taskTypes: [ids], weights: { measureId: number } }]`
   - `benchmarks.json`: `[{ id, label, taskType, description, items, scoring }]`

## Measures (v1)
Computed per row within one task type, from that row's runs only:
- **speed**: median wall seconds per unit of work (`wallSeconds / task.size.<unit>`), and per task.
- **quality**: share of the row's work that a later blind review passed without overturning
  (`review.overturned == false`), with the rounds until pass.
- **thoroughness**: files read / files assigned and records judged / records assigned (median); for review tasks,
  findings that a later reviewer confirmed.
- **laziness**: share of runs flagged `incomplete-read` or `scope-cut`, or whose read ratio is under 0.9.
- **cheating**: share of runs flagged `shortcut`, `cheating`, `fabricated`, `test-weakened` or `false-claim`; every
  such run is listed with its evidence, never only counted.
- **cost**: measured credits or quota per accepted unit; estimates (`cost.estimate`) are shown, labelled "estimate".
- **reliability**: share of runs that failed to start or ended `failed` / `cancelled` for infrastructure reasons.

## Honesty rules
- Every value carries `n` (runs behind it). Rates carry a 90% Wilson interval; medians carry the range.
- A row is **ranked** in a task type only with at least `minRuns` runs there (catalog setting, default 5) and at least
  one blind review. Below that it shows "too little data (n = 2)" and its runs, unranked.
- Observed and benchmark data are separate columns; they are never averaged together.
- Models are compared only within a task type: harder work goes to stronger models, so a pooled pass rate flatters the
  weaker ones.
- Reviewers are not ground truth: an overturn records which reviewer and model overturned, and a disputed overturn
  (`review.disputed`) is shown as disputed.
- Fix rounds on the same item are not independent samples: they count once per item for quality.

## Uses (the three views)
1. **Planning:** which models for Scout, the planning wizard and WorkNode creation (`scout`, `plan`,
   `worknode-create`).
2. **Building:** which models for building WorkNodes (`worknode-build`, `build`, `fix`, `test`).
3. **Puppet Master users:** advice for each kind of task a user runs (every task type with enough data), in plain
   words: "Recommended", "Fine", "Avoid for this", "Not enough data yet", each with the one-line reason and its n.
A use's recommendation is computed by the server from the catalog weights; the page only draws it.

## GET /api/scoreboard (view v1, sketch)
```jsonc
{
  "version": 1, "generatedAt": "...",
  "sources": [ Source ],                       // as in docs/status-tab.md
  "catalog": { "models": [...], "taskTypes": [...], "measures": [...], "uses": [...] },
  "uses": [ {
    "id": "planning", "label": "Scout, planning wizard, WorkNode creation",
    "taskTypes": [ {
      "id": "scout", "label": "Scout",
      "rows": [ {
        "model": "anthropic/claude-opus-5-5", "label": "Opus 5.5", "runtime": "claude-code", "effort": "xhigh",
        "n": { "observed": 12, "benchmark": 0 },
        "ranked": true, "rank": 1, "verdict": "recommended", "why": "Kept the most facts with no shortcuts (n = 12).",
        "measures": { "speed": { "value": 498, "unit": "s/task", "n": 12, "low": 396, "high": 1020 }, "...": {} },
        "flags": [ { "kind": "shortcut", "run": "<run id>", "evidence": "<path>" } ]
      } ]
    } ]
  } ],
  "recent": [ RunSummary ]                      // the latest runs, for "what's new"
}
```

## Puppet Master later
The run, catalog and view shapes avoid anything AAC-specific, so Puppet Master's Models System can read the same files
to rank models per persona and task. Nothing is added to Puppet Master's `Plans/`.
