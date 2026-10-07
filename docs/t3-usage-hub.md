# T3 usage hub

T3 Code can show subscription quota bars ("Usage, Limits") for accounts it
cannot run turns on, through a CLIProxyAPI hub. AI Account Center answers the
two hub requests T3 makes, so T3 shows the 5-hour and weekly bars of every
AAC-managed Codex and Claude account. The readings are the dashboard's own,
from its cache.

Source: `src/web-server/usage-hub/`. Command: `src/commands/usage-hub-command.ts`.

## What it answers

The hub is mounted at `/v0/management` on the dashboard's own listener. T3
builds each URL as `new URL('/v0/management/' + path, hubUrl)`, so any path in
the hub URL is dropped: give T3 the dashboard address with no path.

| Request | Answer |
| --- | --- |
| `GET /v0/management/auth-files` | `{ files: [...] }`, one entry per Codex and Claude account: `id` (the AAC account id), `auth_index` (stable, opaque), `provider`, `email`, `disabled: false`, plus `label`, `status`, `status_message` and `sampled_at` for people reading it |
| `POST /v0/management/api-call` for `https://chatgpt.com/backend-api/wham/usage` | `{ status_code: 200, body }`, `body` the Codex `wham/usage` JSON: `plan_type`, `rate_limit.primary_window` (5-hour) and `secondary_window` (weekly), each `used_percent`, `reset_at` (epoch seconds), `limit_window_seconds` |
| `POST /v0/management/api-call` for `https://api.anthropic.com/api/oauth/usage` | `{ status_code: 200, body }`, `body` the Claude `oauth/usage` JSON: `five_hour`, `seven_day` (`utilization` 0-100, `resets_at`) and `limits[]` for the Opus, Sonnet, Fable and Haiku weeklies |
| An account with no usable reading | `{ status_code: 503, body }`: T3 marks only that account "probe failed" |
| Codex reset credits, their redemption, `reset-quota`, any other URL, method or body | 403 `unsupported_api_call` |
| Any other management path | 404 |

Accounts hidden on the dashboard or in the trays are still listed: hiding is
display only. Claude profiles still waiting for their first sign-in are left
out.

It is not a proxy. No upstream request is ever made: the headers T3 sends
(including its `$TOKEN$` placeholder) are never used, and every request outside
the two usage reads is refused. A window whose reset time has passed is left
out rather than shown stale, and no reading is ever invented.

The account list comes from `peekAccountDashboard`, which never starts a
collection. The dashboard's own refresh (the Analytics sampler, every refresh
interval, for the Mac and Windows views) keeps the cache current, so a T3 poll
(every 5 minutes, plus on settings changes and Limits refresh) never adds a
provider quota read. T3 stamps its own `checkedAt`, so the age of AAC's
reading (up to one refresh interval) is not visible in T3; `sampled_at` in
`auth-files` shows it.

The contract is mirrored from T3's client, `apps/server/src/usage/cliproxyApi.ts`
in pingdotgg/t3code (tag `v0.0.46-nightly.20261006.2735`); see
`src/web-server/usage-hub/usage-hub-contract.ts` for line references.

## Security

- **Off until a key exists.** Without a key every request is 404
  `usage_hub_off`.
- **The key.** `aacu_` plus 32 random bytes (base64url). Only its SHA-256 is
  stored, in `~/.ccs/auth/usage-hub-key.json` (folder 0700, file 0600, atomic
  writes through the dashboard's auth write gate), like paired-device token
  hashes. The key is shown once, when it is made. The server reads the file on
  every request, so a new, rotated or removed key applies without a restart.
- **Comparison.** The presented key is hashed and compared with
  `crypto.timingSafeEqual` against the stored 32-byte digest, so timing never
  depends on the key.
- **Headers only.** `Authorization: Bearer <key>` (what T3 sends) or
  `X-Management-Key`. A key in the query string is refused (400) and redacted
  from the request log.
- **Transport.** The dashboard's `isSecureTransport` rule applies before the
  key is read: loopback with a loopback `Host`, in-process TLS, a trusted local
  TLS proxy, or a peer inside `dashboard_network.trusted_networks` while
  `dashboard_network.trust_local_network` is on (the "trusted local network"
  switch in Settings, which can only be turned on from the dashboard computer).
  Requests that carry proxy headers never count as local network peers.
  Anything else gets 403 `secure_transport_required`. So loopback always
  works, and other computers on the LAN work only when the owner trusts the
  local network. A request that came through a `lan-https-proxy` is answered
  404 before any of this, so the hub never faces the internet.
- **Separate from dashboard sign-in.** The key opens only these read-only
  routes. Dashboard passwords, sessions and device tokens never open them, and
  the key opens nothing else.
- **Rate limits.** 120 requests per minute per client address, and 10 refused
  keys per 15 minutes per client address (both 429 with `Retry-After`).
- **What leaves.** Account id, label, email, plan type and quota numbers. Every
  answer is built field by field from an allowlist: no tokens, refresh tokens,
  cookies, credential contents, paths, sources or collector messages. Answers
  are `Cache-Control: no-store`.

## Turning it on

On the computer that runs the dashboard:

```bash
ai-account-center dashboard usage-hub status            # on or off, fingerprint, URLs, LAN access
ai-account-center dashboard usage-hub generate --stdout # make the key; it is printed once
ai-account-center dashboard usage-hub rotate --stdout   # replace it; the old key stops working
ai-account-center dashboard usage-hub off               # remove it; the hub answers 404
```

`generate` and `rotate` write only the key to standard output, and only with
`--stdout` (or `--print-once`); every message goes to standard error. Pipe the
key where it is needed instead of leaving it in a terminal or a file. A lost
key is rotated, never shown again.

In T3, on each T3 environment that should show the bars: **Settings, Providers,
Usage providers, Add hub**, with:

- **Hub URL:** `http://127.0.0.1:3000` when T3 runs on the dashboard computer,
  or `http://<dashboard LAN address>:3000` from another computer (no path; the
  port is the dashboard's).
- **Management key:** the key.
- **Label:** `AI Account Center`.

T3 stores the key in its server secret store (`<T3 home>/userdata/secrets/`)
and keeps only a redaction marker in `settings.json`
(`usageLimitSources.cliproxy-<host>-<port>`). The bars appear under
**Usage, Limits** after the next poll; the dashboard's request log shows
`GET /v0/management/auth-files` with status 200 from that computer.
