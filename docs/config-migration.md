# Config root migration: TS loader to Rust loader

The private config root is selected the same way by both loaders — `--config-dir`
(CLI `config_dir`) over `CCS_DIR` over `CCS_HOME` (with `.ccs` appended) over the
default home's `.ccs`; an empty value is skipped. The TS resolver
(`src/utils/config-manager.ts`, `_resolveCcsDir`) passes the selection through
Node's `path.resolve`; the Rust loader (`crates/aac-config/src/lib.rs`,
`Config::load`) resolves it through `aac_files::path::node::resolve`
(`crates/aac-files/src/path.rs`). Both collapse dot segments lexically and
follow no symlink while resolving. They differ on one Windows spelling:
Node's `path.resolve` keeps a current directory per drive, so a drive-relative
root such as `CCS_DIR=C:fixture` resolves against the drive's own current directory
(drive C's cwd). Rust's `Path::is_absolute` is also false for a drive-relative
path, but `base.join("C:fixture")` replaces the base because the selection carries
a drive prefix: with process cwd `D:\work` the result is still-relative `C:fixture`,
and the loader refuses it with `store root must be absolute`. A root-relative
Windows path without a drive (`CCS_DIR=/ccs`) keeps the base's drive in Rust
(`D:\ccs`, accepted), matching TS's current-drive resolution. Until the Rust
loader takes the drive-relative shape (M6), Windows operators should spell the
root fully absolute (`C:\ccs`) or plainly relative (`team\ccs`); the table lists
what each loader does with every shape.

| `CCS_DIR` / `CCS_HOME` shape | TS loader (`path.resolve`) | Rust loader (`node::resolve`) |
| --- | --- | --- |
| Absolute POSIX `/srv/ccs` (Linux, macOS) | `/srv/ccs`, dot segments collapsed | same |
| Absolute Windows `C:\ccs` | `C:\ccs`, dot segments collapsed | same |
| UNC `\\server\share\ccs` | absolute, kept | same |
| Relative `team/ccs` | process cwd joined with the selection | same |
| Drive-relative Windows `C:fixture` | drive C's own current directory joined with `fixture` | join replaces the base; still-relative `C:fixture` refused: `store root must be absolute` |
| Root-relative Windows `/ccs` (no drive) | current drive's root joined with `ccs` (`D:\ccs` from `D:\work`) | keeps the base's drive: `D:\ccs`, accepted (same) |
| Tilde `~/ccs` or `~` | not expanded: a literal `~` component under the process cwd | same (neither loader expands `~` here) |
| Dot segments `a/../b` | collapsed lexically, no symlink followed | same |
| Empty string | skipped, next source wins | same |

Both loaders append `.ccs` to the `CCS_HOME` value; the shapes above apply to
the value before that append.

## Named refusals
The Rust loader refuses with `Cause::Refused` and one of these words:

- `relative root requires an absolute base` — `node::resolve` was given a
  relative selection and a base that is not absolute. `Config::load` always
  supplies the process current directory as the base, so this guards direct
  use of the resolve API.
- `store root must be absolute` — the joined result is still not absolute
  (the drive-relative Windows shape above: the join keeps the selection's
  drive prefix and no root).

## Unchanged root rules

This guide records a difference; it changes no behavior. The private directory
rules stay as they are: owner-only (`0o700`) creation, a symlink in the root's
place refused (`crates/aac-store/src/fsx.rs`), and bounded reads through
`symlink_metadata` (`crates/aac-config/src/lib.rs`). The TS and Rust root
fixtures (`crates/aac-config/tests/fixtures/root-paths.json`) pin the shared
shapes; a future code change that admits the drive-relative shape adds its
case there first.
