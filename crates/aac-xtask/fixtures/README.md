These are declarative synthetic fixture documents captured from the X4 JavaScript
oracles at Unix millisecond 1793534400000 (2026-11-01 12:00 UTC). They are input
data for the native generator, not JavaScript programs or expected test outputs.
Each file body is kept verbatim as text; the two PNGs are base64. The native
writer shifts timestamps by the requested clock delta, substitutes the requested
root, assigns run date folders, sets board modification times, and handles
malformed variants, quota suppression, video generation and signed answers.

`status-kinds.json` includes the additional card kinds and resulting writer
sequence changes. The Scoreboard catalog is embedded directly from the source
catalog with `include_bytes!`, so catalog edits do not require recapturing data.

`tests/fixture-parity.mjs` independently invokes the existing JS generators at
two different dates and compares output bytes, file lists, board times and
metadata. It also validates signatures, card hashes, key compatibility, history
append, private file modes, and the synchronous browser-kit module adapter.
The old scripts remain the test oracle until the coordinated deletion lane.
