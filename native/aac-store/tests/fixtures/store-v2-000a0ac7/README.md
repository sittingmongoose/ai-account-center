# Store written by 000a0ac7 (usage projector version 2)

Written by the `000a0ac7` crate, before observations were checked for credentials, so its redb
projection and checkpoints are that build's own. Synthetic values only: the label
`sk-ant-SYNTHETIC_SECRET_abcdefgh` is shaped like a credential and valid for nothing.

- `open/`: account `a` (that label, 99%) and account `b` (`5 hour`, 10%), one sample each at
  `1793000000000`.
- `closed/`: two samples each, five minutes apart (`a` 99% then 100%, `b` 10% then 20%), then
  maintenance at three days later, so the ledger holds the `usage.day_closed` summaries that build
  computed from them.

- `multi/` (the round-4 recheck's writer, refused value 100): series `a/five` holds 10 and 20 on
  day 0; 30, a refused 100 (credential-shaped label), 55 and 60 on day 1; 70 and 75 on day 2.
  Series `b/five` is clean. Account `c` has only readings whose window identity is
  credential-shaped. One maintenance pass closes days 0 to 5, so the day-0 close carries day 1's
  reset periods, built over the refused reading.
- `weekly/`: a 7-day window, one reading a day, maintenance an hour after each midnight; day 2's
  reading is refused (100), so that build saw 100 then 40 as a reset and closed the invented
  period on day 6.

- `forward`, `forward-wm`, `long-unknown-lookback`, `long-unknown-inside`, `edges`, `two-windows`:
  plus `week-apart-5h` and `week8-wm7` (a known length without reset times, from the round-6 recheck):
  the recheck plans (`../plans/<name>.json`, records and maintenance times), written by
  that build with its plan runner. The tests replay each plan without its refused readings to get
  the clean reference.

`aac.redb.zst` is the redb file compressed with zstd (`store.lock` is left out);
`tests/store_upgrade.rs` unpacks each store into a temporary folder.
