# CCS OpenCode Zen browser usage bridge

This small extension reads the existing sign-in through Brave/Chrome's normal cookie API. It avoids macOS Keychain extraction and Windows app-bound-encryption workarounds. It does not log in, reset limits, consume credits, change billing settings, restart apps, or alter browser policy.

The browser sends only `auth` and `__Host-console_session` cookies for the exact HTTPS `opencode.ai` origin, `/` path, with the real cookie expiration. A same-user native Python host validates the live workspace list, reads that workspace's prepaid wallet, and optionally its Go usage. Verified credentials are stored only in the owning Mac user's private directory and 0600 capsule for background CCS refresh. Credentials and request/response bodies are never written to evidence, stdout logs, or extension storage. The normal native messaging transport necessarily carries the cookie in its private browser-to-host request.

The console workspace is kept separate from an existing API-key Go account. Go's API-key usage endpoint does not expose a workspace identifier, so provider names alone cannot prove that two sources own the same wallet. The normalizer emits a distinct account ID with an opaque workspace hash. It selects the signed-in workspace from the open console URL or from a single returned workspace; multiple workspaces require the user to open the desired billing page.

## Install on the Mac

Run `install-macos.py` with the existing private Python environment containing pinned `curl_cffi==0.16.3`. It installs source files under `~/.ccs/opencode-usage-bridge/` and standard user-level Brave/Chrome native messaging manifests. It does not force-install the extension.

In **the Mac Brave profile already signed in to OpenCode**, open `brave://extensions`, enable Developer mode, choose **Load unpacked**, and select:

`~/.ccs/opencode-usage-bridge/extension`

Open that workspace's OpenCode billing page, open **CCS OpenCode Zen Usage Bridge** from Extensions, and choose **Sync usage**. The popup displays the wallet and only the actual reset/expiry fields returned by the provider. After a successful sync it rereads the browser session every two hours while Brave is running. Failed refreshes do not turn the old sample into a fabricated zero.

Extension ID: `nkabpjmpmbdklhknjnjgmapmcjcmmlop`. The pinned public key is public; the generation key was discarded. Permissions are `cookies`, `nativeMessaging`, `storage`, `activeTab`, `alarms`, scoped to `https://opencode.ai/*` and `https://dev.meta.ai/*`.

## Background collector integration

The installed launcher accepts `--collect` to read the same private capsule and emit one normalized DashboardAccount. Installed path:

`~/.ccs/opencode-usage-bridge/native-host/launch-host.sh --collect`

Private capsule: `~/.ccs/account-usage/opencode-console-session.json`, schema 1, exact origin/source, validated `workspaceId`, `capturedAt`, and cookie objects. A capsule older than 24 hours or a cookie past its own expiration requires another normal browser sync. The collector always checks live workspace membership before reading a wallet. Failed or denied requests return unavailable; zero and negative signed balances are valid.

Only fixed GET routes are used: `/console/api/orgs`, `/console/api/billing/status`, and optional `/console/api/go/status`. Workspace requests use `x-org-id`. Prepaid `balanceMicroCents` is divided by 100,000,000 to produce USD, as in the current console parser. No expiration is inferred from a balance or subscription date. Non-prepaid workspaces are reported explicitly instead of pretending they own prepaid Zen credits. Legacy SolidStart auth is not migrated or treated as current console auth.

## Verification

`python3 -m unittest discover -s tests -p 'test_*.py'`

`node --test tests/bridge-core.test.mjs`

The Python tests include a complete native-framed request/response using a synthetic cookie/provider fixture, schema rejection, workspace selection/mismatch, genuine zero/negative/fractional balances, signed units, missing versus malformed expiration, independent console meter windows, secret-free sample output, capsule permissions/symlinks/expiry, and no writes when verification fails. Node tests verify exact cookie scope, URL-selected workspace, projection redaction, and rejection of malformed native responses.
# Muse Code support

Version 1.1 adds the exact `https://dev.meta.ai/*` scope to this same extension.
Choose **Sync Muse usage** in its popup. It reads only the existing Muse portal
session cookies, verifies the portal email against the live Muse CLI account,
and checks the selected team's subscription tier. A single team is selected
automatically; multiple teams require an explicit choice in the popup.

This performs GET requests to the portal's account, teams and subscription-quota
routes. The CLI device credential is used only with `api.meta.ai/muse-code/key`.
No prompt or model request is made. The captured web session remains on its Mac
in an owner-only directory and file. Dashboard output contains only account
identity, weighted token usage, actual percentages and reported reset times.
The regular Muse collector then refreshes live portal quotas from that session.

If the existing browser sign-in is unavailable, the dashboard keeps the
confirmed account and plan and explains the missing quota. It never invents
zero usage or treats an omitted key-response quota as a failed CLI login.
