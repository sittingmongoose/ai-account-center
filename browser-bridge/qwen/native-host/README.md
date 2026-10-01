# AI Account Center Qwen usage Native Messaging host

`CCS.QwenUsageBridge.exe` is a framework-dependent .NET 8 Windows console host.
The browser must be the installed AI Account Center bridge extension with the pinned origin
`chrome-extension://clobbdmblhillanldmmjnlpbaafbnklj/`.
It receives exactly one standard Chromium Native Messaging request (four-byte
little-endian JSON byte length followed by UTF-8 JSON) and sends one framed,
safe response. Normal operation writes nothing else to stdout or stderr.

The only input operation is:

```json
{"action":"collect","region":"intl","cookies":[{"name":"login_qwencloud_ticket","value":"browser-supplied-value","domain":".qwencloud.com","path":"/","secure":true}]}
```

`region` may be `intl` or `cn`; `expirationDate` is optional Unix seconds.
Only the cookie fields shown plus `expirationDate` are accepted. Unknown or
duplicate fields, non-allowlisted domains, header delimiters, controls, more
than 200 cookies, or messages above 256 KiB are rejected. The host only accepts
its pinned extension caller plus Chromium's optional numeric `--parent-window`.

Console and usage gateway cookies are projected independently for their fixed
HTTPS hosts and paths. Non-expired host-only and path-scoped console cookies are
retained for the console without being forwarded to the gateway. The console
header must be nonempty; the gateway header may be empty because the gateway
can authorize the read with the console's `SEC_TOKEN`. No named cookie or
shared-domain ticket is required: Qwen verifies the existing session through
its read-only user-info/page response.

The domain allowlist comprises root scopes `qwencloud.com`, `.qwencloud.com`,
`aliyun.com`, `.aliyun.com`, and exact fixed-host scopes (with or without a
leading dot): `home.qwencloud.com`, `cs-data.qwencloud.com`,
`bailian.console.aliyun.com`, `bailian-cs.console.aliyun.com`. Other scopes such
as `.console.aliyun.com` are rejected. Each header is bounded to 32,768 ASCII
characters; their encrypted internal JSON is bounded to 70,000 UTF-8 bytes,
and the DPAPI result to 128 KiB.

The user-local session is encrypted with Windows CurrentUser DPAPI and
`CRYPTPROTECT_UI_FORBIDDEN`, with no optional entropy or machine-wide flag.
It is atomically written to the fixed path:

```
%USERPROFILE%\.ccs\account-usage\qwen-console-session.json
```

Its v2 grammar is
`{ "version": 2, "region": "intl" | "cn", "cookiesDPAPI": "base64-DPAPI-blob" }`.
The decrypted internal UTF-8 JSON is exactly
`{ "consoleCookie": "console-scoped Cookie header", "gatewayCookie": "gateway-scoped Cookie header or empty string" }`.
The deployed Python collector must support this v2 schema before the host is
installed. The native host writes v2 only and never writes the legacy
single-header `cookieDPAPI` capsule.
New temporary and destination files grant access only to the current user and
SYSTEM before any contents are written. Plaintext is never written to files,
arguments, environment, logs or browser replies. The bridge changes no browser
or provider authentication state and makes no login, refresh or inference call.

The fixed subprocess is:

```
C:\Program Files\Python313\python.exe -E -s -X utf8 "%USERPROFILE%\.ccs\account-usage\plan_usage.py" --provider qwen --platform windows
```

The Python helper and its `plan_common.py` companion must already be deployed by
the parent AI Account Center installation. No caller can choose a program, script, host, path,
provider or URL. Both child pipes are bounded at 65,536 characters; stderr is
discarded. The child has a 60-second deadline and is killed on failure/timeout.
A global, user-SID-specific mutex serializes the capsule write, usage read and
normalized-cache write across browser instances, waiting at most five seconds.

The Python collector reads Qwen's fixed read-only usage, add-on summary,
subscription and quota-config endpoints. The C# layer independently projects
the result into an explicit safe `DashboardAccount`: seven fixed window keys
plus up to 100 individual credit packs with keys matching exactly
`addon-pack-[0-9a-f]{12}` (107 rows total),
fixed labels/source/capabilities, finite nonnegative numbers, validated UTC
timestamps and allowlisted plan labels. Subscription and individual credit-pack expiry remain `expiresAt`
and is not displayed as a quota reset. Unknown limits/percentages remain null.
No raw upstream or subprocess error string is returned. A success response is
`{"ok":true,"sample":<DashboardAccount>,"error":null}` and the same normalized
sample is atomically saved as `qwen-browser-usage.json` in that fixed directory.
An error response has `ok:false`, `sample:null` and a classification such as
`needs_sign_in`, `invalid_request`, `invalid_caller`, `busy`, `timeout`,
`collector_unavailable`, `collector_error`, `invalid_response`,
`credential_protection_error`, `cache_write_error` or `unavailable`.

The fixed `--collect-existing` diagnostic command accepts that single flag
only, runs on Windows as the current user, and acquires the same collection
mutex. It runs the fixed Python collector against the existing encrypted
session and writes only the normalized usage cache. It does not replace the
session capsule or accept cookies, script paths, URLs or other actions. Its
stdout is one ordinary JSON `NativeReply` instead of browser framing; raw
errors and credentials remain excluded. Normal browser requests still require
the pinned extension caller and standard framing.

Build and test using a .NET 8 SDK (publishing does not install or register it):

```powershell
dotnet publish .\CCS.QwenUsageBridge.csproj -c Release -r win-x64 --self-contained false -o .\publish
.\publish\CCS.QwenUsageBridge.exe --self-test
```

The `--self-test` exception is an offline command with synthetic fixtures only.
It covers framing, caller/input/header guards, independent domain/path/expiry
projection, empty gateway headers, encrypted v2 grammar, combined-header
capacity, safe scalar projection, real limits/credits/expiry, private file ACLs,
concurrent transaction locking and same-user DPAPI roundtrips. It uses no saved
credentials or network and writes
no capsule or usage cache. Native manifest/extension registration is owned by
the parent bridge installer outside this directory.
