# Antigravity accounts on Ubuntu

AI Account Center retains Antigravity usage without installing a launcher or
changing the native login. The account-control runtime is staged and **disabled**
in this source checkpoint. A passing fixture, an edited settings file or a
running service cannot release native activation. The reviewed native identity,
same-conversation restart and two-account round trip are still required.

The future control target is the Ubuntu CLI. Dashboard, Mac bar and Windows tray
controls address that same target. They do not change Mac or Windows logins.
Native credential selection and a verified running CLI account remain distinct.
Inactive-account quota reads use private saved credentials without changing the
live native credential. No credential or raw Google subject is returned over HTTP.

## Installation layout and held setup

The product keeps the official executable at `~/.local/bin/agy`. It preserves
the existing HOME, working directory, conversation database, history and native
settings. Public runtime modules and the pinned parser dependencies use a
versioned, private bundle under
`~/.local/share/ai-account-center/antigravity-runtime/bundles/`.
There are no experiment-directory dependencies in installed source.

The parser requirements are `pyte==0.8.2` and `wcwidth==0.9.1`, installed with
the packaged wheel hashes into the bundle's own `parser/` directory. pyte is pure
Python and wcwidth is a CPython stable-ABI (`abi3`, 3.10+) wheel, so that
directory does not depend on the system Python minor version and survives an
Ubuntu release upgrade of Python. The bundle's private
virtual environment supplies only the launcher and helper interpreter. The runtime
requires Ubuntu, Python 3.9+, a real foreground terminal and the native executable
version/pin supported by its reviewed release. The system interpreter also needs
Ubuntu's [python3-dbus package](https://packages.ubuntu.com/jammy/python3-dbus)
and a reachable per-user D-Bus session to verify the native credential backend.
These are separate from the pinned parser virtual environment. Missing D-Bus,
a locked/ambiguous native target, or an unexpected backend keeps activation
unavailable; setup does not unlock or create a keyring item. See the packaged
[dependency notices](../scripts/antigravity/runtime/THIRD_PARTY.md).

From the built checkout or installed package directory, the setup plan is read-only:

```bash
python3 -I scripts/antigravity/install_runtime.py --plan
```

After the native release gate passes, explicit preparation would use `--apply`.
The current gate rejects it before creating any bundle. Preparation verifies the
public file manifest, preserves the native executable and writes its private
installation descriptor. It does not import accounts, adopt shell profiles,
launch a resident service or start the CLI.

If dependency preparation fails before a descriptor or launcher is published,
the private preparation marker retains the exact owned bundle identity. Explicit
`--recover-preparation` removes only that incomplete, unadopted public bundle and
allows retry. A foreign bundle, descriptor or launcher is preserved and requires
review. Account credentials, settings and history are outside that cleanup.

Explicit adoption is a separate gated operation:

```bash
python3 -I scripts/antigravity/adopt_runtime.py
python3 -I scripts/antigravity/adopt_runtime.py --rollback
```

Adoption appends a launcher PATH block at the end of `.bashrc` and `.profile`,
after stock Ubuntu `~/.local/bin` PATH blocks so a new login shell resolves
`agy` to the managed launcher. It multiplexes the existing native status-line
command and creates the user service
`ai-account-center-antigravity.service`. Original bytes, permissions and timestamps
are recorded privately before publication. Recovery checks each exact owned
publication; it preserves foreign edits. Prepared and partial recovery journals
can also be rolled back. A reachable service is separate from native capability.
The current packaged resident entry point has no released native binder and
cannot claim activation readiness.

With a released and adopted runtime, the ordinary user invocation remains:

```bash
cd /path/to/project
agy
```

The foreground launcher retains the original terminal, environment, project and
actual opened conversation. It never manufactures a conversation from a recent
summary or replays a prompt. Unknown flags and native version/help/update/login
commands pass through to the official executable. An official update invalidates
the old native pin; ordinary native controls continue to work.

## When the runtime service fails

`ai-account-center antigravity status` and the dashboard name the cause when the
service is not running. A read-only check
(`scripts/antigravity/runtime_health.py`) runs the service interpreter,
`/usr/bin/python3 -I -B`, against the installed bundle and reports, for example,
"Runtime service failed: missing Python module pyte (system Python is 3.14; the
runtime bundle was built for 3.13)". It writes nothing and reads no account data.

Bundles built before the `parser/` directory kept the parser in the virtual
environment's `lib/pythonX.Y/site-packages`. A release upgrade that replaces the
system Python leaves that path behind, and the service then cannot start. Do not
start the service again or edit the bundle. Rebuild it from the packaged sources:

```bash
python3 -I scripts/antigravity/rebuild_bundle.py --plan
python3 -I scripts/antigravity/rebuild_bundle.py --apply
```

The rebuild treats a bundle whose parser no longer loads as stale even when the
native CLI is unchanged. It builds a new bundle with the version-neutral parser,
repoints the launcher, unit and descriptor, starts the service and restores the
previous owned bytes if the service does not answer. Only the runtime service is
restarted: the dashboard asks the service socket on every check, so it picks up
the rebuilt runtime without a dashboard restart. The pin-named descriptor
backup of an earlier generation is kept; this generation's backup is stored next
to it under a content-addressed name. If the same packaged sources would rebuild
the same failure, `--plan` refuses with `runtime-parser-unusable`. A parser check
that cannot run refuses with `runtime-parser-check-failed`; it is never reported as
current. After a failed cutover, `--recover-rebuild` removes the incomplete bundle
and `--plan`/`--apply` can be retried: the rollback restores each owned file's bytes,
mode and timestamps through a new inode, and both accept that restored state.

## Manual and automatic control

Coding plans are normalized only from reported plan/tier values; unknown values
stay unchanged. The optional `antigravityPlan` account field describes the plan,
model availability at the sample time and quota policy. Free and Google AI Plus
have weekly quota only; Pro trial, paid Pro and Ultra have 5-hour and weekly
windows. Ultra 5x and 20x remain distinct when explicitly reported. Workspace
alone and Code Assist plans have no bundled Antigravity coding quota; Code
Assist's Gemini CLI quota is separate and AAC does not read it. Enterprise
Standard/Plus use pooled 7-day project credit; pay-as-you-go is metered.

The dashboard shows Gemini and Claude/GPT pools separately, with `Weekly only`
or `Not on plan` for known missing windows. AI credits are labelled
`AI credits (overage)` and are used only after plan quota runs out when AI Credit
Overages is on. AAC does not assume that setting is enabled. Details include
the plan summary, available models and a reminder that family members sharing
a Pro/Ultra plan may share one quota pool. Retired Claude 4.6 and GPT-OSS models
leave the plan model list for samples from 2026-11-02 onward.

Manual activation requires two separately verified saved account identities and
supported native continuity. When the CLI is running, an explicit, one-use
confirmation binds the exact reviewed processes and saved target account.
Unmanaged, busy, unreadable or changed sessions remain deferred.

Automatic switching is independent of Codex. Defaults are **off**, **95% used**,
a **60-second** check interval and no selected quota pool. Before enabling it,
choose the exact reported model/quota pool shared by both accounts. Gemini and
third-party model pools are separate; their 5-hour and weekly constraints are not
combined into an invented allowance. The controls display provider labels where
reported and preserve derived bucket-membership provenance. Unknown, cached,
expired or rate-limited readings cannot authorize a switch.

The automatic path may stop only a freshly proved, owned idle resident CLI. It
rechecks identity, credential revision, conversation, project, terminal, input
generation and exact restorable plan under the account lock. After stopping, it
uses a newly earned private quiesced receipt while refreshing quota/settings.
Changed policy resumes and proves the previous session before deferring. Input
remains blocked through restart, proof and durable completion. A failed target
can restore the original terminal/template once; a lost completion reply never
authorizes stopping a potentially resumed session.

## Updates and verification limits

The dashboard's installed-app updater identifies the original executable rather
than the product PATH shim. A closed CLI can update under a complete fresh census
without inventing a live identity or restarting anything. A running managed CLI
uses the same proved idle session coordinator and account lock.

The old executable is backed up privately. The official updater supplies no
authenticated publication-ownership receipt: automatic rollback therefore never
overwrites a changed or renamed executable, including an unrelated same-byte
replacement. If a running session's updated executable has no supported native
proof, the result remains `restart_failed` with recovery retained. It is not a
successful managed update. A verified unchanged no-op makes no binary writes.

Offline tests cover injected account/driver contracts, actual disposable
same-user socket/PTY workflows, original status-line preservation, partial
adoption rollback, cached quota boundaries and foreign replacements. They do not
prove native Antigravity sign-in, the second account, automatic switching or an
actual native update. Those live checks remain required before release.

## Saved profiles: add, sign in again and remove

Saved profiles live in the private registry (`~/.ccs/antigravity-profiles/`,
credential generations in `~/.ccs/antigravity-instances/<profile>/ubuntu/`).
Each save keeps the current credential plus one previous copy; older owned
generations are deleted.

### Add or sign in again from a terminal on Ubuntu

```bash
ai-account-center antigravity signin <profile>
```

A new name adds a profile; a saved name signs that profile in again. Run it in
an interactive terminal on Ubuntu; SSH from the Mac or Windows is fine. The
official CLI starts inside a private sign-in home: bubblewrap with user, PID,
IPC and UTS namespaces, the root read-only, an owned folder bound over
`~/.gemini`, an empty folder over `/run/user/<uid>` and a private session bus
so the real session bus and Secret Service are out of reach, an allowlisted
environment and browser stubs. The user chooses **Google OAuth**,
opens the link in a browser, signs in to the Google account for that profile
and pastes the code back into the CLI. The command never reads the link, the
code or the screen. As soon as the new credential file is complete, the CLI is
stopped (before any task can be typed), the credential is checked with Google,
and it is saved. The live login, history and settings are never touched, and
the private sign-in folder is removed after every attempt.

- Add refuses a name already used, a 17th profile and a Google account already
  saved in another profile.
- Sign in again keeps the same Google account (email and subject) and is
  refused for the live login, because a later switch saves the live login back
  into that profile. Switch to another profile first.
- A running switch (the registry's transaction lock) refuses both, and so does
  another sign-in for the same profile.

While it runs, the command marks the profile in
`~/.ccs/antigravity-signin/<profile>.running` (its pid, process start time and
boot). Remove and Sign in again for that profile answer 409 `signin_running`
until it ends; a marker whose process is gone does not count. Ctrl+C reaches
the CLI and cancels. A closed terminal (SIGHUP) or SIGTERM stops the CLI and
removes the private sign-in folder, with any new credential in it, before the
command exits.

The transaction lock names its holder the same way and is refreshed every
minute while it is held. A lock whose holder process is gone (dead, its pid
reused, or from an earlier boot), or whose holder sent no refresh for ten
minutes, is abandoned: it does not refuse Add, Sign in again or Remove, and
the next one of them takes it over (one takeover at a time) and logs it.

The command needs `/usr/bin/bwrap`, `/usr/bin/dbus-run-session` and an
unprivileged user namespace; its preflight checks them with one harmless
`/bin/true` probe and never starts the CLI on failure.

The dashboard runs this same isolated sign-in as a supervised job (CONTRACT-
registry-lifecycle 6.2 and 6.6). When the preflight passes on the dashboard
host, `providers[].signIn` for Antigravity reads `available: true`, and
`POST /api/accounts/add` with `{provider: 'antigravity', profileName}` (and
`POST /api/accounts/antigravity:profile:<id>/signin-again` for a saved,
non-live profile) answer 202 with a `supervised-cli` sign-in job, like Codex
and Muse. A fixed Python driver (`src/antigravity/signin-driver.ts`) owns the
CLI's PTY: it presses Enter through the first-run login-method screen (Google
OAuth is the highlighted default), surfaces the one `https://accounts.google.com`
authorization URL to the page (read from the CLI's OSC 8 hyperlink, or rejoined
from its wrapped plain text), feeds the code the user pastes in the dashboard
back to the CLI with a carriage return, and stops the CLI once the new
credential is complete. The job's `complete` then imports and provider-verifies
that credential (`importSignIn`, never committing after a cancel) and
best-effort refreshes the runtime descriptor; the runner's accounts-changed
hint shows the account. The pasted code needs a trusted transport
(`submitJobCode` requires it), so Add over plain HTTP answers 403
`secure_transport_required`.

When the preflight cannot run on the host (no `agy`, no bubblewrap or no
unprivileged user namespace), `providers[].signIn` reads `available: false`
with `unavailableReason: 'preflight_failed'` (or `'tool_missing'` without the
CLI), and both routes fall back to the terminal command: 409
`preflight_failed` / `tool_missing`, or 403 `secure_transport_required`, each
carrying
`fallback: {kind: 'terminal', host: 'ubuntu', command: 'ai-account-center antigravity signin <id>'}`;
`GET /api/accounts/signin-command?provider=antigravity&profile=<id>` returns
`{host: 'ubuntu', command}`. The preflight result is cached for five minutes,
so a host that gains or loses the sandbox reflects it without a restart.

### Remove

`POST /api/accounts/antigravity:profile:<id>/remove` (with `{}` and then
`{confirmationToken}`) deletes that profile's saved snapshot and its own
credential files only. It never logs out and never touches the live login or
shared history. It refuses the live login (the same saved bytes, else the same
verified Google identity), the registry's runtime-verified active profile, a
held transaction lock or pending switch, and a running sign-in. A live-login
check that cannot run refuses with 500 `remove_failed`. A missing or damaged
saved snapshot does not block it: the live login is then compared by verified
identity. Files in the profile's folder that cannot be proven to be its own
credentials stay in place and are logged (`accounts.remove.left_in_place`).
The daily maintenance deletes credential files of profile folders no saved
profile names, for example after a crash during a Remove.

### Reading status

A reading that cannot be bound to the saved account it was taken for is an
`error` row with `statusReason: 'identity_unbound'` and is not a switch target.
Any other failed reading stays a switch target; activation makes its own proof.

## Releasing account switching

Switching is closed in this build by four independent gates:

| Gate | Where | Closed state |
| --- | --- | --- |
| Dashboard native release | `src/antigravity/production-runtime.ts`, `ANTIGRAVITY_NATIVE_RELEASED` | `false`: every activation answers `unsupported-runtime-probe` |
| Runtime release | `scripts/antigravity/runtime/release.json`, `nativeActivationReleased` and `nativeProofReceiptSha256` | `false` and `null`: install and adoption refuse, `agy` runs unmanaged |
| Runtime installation and adoption | `~/.ccs/antigravity-switching/runtime-installation.json`, the bundle, the PATH blocks, the status-line hook and `ai-account-center-antigravity.service` | not installed |
| Automatic switching | the `enabled` setting (`PUT /api/antigravity/auto-switch`) | `false` (default) |

Both saved profiles (`gmail` and `party`) must already be in the registry;
`ai-account-center antigravity signin <profile>` adds a missing one.

### Exact steps to open the gates

Run these on Ubuntu as the dashboard user, with Antigravity idle (no `agy`
process). `$PKG` is the installed package,
`~/.local/lib/node_modules/@sittingmongoose/ai-account-center`.

`ai-account-center antigravity status` reports each gate (read-only, no
credential, no network) and names the next step; run it before step 1 and
after each step.

1. Read-only preflight. Stop if any check fails.
   - `sha256sum ~/.local/bin/agy` prints
     `19be6af38f7beeaa0db415df9297e314ab3d33fdd6f853434d49f88819bc68e4` (agy
     1.3.0, the current pin; 1.2.14 and 1.2.16 stay in the reviewed set).
   - `pgrep -u "$USER" -x agy` prints nothing.
   - `python3 -I "$PKG/scripts/antigravity/install_runtime.py" --plan` prints
     `"nativeActivationReleased": false` and `"installed": false`.
   - `GET /api/antigravity/profiles` lists `gmail` and `party`, both
     `available: true`.
2. Release commit, on the branch that is deployed next:
   - Write the release receipt (who approved, when, the preflight results; no
     secret) and take its SHA-256 as `<receipt>`.
   - In `scripts/antigravity/runtime/release.json` set
     `"nativeActivationReleased": true` and
     `"nativeProofReceiptSha256": "<receipt>"`.
   - In `scripts/antigravity/runtime/runtime-manifest.json` set the `release.json`
     row to the new `sha256sum scripts/antigravity/runtime/release.json`
     (`python3 -c "import sys; sys.path.insert(0, 'scripts/antigravity'); from pathlib import Path; from install_runtime import verify_public_sources; verify_public_sources(Path('scripts/antigravity/runtime'))"`
     must then pass).
   - In `src/antigravity/production-runtime.ts` set
     `ANTIGRAVITY_NATIVE_RELEASED = true`.
   - The two tests that pin the closed gate change with it:
     `tests/unit/antigravity/production-native-pin.test.ts` and
     `tests/unit/antigravity/production-usage-only.test.ts`
     (`expect(ANTIGRAVITY_NATIVE_RELEASED).toBe(true)`).
   - Run the gates, build, `npm pack`, install the package and restart
     `ccs-dashboard` with the approved commands.
3. Install the runtime bundle (it downloads the two hashed parser wheels from
   PyPI): `python3 -I "$PKG/scripts/antigravity/install_runtime.py" --apply`.
4. Adopt it: `python3 -I "$PKG/scripts/antigravity/adopt_runtime.py"`. This adds
   the PATH block to `~/.bashrc` and `~/.profile`, multiplexes the status-line
   command in `~/.gemini/antigravity-cli/settings.json`, and enables and starts
   `ai-account-center-antigravity.service`. Originals are journaled in
   `~/.ccs/antigravity-switching/runtime-adoption.json`.
5. `systemctl --user restart ccs-dashboard`: the dashboard picks the installed
   runtime only at start.
6. Verify: `ai-account-center antigravity status` shows both gates open, the
   runtime installed and adopted and its service socket present, and
   `GET /api/antigravity/profiles` reports `activationSupported: true`.
   New shells resolve `agy` to the managed launcher
   (`command -v agy` is `~/.local/share/ai-account-center/antigravity-runtime/bin/agy`).
7. Live test (approved for the orchestrator only): with no `agy` running,
   `POST /api/antigravity/profiles/party/activate` with `{"hostId": "ubuntu"}`
   answers 200 `{"status": "active"}`; then the same for `gmail`. A
   `confirmation-required` answer (managed idle sessions) is confirmed with
   `POST /api/antigravity/profiles/<id>/confirm` and its token. Any other
   answer means switching stays off: roll back. The same guarded switch runs
   locally as `ai-account-center antigravity activate <profile>` (owning user,
   interactive terminal): it lists running programs for review first.
8. Automatic switching, after the live test passed:
   `PUT /api/antigravity/auto-switch` with
   `{"enabled": true, "requestedPoolId": "<a pool both accounts report>"}`
   (threshold default 95% used).

### When the official CLI auto-updates past the reviewed pin

An official update invalidates the pin on purpose: activation answers
`unsupported-runtime-probe`, and status and the dashboard say
`Antigravity updated to X; switching paused until reviewed` instead of failing.
Releases follow the same review the 1.2.16 and 1.3.0 receipts record
(`status/AGY-RELEASE-RECEIPT-1.2.16.md`, `status/AGY-RELEASE-RECEIPT-1.3.0.md`): binary identity and ownership,
changelog, storage layout and permissions, token format and lifecycle, process
names, status vocabulary and credential backend, all with read-only probes and
no secrets printed. If the new build is compatible, the release commit appends
its version and hash to the `reviewedNatives` set in
`scripts/antigravity/runtime/release.json`, moves the current
`nativeVersion`/`nativeSha256` pin to it, refreshes the manifest row, and points
`nativeProofReceiptSha256` at the new receipt; then the installed runtime is
refreshed to the new build before any activation:
`ai-account-center antigravity runtime refresh` re-pins the installation
descriptor, and `python3 -I "$PKG/scripts/antigravity/rebuild_bundle.py" --apply`
rebuilds the installed bundle when it still pins the old build (`--plan`
previews read-only; `--recover-rebuild` removes an interrupted rebuild after
review). The rebuild refuses unreviewed binaries, foreign files and running
switches, keeps the previous bundle and descriptor bytes, and restores them if
verification fails. Status and the live test run again after it.

After deploying a re-pinned release, the install descriptor still names the
previous pin until it is refreshed. The first switching proof refreshes it
automatically when the installed CLI is a reviewed build (the descriptor is
re-pinned atomically with the previous pin kept as a backup); an unreviewed
build stays paused. `ai-account-center antigravity runtime refresh` runs the
same refresh explicitly: it re-verifies the installed CLI against the reviewed
set with the adoption proofs (owned descriptor, bundle gate, exact paths) and
reports `current`, `refreshed`, or the paused state. It runs only as the user
that owns this computer's Antigravity state. The previous descriptor is kept as
`descriptor-backups/<old pin>.json`; when an earlier generation already keeps
different bytes under that name (a bundle rebuild keeps the pin), this one is
kept beside it as `<old pin>-<sha16>.json`, the name the rebuild uses. Existing
backups are never overwritten.

### Update all holds unreviewed builds

The dashboard's Update all never installs an Antigravity CLI build that is not
in the reviewed set, on any of the four computers it updates (Ubuntu, Mac,
Windows and Nas1). Account switching exists on Ubuntu only: the other
computers, Nas1 included, are only ever updated. The helper reads the
official release manifest first; when its newest version is not reviewed, the
row reads "Update held: Antigravity X is waiting for a switching review" and
the installed build stays. When the review list or the manifest cannot be read,
nothing is installed either. A version check is the right gate here: the
manifest names a version and a hash of the download archive, not the hash of
the installed binary the review pins, and the official updater cannot install a
chosen version, so the only safe choices are "update to a reviewed version" or
"hold". The switching gate itself
stays hash-based: a held or skipped update changes nothing, and a binary that
somehow differs from its reviewed hash still pauses switching. No automatic
compatibility check replaces the review: several reviewed surfaces (token
renewal, the signed-in header, live status frames) need a real login, which an
unattended updater must not exercise. The CLI's own background self-update
during ordinary runs is outside Update all and still pauses switching until a
review (`scripts/app-updates/README.md`).

### Rollback

`python3 -I "$PKG/scripts/antigravity/adopt_runtime.py" --rollback` restores
the shell profiles and status line and disables the service. Reinstall the
previous dashboard package (its gates are closed) and restart `ccs-dashboard`.
The installed bundle and descriptor stay inert while the dashboard gate is
closed. A bundle rebuild keeps the previous bundle and descriptor bytes in
place, and the adoption rollback still restores the profiles, status line and
service afterwards. Saved profiles and the live login are unchanged by every
step above except the approved activations themselves.
