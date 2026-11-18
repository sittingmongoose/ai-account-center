# Report refusal fixtures

Each file under `refused/` contains a complete `document` and the dashboard reader's expected `reason`. Validate `document` against its named schema or save it as a standalone file for `agent-report validate`. The reader must return `reason`, and schema validation must refuse the document.

The published copies are under `agent-reports/v1/fixtures/refused/`. The card examples use synthetic writer and card IDs.
