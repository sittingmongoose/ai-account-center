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

The parser requirements are `pyte==0.8.2` and `wcwidth==0.9.1`, installed into
that bundle's virtual environment using the packaged wheel hashes. The runtime
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

Adoption proposes a launcher PATH block at the top of `.bashrc` and `.profile`,
multiplexes the existing native status-line command and creates the user service
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

## Manual and automatic control

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
