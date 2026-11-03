# Claude sign-in renewal

Keeps every Claude Code sign-in alive on every computer AI Account Center covers, with a person needed
only for a CAPTCHA.

## Why sign-ins end

Each Claude Code OAuth sign-in has a fixed end, `claudeAiOauth.refreshTokenExpiresAt`, about 30 days
after it was made. Refreshing the access token (every ~8 hours) does not move it. Once it passes, the
next refresh fails with "OAuth session expired and could not be refreshed", Claude Code blanks the
tokens, T3 marks that provider "not authenticated" and its threads stop.

## What the runner does

`claude_signin.py tick` runs every 10 minutes on one computer, the one with the browser and the
person (the Mac). Each run:

1. **Reads** every sign-in on every computer in its settings: this one locally, the others over ssh
   (the script streams itself over ssh stdin; nothing is installed there). It reads the end time,
   the access-token time and the account email. On the Mac it reads Keychain items
   `Claude Code-credentials-<first 8 hex of sha256(config dir)>` (`Claude Code-credentials` for
   `~/.claude`), which only a process in the Mac's desktop session can do; elsewhere
   `<config dir>/.credentials.json`.
2. **Renews** each sign-in whose end is within `renewWithinHours` (96) or that is already logged out,
   grouped by account. It runs `claude auth login --claudeai` for that profile under a terminal (a
   pty here, `ssh -tt` there), opens the printed link in that account's own Chrome profile
   (`browser_auth.mjs`, over the DevTools protocol), checks the page says "Logged in as <email>",
   clicks Authorize, types the code it shows into the prompt and checks that the saved end moved
   later. On Windows it sets `CLAUDE_CONFIG_DIR=X&&` with no space, so Claude saves to the right
   folder.
3. **Verifies** each renewal again `verifyAfterHours` (9) later and 9 hours after the old end: a
   Claude process that was already running can write its old sign-in back. A sign-in that went back
   is renewed again at once; one that keeps going back is reported.
4. **Publishes** `claude-signins.json` and `claude-signins.md` into the usage snapshot folder. The
   dashboard shows them (Claude rows, Details, a notice) and `aac-usage signins` prints them.

Failures back off (1, 2, 4 hours, at most 6). A sign-in that keeps failing within a day of its end is
reported as needing a person.

## When a person is needed

The Chrome profiles stay signed in to claude.ai, so most renewals need nobody. A claude.ai browser
session lasts about 28 days from its sign-in, a little less than a Claude Code sign-in, so the
runner tracks when each account's session ends (the session cookie's expiry, never its value; every
6 hours, published as `browserSessions`) and once a day opens claude.ai briefly to learn whether a
visit extends it (`visitExtends`). Chrome starts hidden and is quit after each run unless a person
is working in it. Two things only a person may do:

- **A signed-out profile.** The runner enters the email on claude.ai/login, chooses "Continue with
  email", reads the `https://claude.ai/magic-link#...` link from the mail (Mail.app on the Mac, or
  IMAP) and opens it. claude.ai then shows an hCaptcha.
- **Cloudflare's "Verify you are human".** After many sign-ins from one network in a short time,
  claude.ai can put this check in front of every page, signed in or not.

When a person passes one of these, the runner at once signs in every other account whose
browser session ends within 5 days, so those CAPTCHAs come in the same sitting rather than on
separate days.

In both cases the runner brings that Chrome window forward, posts a desktop notification, publishes
"needs you" (dashboard notice, `claude-signins.md`, `aac-usage`), waits up to `humanWaitMinutes`
(20), and carries on by itself once the check is passed. If nobody passes it in time, the page stays
open, the account's other sign-ins wait, and the next run picks up where it left off (a fresh link
at most every 50 minutes, a reminder every 6 hours). Agents must not solve these checks.

## Set up

```sh
python3 claude_signin.py install     # copies itself to <CCS dir>/claude-signin/app and schedules tick
python3 claude_signin.py doctor      # settings, ssh to each computer, Node, publish folder, schedule
python3 claude_signin.py status      # every sign-in and when it ends
python3 claude_signin.py renew --dry-run
python3 claude_signin.py renew --host windows --profile party --force   # renew one now
python3 claude_signin.py uninstall
```

`install` uses launchd on the Mac (in the desktop session, so the Keychain and Chrome work), a systemd
user timer on Linux and Task Scheduler on Windows (while signed in). Install it on **one** computer.
The installed copy updates itself from the `claude-signin/` folder the dashboard keeps in the usage
snapshot folder.

Needs Python 3.9+, Node 22+ (built-in WebSocket) and Google Chrome on the runner; Python and Claude
Code on each covered computer; ssh keys from the runner to each of them.

### Settings: `<CCS dir>/claude-signin/config.json`

Private to this computer (0600). Host names, addresses and emails live only here.

```json
{
  "hosts": [
    {"name": "mac", "os": "mac", "ssh": "me@mac.local", "hostnames": ["my-mac"]},
    {"name": "ubuntu", "os": "linux", "ssh": "me@vm", "hostnames": ["vm-host"]},
    {"name": "windows", "os": "windows", "ssh": "me@pc", "hostnames": ["pc"]}
  ],
  "ignoreProfiles": ["retired"],
  "skip": ["windows/default"],
  "publishDir": "/Volumes/share/aac-usage",
  "mail": {"default": {"kind": "mailapp"},
           "me@example.com": {"kind": "imap", "host": "imap.example.com", "user": "me@example.com",
                               "passwordFile": "~/.ccs/claude-signin/imap-password", "folder": "INBOX"}}
}
```

Other keys: `renewWithinHours`, `verifyAfterHours`, `humanWaitMinutes`, `chromeProfilesDir` (default
`<CCS dir>/claude-signin/chrome/<email>`; a profile left by the earlier `claude-relogin` tool in
`~/.claude-relogin/chrome/<email>` is used where it is), `notify`, `nodePath`, `claudePaths`
(`{"<host>": "<path to claude>"}`). Use the AI Account Center computer names (`mac`, `ubuntu`,
`windows`, `nas1`) so the dashboard matches the rows.

## Privacy

Only times, account emails, profile and computer names and fixed state words are written to the
state file, the log or the share. Codes travel from the browser helper to the login prompt through a
pipe and are never logged; links and URLs reach the browser helper on stdin, never on a command line.
The login prompt's output is kept in memory only. Logs pass through a scrubber that removes anything
shaped like a link, code or token.

## Checking it end to end

`claude_signin.py test-login --host <name> --account <email> --dir <throwaway folder>` signs a
throwaway `CLAUDE_CONFIG_DIR` in on that computer through the whole flow and checks the saved end,
without touching a real sign-in. Remove the folder afterwards (and on the Mac its Keychain item,
`security delete-generic-password -s "Claude Code-credentials-<sha8>"`).
