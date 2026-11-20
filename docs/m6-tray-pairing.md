# Supervised M6 tray pairing

Use a staging URL served by the **composed Rust dashboard**. Open its Home page
from both computers and confirm it serves usage and **Settings → Dashboard
sign-in**. Use an HTTPS URL trusted by both computers and a disposable dashboard
login. Use unpaired test trays, or trays already pointed at this staging URL.

1. **Mac.** Click the AI Account Center menu-bar icon. From **Connect this Mac**,
   enter the staging URL and click **Continue**. Enter the dashboard username and
   password, then click **Pair this Mac**. If already paired to staging, start at
   the gear → **Connection → Re-pair**. Check that the usage panel opens and
   **Connection** says **Paired as Mac tray** with a recent **last synced**.
2. **Windows.** Open the AI Account Center notification-area icon, using the
   overflow arrow if needed. From **Connect this PC**, enter the same URL and
   click **Continue**. Enter the dashboard username and password, then click
   **Pair this PC**. If already paired to staging, start at the gear →
   **Connection → Re-pair**. Check **Paired as Windows tray** and **last synced**.
3. **Restart Rust once.** Leave both trays running. In the staging server
   terminal, stop the foreground Rust process with **Ctrl+C** and repeat its
   launch command with the same private root, session secret, address and TLS
   settings. Wait for **GET /api/health** to return 200 at the original URL.
   Click **Refresh usage** in the Mac panel and **Refresh now** in the Windows
   tray icon's right-click menu. Verify that both **last synced** times advance
   and both trays open usage without prompting for the dashboard password.
4. **Revoke Mac.** In the dashboard browser, open **Settings → Dashboard sign-in
   → Paired tray apps**. Click **Revoke** on the Mac row and **Revoke** again in
   its inline confirmation. Trigger a Mac refresh. Check **This tray was signed
   out** and **Pair again**. Refresh Windows and confirm it still syncs.
5. **Revoke Windows.** Use the two **Revoke** clicks on the Windows row, then
   **Refresh now**. Check **This tray was signed out** and **Pair again**. Reload
   the dashboard settings and check that both test rows are gone.

Record the Rust build, each tray version, and pass or fail for each step.
The HTTP fixture covers both platforms, HMAC probes, persistence across restart,
and independent revocation using disposable keys.

```sh
bun tests/spec/m6-tray-parity.ts /absolute/path/to/aac-http
```

The supervised steps verify the native UI and credential storage on each OS.
