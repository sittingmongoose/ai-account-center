# Store written by 000a0ac7 (usage projector version 2)

Written by the `000a0ac7` crate, before observations were checked for credentials, so its redb
projection and checkpoints are that build's own. Synthetic values only: the label
`sk-ant-SYNTHETIC_SECRET_abcdefgh` is shaped like a credential and valid for nothing.

- `open/`: account `a` (that label, 99%) and account `b` (`5 hour`, 10%), one sample each at
  `1793000000000`.
- `closed/`: two samples each, five minutes apart (`a` 99% then 100%, `b` 10% then 20%), then
  maintenance at three days later, so the ledger holds the `usage.day_closed` summaries that build
  computed from them.

`aac.redb.zst` is the redb file compressed with zstd (`store.lock` is left out);
`tests/store_upgrade.rs` unpacks each store into a temporary folder.
