# Setting up AI Account Center for someone (agent guide)

For an agent asked to install AI Account Center (AAC) on a person's computers and connect it to their agents. You do
the commands; the person does the sign-ins. Read the whole page first, then work through it in order, and tell the
person each time you need them.

## What it gives them

- One dashboard with the quota left on every AI account they use (Claude, Codex, Antigravity, Cursor, Muse Code, Kimi
  Code, Qwen, Z.ai, OpenCode Go), refreshed every minute.
- A usage snapshot their agents read before long runs ([agent usage snapshot](agent-usage-snapshot.md)).
- If they use T3 Code: AAC's readings as T3's own quota bars, through the usage hub ([T3 usage hub](t3-usage-hub.md)).

## 1. Decide where the dashboard runs

Ask which computer stays on: the dashboard runs there. Codex and Antigravity switching need it on Linux; every reading
works from macOS or Windows too. Other computers are reached over SSH for their own sign-ins, Analytics and updates
(README, "How it works").

## 2. Build and install (you)

Plan for it, and tell the person before you start: **a source build of about 30 minutes and 5 GB** on a 4-core
computer (most of it one Rust compile), with four toolchains.

1. **Toolchains,** the versions in the README's **What you need**: Node.js 22.13+ or 26 (CI installs 26), Bun 1.4.4, Rust 1.99.1 with
   `wasm32-unknown-unknown`, and wasm-pack 0.15.1. On a Mac, Homebrew's `node` is fine when it is 22.13 or newer.
   `brew install node@22` still works when you want the older end of that range. A fresh Mac needs the Command Line
   Tools first, and Homebrew asks for the person's password in a terminal: those are their steps.
2. **Build:** the README's **Get started** step 1 (clone, `bun install`, `bun run build:all`).
3. **Install:** `./scripts/dev-install.sh --npm`. It runs AAC's own checks before installing (typecheck, lint and
   about 4,500 tests: a few minutes). They pass on a clean Linux or macOS account with no AI tools installed. If one
   fails anyway, don't change AAC to get past it: report the failing test names to the person (and as an issue on the
   repository), and use the next item meanwhile.
4. **Without installing:** the build already runs from the checkout. Use `node dist/ccs.js` wherever this guide says
   `ai-account-center` (for example `node dist/ccs.js dashboard auth setup`). Install later, once the checks pass.

Docker is the alternative (`docker/README.md`). It builds the same source.

## 3. Human steps (the person)

These need the person at the keyboard. Say what each is for, then wait:

1. **Dashboard login:** `ai-account-center dashboard auth setup`. They choose the username and password; never ask
   for it in chat and never write it down.
2. **Accounts:** start the dashboard (`ai-account-center dashboard --host localhost --port 3000 --no-open`), open
   `http://localhost:3000`, and have them add each account under **Accounts & Settings → Add account**, signing in to
   each provider in the browser. A Codex login already on the computer can be saved with
   `ai-account-center codex-auth import-default <name>`.
3. **Trays** (optional): the Mac menu-bar app and the Windows tray (README step 4). They give the tray the dashboard
   address and password once.

## 4. Find what they have (you)

Don't assume a list of providers. After the sign-ins, read what the dashboard found: the Accounts page, or the
snapshot once step 5 is on (`aac-usage` lists every account with provider, email, profile, computer and plan). Report
back which providers and accounts showed up, and which sign-ins are still missing.

## 5. Usage for agents (you)

Pick a folder every computer the agents use can read (a network share, or a local folder on one computer):

```bash
ai-account-center dashboard usage-snapshot set /path/to/shared/aac-usage \
  --location "<computer or group>=<path there>"   # one per place the folder is mounted differently
python3 /path/to/shared/aac-usage/aac-usage install-rules   # on each computer the agents use
```

`install-rules` adds a short managed block to each agent's global instructions, so every agent knows to check quota
before long or parallel runs. Re-running is safe.

## 6. T3 Code usage hub (you, then the person pastes one key)

Only if they use T3 Code. On the dashboard computer:

```bash
ai-account-center dashboard usage-hub generate --stdout
```

The key is printed once. Don't paste it into chat: with T3's agent tools, ask for it with `request_secret` and have
the person enter it there; otherwise have them copy it straight from the terminal. In T3, on each T3 environment:
**Settings → Providers → Usage providers → Add hub**, hub URL `http://127.0.0.1:3000` on the dashboard computer (or
`http://<dashboard LAN address>:3000` from another computer, with "trusted local network" on in AAC's Settings), the
key, label `AI Account Center`. The bars appear under **Usage → Limits** after T3's next poll.

## 7. Check

- `ai-account-center dashboard usage-hub status` says on, and the dashboard's request log shows
  `GET /v0/management/auth-files` 200 from each T3 computer.
- `aac-usage` on each agent computer lists the accounts, none marked stale.
- In a new agent thread, the instructions contain the AAC usage block.

## Remove

`ai-account-center dashboard usage-hub off`, `ai-account-center dashboard usage-snapshot off`, remove the hub in T3's
settings, and delete the `aac-usage rules` block from the agents' instruction files (it sits between its own markers).
