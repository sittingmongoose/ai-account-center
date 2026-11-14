# Optional Rust service (M3 skeleton)

The TypeScript dashboard remains the public HTTP server. `ServerOptions.rustService`
(or `AAC_RUST_SERVICE`) selects `off`, `shadow` or `on`; the default is `off`.
`ServerOptions.rustServiceBinary` (or `AAC_RUST_SERVICE_BINARY`) can name an existing
`aac-http` binary. The packaged default location is `dist/native/aac-http`.
Starting the dashboard never builds, downloads or installs this binary.

| Mode | Answer and failure behavior |
| --- | --- |
| `off` | All routes use TypeScript. No Rust process is started. |
| `shadow` | TypeScript answers first. A bounded background request compares status and canonical JSON; differences are logged without response bodies. |
| `on` | The three read routes below use Rust after their existing TS guards. Missing/unhealthy service, deadline, response cap or invalid DTO uses the TS handler and logs `rust.service.fallback`. |

The one ownership table is [`rust-service-routes.json`](../src/web-server/rust-service-routes.json),
imported by TS and embedded by Rust. GET `/api/health`, `/api/accounts/settings`
and `/api/diag` are `rust`; its wildcard marks all other routes `ts-fallback`.
HEAD uses the same read handler. Writes remain with TS.

`/api/health` remains public liveness only: `{"status":"ok"}`. Settings keeps its
existing signed-in-session and query checks, and reads the bounded, existing
`account-refresh-settings.json`. Before any DTO parse, a shared raw JSON gate
requires an object with exactly the expected unique keys. Arrays (including
single-element arrays), scalars, null, missing/extra keys and duplicates take
the logged TS fallback. Original bytes are retained for the strict DTO parse;
no Value conversion can erase duplicates or numeric spelling. Rust answers only a single DTO whose JSON value
is an integer token in 30..3600; these values are exactly representable in JS.
Decimal/exponent forms, strings, duplicate fields, out-of-range or unreadable
files, and any other unsupported input return a private 503 so the bridge logs
and serves the existing TS reader's result. Rust never invents a default from
an unproven parse. TS remains that file's only writer. Diagnostics
requires a signed-in dashboard session, including when dashboard auth is disabled.
It reports no Rust jobs/computers yet, scheduler `unavailable` / `jobs_not_migrated`,
and store `unavailable` / `store_owned_by_ts`. It does not open the native store or
refresh any provider.

## Boundary and lifecycle

The service binds no TCP port. It accepts only its Unix socket, mode 0600, in a
0700 random boot directory under the canonical AAC private directory's `run`
folder. TS refuses symlinked/nonprivate run directories. The Rust listener sets
0600 before it accepts requests and refuses existing socket paths.

TS sends a new 256-bit secret over the child's stdin pipe, never argv, inherited
environment or logs. A 0600 `run/aac-http.json` descriptor under the 0700 run folder
allows another front door using the same private root and binary to attach to a
healthy boot. It includes the boot secret and must be treated as credential data.
An owned exit removes its descriptor and boot directory. Startup replaces stale
metadata using a fresh random directory/secret, without signalling a PID from disk.
An attachment does not terminate the owner on cleanup. The spawning front door
owns the boot; its stdin closing ends that service. Normal shutdown also sends
SIGTERM, with a one-second SIGKILL backstop.

The same raw object gate covers the boot input, principal header and each
embedded route-table entry. The route list itself intentionally remains an
array; entries are retained as RawValue bytes and then checked as exact objects.
Each parsed DTO also denies unknown fields. Health and diagnostic DTOs are
output-only in this service and are not parsed from requests.

Every socket request needs both the boot secret and a strict `x-aac-principal`
JSON header (`kind`, `id`, `role`). Duplicate, missing or malformed headers and
impossible role/kind combinations get 401, including on liveness. TS constructs
these headers only inside the guarded handlers; it copies no caller credentials,
origin, principal or secret header. Session ids are hashed before forwarding.
The existing bar authenticates as a session; a nonce alone never creates a bar
principal. No hub authentication is ported. The DTO reserves device/bar/hub kinds
for later ports without granting them settings/diagnostic access.

For signed-out public liveness only, TS asserts the reserved envelope
`{kind:"session",id:"public-health",role:"public"}`. This denotes the verified
public-route decision, not a signed-in session; Rust cannot use it for private
routes. The secret is still mandatory. The socket boundary trusts the OS account
and its private run folder; it does not isolate mutually hostile processes running
as that same account.

Requests have a 500 ms total deadline, a 64 KiB response cap and a maximum of 16
in flight per front door. TS owns HTTP encoding, cookies, caching, compression,
ETags and all existing auth/origin/session checks. On Windows, where this service
is not built, the TS fallback remains available. A failed boot is retried on the
next dashboard start; there is no restart loop or provider activity.

## Offline checks

Build Rust only on Nas1 or the Mac, using an isolated copy, `nice -n 19`, two Cargo
jobs and the host's memory-admission rule. Never compile it on the VM. Workspace
`cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` and
`cargo check --target wasm32-unknown-unknown -p aac-api` cover the DTO and socket
boundary. The Linux integration test examines the child process's socket inodes
against both TCP tables; unsigned requests are tested over the real Unix socket.

On the VM use `AAC_NO_LOCAL_RUST_BUILD=1` and a temporary `CCS_HOME`. After
`bun run build:server`, copy the Nas1-built binary into the ignored
`dist/native/aac-http`, then run:

```sh
AAC_RUST_SERVICE=off node tests/spec/run.mjs
AAC_RUST_SERVICE=on node tests/spec/run.mjs
AAC_RUST_SERVICE=shadow bun tests/spec/capture.ts
```

`settings-numeric-forms.json` and `settings-parity.ts` compare 65 numeric/document
forms against the real TS reader and the actual Rust socket, then run them through
the production off/on/shadow bridge with exact TS-call and reply/fallback counters.
The table covers fractional rounding, exponents, huge numbers, negative zero,
strings, duplicate/out-of-domain values, top-level JSON shapes, nested arrays,
extra keys, and the canonical writer integers.
Unsupported shadow responses also log a fallback rather than compare a guessed
value. `bun tests/spec/settings-parity.ts --ts-only` checks the real TS side
without running Rust; the full spec runner selects that on the VM in off mode.

The fixture permits only that exact existing binary to cross its otherwise closed
process boundary. On/shadow checks fail when the binary is missing, and assert
actual forwarding counts so a fallback cannot falsely prove Rust parity. All five
roles retain their golden responses. The same sandbox injects real socket faults,
checks spoofed caller headers, route guards, attachment, unhealthy fallback,
response caps/deadlines and a real shadow difference. No live home or provider is
used. See [the contract harness](../tests/spec/README.md) for capture limits.
