using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls.Primitives;

namespace CCSBar;

/// <summary>
/// Device pairing, the sign-in screen's flows and the round-2 bindings (CONTRACT-auth-devices sections 5 to 10, the
/// trays concept's sign-in states, F4, F6 and the tray visibility). Every case drives the tray's own screen (its fields
/// and its primary button) or its own flows against <see cref="PairingFixture"/>, with the connection in an isolated
/// temporary folder that is asserted to be outside the tray's real state folder.
/// </summary>
public static partial class Checks
{
    private static void Press(ButtonBase button) =>
        typeof(ButtonBase).GetMethod("OnClick", System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance)!.Invoke(button, null);

    /// <summary>One click of the sign-in screen's primary, then the flow it started, to the end.</summary>
    private static async Task Submit(MainWindow window)
    {
        Press(window.SignIn.PrimaryButton);
        await window.SignInFlowTask;
        await window.Dispatcher.InvokeAsync(() => { }, System.Windows.Threading.DispatcherPriority.Background);
    }

    /// <summary>The E2E driver's click: the same as the checks'.</summary>
    internal static Task SubmitForE2E(MainWindow window) => Submit(window);
    /// <summary>The E2E driver's plain click, without waiting for the flow it starts.</summary>
    internal static void PressForE2E(ButtonBase button) => Press(button);

    private static string[] Keys(string path) => File.Exists(path) ? SecureStore.StoredKeysForCheck(path).OrderBy(key => key, StringComparer.Ordinal).ToArray() : Array.Empty<string>();

    private static async Task PairingChecks(CheckReport report)
    {
        var folder = Path.Combine(Path.GetTempPath(), "aac-pairing-check-" + Guid.NewGuid().ToString("N"));
        var real = Path.GetFullPath(SecureStore.StateDirectory).TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar;
        var isolated = !Path.GetFullPath(folder).StartsWith(real, StringComparison.OrdinalIgnoreCase);
        report.Checks["pairing_checks_use_an_isolated_store"] = isolated;
        if (!isolated) return;
        var motion = Motion.Enabled;
        Motion.Enabled = false;
        var windows = new List<MainWindow>();
        var launched = new List<string>();
        var realLaunch = MainWindow.Launch;
        MainWindow.Launch = info => { lock (launched) launched.Add(info.FileName); return null; };
        using var fixture = new PairingFixture();
        int store = 0;
        string Store() => Path.Combine(folder, "case-" + (++store), "connection.dpapi");
        // The windows' preferences live in the isolated folder too: a migration writes its attempt time there, never in
        // the tray's real preferences.json, whether or not AAC_TRAY_STATE_DIR is set (review B5N finding 5).
        var preferencesFile = Path.Combine(folder, "preferences.json");
        var realPreferences = Preferences.DefaultPath;
        DateTime? Stamp(string file) => File.Exists(file) ? File.GetLastWriteTimeUtc(file) : null;
        var realPreferencesBefore = Stamp(realPreferences);
        MainWindow Panel(string path)
        {
            var window = new MainWindow(new Preferences { Theme = "light", Hotkey = false, StorePath = preferencesFile }, loadConnection: false)
            { StepPace = TimeSpan.FromMilliseconds(1), SuccessPause = TimeSpan.FromMilliseconds(1), AddressTimeout = TimeSpan.FromSeconds(5), KeyProofBackoff = new[] { TimeSpan.FromMilliseconds(20) } };
            window.UseConnectionStoreForCheck(path);
            windows.Add(window);
            return window;
        }
        var expectedV2 = new[] { "baseURL", "deviceId", "deviceToken", "installId", "pairedAt", "rotateAfter", "username", "version" };
        try
        {
            Directory.CreateDirectory(folder);

            // ---- first run: the address
            report.Checks["new_tray_has_no_default_dashboard_address"] = new ConnectionSettings().BaseURL.Length == 0;
            var first = Panel(Store());
            first.ShowSignIn(SignInState.FirstRun);
            var view = first.SignIn;
            report.Checks["signin_first_run_starts_empty"] = view.Model.State == SignInState.FirstRun && view.Addr.Value.Length == 0 && view.TitleText == "Connect this PC" && first.SignInStatus == "Not paired";
            fixture.ClearLog();
            await Submit(first);
            report.Checks["signin_empty_address_refused_locally"] = view.Model.State == SignInState.FirstRun && view.MessageText == "Enter the dashboard address." && view.Addr.Bad && fixture.Requests.Count == 0;
            foreach (var address in new[] { "203.0.113.5:3000", "http://[2001:db8::1]:3000", "100.64.1.2:3000", "169.254.3.4:3000" })
            {
                view.Addr.Value = address;
                await Submit(first);
            }
            report.Checks["signin_public_address_refused_before_anything_is_sent"] = view.Model.State == SignInState.NotLocal && view.TitleText == "This address isn't on your local network"
                && view.LedeText.Contains("is a public address", StringComparison.Ordinal) && view.RegionOpen("guide") && view.Addr.Bad && fixture.Requests.Count == 0;
            report.Checks["local_network_ranges_match_the_dashboard"] = new[] { "10.6.0.9", "192.168.50.20", "172.16.0.1", "172.31.255.255", "127.0.0.1", "fd00::5", "::ffff:192.168.1.4", "localhost", "ubuntu-vm", "dash.local", "dash.lan", "dash.home.arpa" }.All(LocalNetwork.IsLocalHostName)
                && !new[] { "203.0.113.5", "100.64.1.2", "169.254.3.4", "172.32.0.1", "2001:db8::1", "fe80::1", "home.example.net", "0.0.0.0" }.Any(LocalNetwork.IsLocalHostName);
            view.Addr.Value = ClosedLoopbackOrigin();
            await Submit(first);
            report.Checks["signin_unreachable_address"] = view.Model.State == SignInState.Unreachable && view.TitleText == "Can't reach that address" && view.MessageText.StartsWith("Nothing answered", StringComparison.Ordinal) && !File.Exists(first.ConnectionStorePath);
            using (var elsewhere = new NotDashboardFixture())
            {
                view.Addr.Value = elsewhere.Origin;
                await Submit(first);
                report.Checks["signin_wrong_address"] = view.Model.State == SignInState.WrongAddress && view.TitleText == "That isn't a dashboard address" && view.Addr.Bad;
            }

            // ---- the dashboard's view of this computer: switch off, not a local peer, then the password
            fixture.TrustSwitch = false;
            view.Addr.Value = fixture.Origin.Replace("http://", "", StringComparison.Ordinal);
            await Submit(first);
            report.Checks["signin_pairing_off_when_trust_is_off"] = view.Model.State == SignInState.PairingOff && view.TitleText == "Pairing is turned off for remote computers"
                && view.LedeText.Contains("Trust this local network", StringComparison.Ordinal) && view.Model.Verified == fixture.Origin && view.FootText.StartsWith("Only a browser on the dashboard machine", StringComparison.Ordinal);
            fixture.TrustSwitch = true; fixture.ConnectionTrusted = false; fixture.Peer = "100.70.1.4";
            await Submit(first);
            report.Checks["signin_not_local_by_the_dashboards_view"] = view.Model.State == SignInState.NotLocal && view.LedeText.Contains("The dashboard sees this PC at 100.70.1.4", StringComparison.Ordinal);
            fixture.ConnectionTrusted = true; fixture.Peer = "192.168.1.31";
            view.Addr.Value = fixture.Origin;
            await Submit(first);
            report.Checks["signin_trusted_address_asks_for_the_password"] = view.Model.State == SignInState.Password && view.TitleText == "Sign in to pair" && view.RegionOpen("creds") && view.PrimaryLabel == "Pair this PC";

            // ---- wrong password, tries left, then the rate limit
            view.User.Value = "fixture"; view.Pass.Value = "wrong-password";
            await Submit(first);
            report.Checks["signin_wrong_password_shows_tries_left"] = view.Model.State == SignInState.WrongPassword && view.MessageText.Contains("4 tries left", StringComparison.Ordinal) && view.Pass.Bad && !File.Exists(first.ConnectionStorePath);
            fixture.RateLimitSeconds = 120;
            await Submit(first);
            report.Notes["signin_rate_limited_countdown"] = view.CountdownText;
            report.Checks["signin_rate_limited_counts_down_and_rests_the_button"] = view.Model.State == SignInState.RateLimited && !view.PrimaryEnabled && view.CountdownText is "2:00" or "1:59"
                && view.BannerText.StartsWith("Try again in 2 minutes", StringComparison.Ordinal) && view.Pass.Value.Length == 0;
            fixture.RateLimitSeconds = null; fixture.TriesLeft = 5;
            view.Model.LimitUntil = DateTimeOffset.UtcNow.AddSeconds(-1);
            await Task.Delay(1200);
            report.Checks["signin_rate_limit_ends_back_on_the_password"] = view.Model.State == SignInState.Password && view.PrimaryEnabled;

            // ---- pairing: the key must work once before anything is saved
            fixture.DeviceMeMode = "401:invalid_token";
            view.User.Value = "fixture"; view.Pass.Value = "fixture-new";
            await Submit(first);
            report.Checks["signin_new_key_must_work_before_it_is_saved"] = view.Model.State == SignInState.Password && view.MessageText.StartsWith("The new device key did not work.", StringComparison.Ordinal)
                && !File.Exists(first.ConnectionStorePath) && first.ClientForCheck is null;
            fixture.DeviceMeMode = "200";
            fixture.ClearLog();
            view.User.Value = "fixture"; view.Pass.Value = "fixture-new";
            await Submit(first);
            var log = fixture.Requests;
            report.Notes["signin_pair_requests"] = string.Join(" | ", log);
            report.Checks["signin_pairs_and_hands_off_to_the_list"] = !first.SignInVisible && first.ClientForCheck is { Paired: true } && first.Dashboard is { Accounts.Count: 1 } && first.StatusFlashForCheck == "Paired · signed in with a device key";
            report.Checks["signin_saves_version_2_with_the_key_and_no_password"] = Keys(first.ConnectionStorePath).SequenceEqual(expectedV2) && SecureStore.Load(first.ConnectionStorePath) is { Version: 2, IsPaired: true, HasPassword: false, Username: "fixture" };
            report.Checks["signin_order_pair_then_key_then_bearer_reads"] = log.Count >= 3 && log[0] == "POST /api/auth/devices/pair 201" && log[1] == "GET /api/auth/devices/me 200 bearer"
                && log.Skip(2).All(line => line.EndsWith(" bearer", StringComparison.Ordinal)) && !log.Any(line => line.Contains("/api/auth/login", StringComparison.Ordinal) || line.Contains("cookie", StringComparison.Ordinal));
            report.Checks["signin_sends_this_pcs_name_and_an_install_id"] = fixture.LastDeviceName == Environment.MachineName && Guid.TryParse(fixture.LastInstallId, out _)
                && SecureStore.Load(first.ConnectionStorePath)?.InstallId == fixture.LastInstallId;
            report.Checks["signin_password_fields_forgotten"] = view.Pass.Value.Length == 0 && view.Confirm.Value.Length == 0;

            // ---- bearer reads, Claude Open, and never a cookie sign-in
            fixture.ClearLog();
            var paired = first.ClientForCheck!;
            await paired.Dashboard(false);
            await paired.ClaudeDesktopProfiles();
            report.Checks["bearer_on_every_tray_route_and_no_cookie"] = fixture.Requests.SequenceEqual(new[] { "GET /api/accounts/dashboard 200 bearer", "GET /api/claude/desktop-profiles 200 bearer" });

            // ---- rotation (section 7): the new key is written before its first use; the old one then stops working
            var before = SecureStore.Load(first.ConnectionStorePath)!.DeviceToken!;
            fixture.ClearLog();
            await first.MaintainDeviceKey(force: true);
            var rotated = SecureStore.Load(first.ConnectionStorePath)!;
            await first.ClientForCheck!.Dashboard(false);
            report.Checks["rotation_saves_the_new_key_before_using_it"] = rotated.DeviceToken != before && DeviceTokenFormat.IsToken(rotated.DeviceToken) && rotated.RotateAfter == "2026-12-01T00:00:00.000Z"
                && fixture.LastBearer == rotated.DeviceToken && !fixture.Accepts(before) && Keys(first.ConnectionStorePath).SequenceEqual(expectedV2);
            fixture.TrustSwitch = false;
            var kept = SecureStore.Load(first.ConnectionStorePath)!.DeviceToken;
            await first.MaintainDeviceKey(force: true);
            report.Checks["rotation_deferred_off_the_trusted_network_keeps_the_key"] = SecureStore.Load(first.ConnectionStorePath)!.DeviceToken == kept && first.ClientForCheck is { Paired: true };
            fixture.TrustSwitch = true;

            // ---- a password change in the dashboard does nothing to a paired tray
            fixture.Password = "fixture-changed";
            await first.Refresh(true);
            report.Checks["password_change_keeps_a_paired_tray_signed_in"] = !first.SignInVisible && first.ClientForCheck is { Paired: true } && first.Dashboard is not null && !first.IsStale;

            // ---- Settings › Connection while paired
            first.OpenSettings();
            var who = first.SettingsElement("settings-connection-who") as System.Windows.Controls.TextBlock;
            var via = first.SettingsElement("settings-this-connection") as System.Windows.Controls.TextBlock;
            for (int i = 0; i < 50 && via?.Text == "This connection: checking"; i++) await Task.Delay(50);
            report.Notes["settings_connection_via"] = via?.Text ?? "";
            var whoText = who is null ? "" : new System.Windows.Documents.TextRange(who.ContentStart, who.ContentEnd).Text;
            report.Notes["settings_connection_who"] = whoText;
            report.Checks["settings_connection_paired_lines"] = whoText.StartsWith("Paired as Windows tray, last synced", StringComparison.Ordinal) && via?.Text == "This connection: 192.168.1.31, trusted local network"
                && first.SettingsElement("settings-repair") is not null && first.SettingsElement("settings-disconnect") is not null;
            report.Checks["this_connection_line_forms"] = MainWindow.ConnectionLine(new AuthCheck { Connection = new AuthConnection { Peer = "10.6.0.3", Trusted = true } }) == "This connection: 10.6.0.3, trusted local network"
                && MainWindow.ConnectionLine(new AuthCheck { Connection = new AuthConnection { Peer = "192.168.50.31", Trusted = false } }) == "This connection: 192.168.50.31, not trusted"
                && MainWindow.ConnectionLine(new AuthCheck { SecureTransport = true, Connection = new AuthConnection { Peer = "127.0.0.1" } }) == "This connection: this computer"
                && MainWindow.ConnectionLine(new AuthCheck()) == "This connection: not reported by this dashboard";
            first.CloseSettings(animate: false);

            // ---- Re-pair: Cancel keeps the current key; success replaces it with the same installId
            var currentClient = first.ClientForCheck;
            var currentBytes = File.ReadAllBytes(first.ConnectionStorePath);
            first.ShowSignIn(SignInState.Password, repair: true);
            report.Checks["repair_shows_its_banner_and_cancel"] = first.SignInVisible && view.TitleText == "Sign in to re-pair" && view.BannerText.StartsWith("Re-pairing replaces this tray's device key", StringComparison.Ordinal) && view.AltVisible
                && first.SignInStatus == "Re-pairing · the current key still works";
            Press(view.AltButton);
            report.Checks["repair_cancel_keeps_the_current_key"] = !first.SignInVisible && ReferenceEquals(first.ClientForCheck, currentClient) && File.ReadAllBytes(first.ConnectionStorePath).AsSpan().SequenceEqual(currentBytes);
            first.ShowSignIn(SignInState.Password, repair: true);
            Press(view.PrimaryButton); await first.SignInFlowTask; // the username is filled in; the password is not
            var emptyRefused = view.MessageText == "Enter your dashboard username and password.";
            view.Addr.Value = ClosedLoopbackOrigin();
            view.Model.Verified = view.Addr.Value;
            view.Apply(SignInState.FirstRun);
            await Submit(first);
            report.Checks["repair_unreachable_keeps_the_connection"] = emptyRefused && view.Model.State == SignInState.Unreachable && view.BannerText.StartsWith("Your current connection is unchanged", StringComparison.Ordinal)
                && view.AltVisible && ReferenceEquals(first.ClientForCheck, currentClient) && File.ReadAllBytes(first.ConnectionStorePath).AsSpan().SequenceEqual(currentBytes);
            Press(view.AltButton);
            var installBefore = SecureStore.Load(first.ConnectionStorePath)!.InstallId;
            var oldKey = SecureStore.Load(first.ConnectionStorePath)!.DeviceToken!;
            first.ShowSignIn(SignInState.Password, repair: true);
            view.Pass.Value = "fixture-changed";
            await Submit(first);
            var repaired = SecureStore.Load(first.ConnectionStorePath)!;
            report.Checks["repair_replaces_the_key_with_the_same_install"] = !first.SignInVisible && repaired.DeviceToken != oldKey && repaired.InstallId == installBefore && fixture.LastInstallId == installBefore && !fixture.Accepts(oldKey);

            // ---- Re-pair once Pair is pressed (review B5N finding 1): the dashboard replaces the current key as soon as it
            // answers, so Cancel, Escape and Disconnect rest until the new key is proved, saved and adopted.
            var heldKey = SecureStore.Load(first.ConnectionStorePath)!.DeviceToken!;
            first.ShowSignIn(SignInState.Password, repair: true);
            view.Pass.Value = "fixture-changed";
            fixture.PairDelayMs = 400;
            Press(view.PrimaryButton);
            var heldWhileOut = first.PairHeld && view.AltHeld && first.SignInStatus == "Re-pairing";
            Press(view.AltButton);
            first.EscapeForCheck();
            var disconnectWaits = await first.DisconnectTray();
            var stillPairing = first.SignInVisible;
            await first.SignInFlowTask;
            await first.Dispatcher.InvokeAsync(() => { }, System.Windows.Threading.DispatcherPriority.Background);
            fixture.PairDelayMs = 0;
            var heldResult = SecureStore.Load(first.ConnectionStorePath)!;
            report.Checks["repair_cancel_and_escape_rest_once_pair_is_sent"] = heldWhileOut && stillPairing && disconnectWaits == "Pairing is finishing. Try again in a moment."
                && !first.SignInVisible && first.ClientForCheck is { Paired: true } && heldResult.DeviceToken != heldKey && fixture.Accepts(heldResult.DeviceToken!) && !fixture.Accepts(heldKey)
                && !first.PairHeld && !view.AltHeld && first.StatusFlashForCheck == "Paired · signed in with a device key";

            // A new key whose first check is cut off once is asked again, not given up.
            var beforeRetry = heldResult.DeviceToken!;
            first.ShowSignIn(SignInState.Password, repair: true);
            view.Pass.Value = "fixture-changed";
            fixture.DeviceMeDropNext = 1;
            fixture.ClearLog();
            await Submit(first);
            var retried = SecureStore.Load(first.ConnectionStorePath)!;
            report.Checks["repair_new_key_check_is_asked_again_after_a_network_error"] = !first.SignInVisible && first.ClientForCheck is { Paired: true } && retried.DeviceToken != beforeRetry
                && fixture.Accepts(retried.DeviceToken!) && fixture.Count("GET /api/auth/devices/me") >= 2 && fixture.DeviceMeDropNext == 0;

            // A new key that never answers: the current key is already gone, so the new one is kept and used, and the screen
            // never claims the old connection still works.
            var deadKey = retried.DeviceToken!;
            var seen = new List<SignInState>();
            void Seen(SignInState state) => seen.Add(state);
            first.ShowSignIn(SignInState.Password, repair: true);
            view.Pass.Value = "fixture-changed";
            view.StateApplied += Seen;
            fixture.DeviceMeMode = "abort";
            await Submit(first);
            fixture.DeviceMeMode = "200";
            view.StateApplied -= Seen;
            var unproved = SecureStore.Load(first.ConnectionStorePath)!;
            report.Notes["repair_without_an_answer_states"] = string.Join(" > ", seen);
            report.Checks["repair_without_an_answer_keeps_the_new_key_not_the_dead_one"] = !first.SignInVisible && first.ClientForCheck is { Paired: true } && unproved.DeviceToken != deadKey
                && fixture.Accepts(unproved.DeviceToken!) && !fixture.Accepts(deadKey) && !seen.Contains(SignInState.Unreachable) && first.Dashboard is not null && !first.IsStale;

            // A pair request that never reached the dashboard (nothing listening) leaves the current key, and says so.
            var keptClient = first.ClientForCheck;
            var keptKey = unproved.DeviceToken!;
            first.ShowSignIn(SignInState.Password, repair: true);
            view.Model.Verified = ClosedLoopbackOrigin();
            view.Pass.Value = "fixture-changed";
            await Submit(first);
            report.Checks["repair_pair_never_sent_keeps_the_connection"] = view.Model.State == SignInState.Unreachable && view.BannerText.StartsWith("Your current connection is unchanged", StringComparison.Ordinal)
                && view.AltVisible && ReferenceEquals(first.ClientForCheck, keptClient) && SecureStore.Load(first.ConnectionStorePath)!.DeviceToken == keptKey && fixture.Accepts(keptKey);
            Press(view.AltButton);

            // A pair answer lost after the dashboard replaced the key: the tray is signed out, and the screen says why.
            first.ShowSignIn(SignInState.Password, repair: true);
            view.Pass.Value = "fixture-changed";
            fixture.PairDropsAnswer = true;
            await Submit(first);
            fixture.PairDropsAnswer = false;
            report.Notes["repair_answer_lost_message"] = view.MessageText;
            report.Checks["repair_answer_lost_after_the_key_was_replaced_signs_out_and_says_why"] = first.SignInVisible && view.Model.State == SignInState.SignedOut
                && view.MessageText.StartsWith("Pairing reached the dashboard, but its answer was lost.", StringComparison.Ordinal) && first.ClientForCheck is null && SecureStore.Load(first.ConnectionStorePath) is { IsSignedOut: true };
            view.Pass.Value = "fixture-changed";
            await Submit(first);

            // Pairing turned off never offers a paired tray the password sign-in (it would trade a key for a password).
            fixture.TrustSwitch = false;
            first.ShowSignIn(SignInState.Password, repair: true);
            view.Pass.Value = "fixture-changed";
            await Submit(first);
            report.Checks["pairing_off_never_offers_a_paired_tray_the_password"] = view.Model.State == SignInState.PairingOff && !view.Model.PasswordFallback
                && FixtureRender.FindUid(view, "si-use-password") is null && first.ClientForCheck is { Paired: true };
            Press(view.AltButton);
            fixture.TrustSwitch = true;

            // ---- a remote sign-out: revoked, revoke-all with who, expired; 503 is not a sign-out
            fixture.DashboardMode = "503:auth_store_unavailable";
            await first.Refresh(true);
            report.Checks["auth_store_unavailable_is_not_a_sign_out"] = !first.SignInVisible && first.ClientForCheck is { Paired: true } && SecureStore.Load(first.ConnectionStorePath)!.IsPaired;
            fixture.DashboardMode = "200";
            await first.Refresh(true);
            fixture.RevokeAll();
            await first.Refresh(true);
            var stub = Keys(first.ConnectionStorePath);
            report.Checks["revoked_shows_the_signed_out_screen"] = first.SignInVisible && view.Model.State == SignInState.SignedOut && view.TitleText == "This tray was signed out"
                && view.LedeText.StartsWith("Revoked from the dashboard", StringComparison.Ordinal) && view.FootText.EndsWith("The tray won't retry on its own.", StringComparison.Ordinal) && first.SignInStatus == "Signed out"
                && view.User.Value == "fixture" && view.PrimaryLabel == "Pair again";
            report.Checks["revoked_deletes_the_key_and_keeps_address_and_username"] = stub.SequenceEqual(new[] { "baseURL", "installId", "signedOutAt", "signedOutReason", "username", "version" })
                && SecureStore.Load(first.ConnectionStorePath) is { IsSignedOut: true, SignedOutReason: "device_revoked" } && first.ClientForCheck is null;
            fixture.ClearLog();
            await first.Refresh(true);
            report.Checks["signed_out_never_retries"] = fixture.Requests.Count == 0;
            report.Checks["tray_tooltip_says_signed_out"] = Formatting.TrayTooltip(null, configured: false, signInState: first.SignInStatus) == "AI Account Center · Signed out"
                && Formatting.TrayTooltip(null, configured: false) == "AI Account Center · Not paired";
            // A restart reopens on the same screen.
            var restarted = Panel(first.ConnectionStorePath);
            restarted.ShowSignIn(SignInState.SignedOut);
            report.Checks["signed_out_file_is_read_back_after_a_restart"] = restarted.ClientForCheck is null && restarted.StoredConnectionForCheck is { IsSignedOut: true } && restarted.SignIn.User.Value == "fixture";
            view.Pass.Value = "fixture-changed";
            await Submit(first);
            report.Checks["pair_again_after_sign_out"] = !first.SignInVisible && first.ClientForCheck is { Paired: true } && Keys(first.ConnectionStorePath).SequenceEqual(expectedV2);
            fixture.RevokedReason = "revoke-all"; fixture.RevokedBy = "admin";
            fixture.RevokeAll();
            await first.Refresh(true);
            report.Checks["revoke_all_with_who_when_the_dashboard_says_so"] = view.Model.State == SignInState.SignedOutAll && view.LedeText.StartsWith("admin chose Sign out all devices in the dashboard", StringComparison.Ordinal);
            fixture.RevokedReason = null; fixture.RevokedBy = null;
            view.Pass.Value = "fixture-changed";
            await Submit(first);
            fixture.RevokeAll("expired");
            await first.Refresh(true);
            report.Checks["expired_says_not_used_for_90_days"] = view.Model.State == SignInState.Expired && view.LedeText.StartsWith("Not used for 90 days", StringComparison.Ordinal)
                && SecureStore.Load(first.ConnectionStorePath)?.SignedOutReason == "device_expired";

            // ---- Claude Open with a revoked key ends on the signed-out screen, and starts no URI
            view.Pass.Value = "fixture-changed";
            await Submit(first);
            fixture.RevokeAll();
            await first.OpenClaudeForCheck(new DashboardAccount { Id = "claude:fixture-a", Provider = "claude", Capabilities = new AccountCapabilities { ClaudeProfileId = "fixture-a", ClaudePlatforms = new() { "windows" } } }, "windows");
            report.Checks["claude_open_with_a_revoked_key_signs_out"] = first.SignInVisible && view.Model.State == SignInState.SignedOut && launched.Count == 0;

            // ---- Disconnect: DELETE /api/auth/devices/me, then first run with the address filled in
            view.Pass.Value = "fixture-changed";
            await Submit(first);
            fixture.ClearLog();
            var gone = await first.DisconnectTray();
            report.Checks["disconnect_revokes_and_forgets_the_key"] = gone is null && fixture.Requests.Contains("DELETE /api/auth/devices/me 204 bearer") && fixture.ActiveKeys == 0
                && SecureStore.Load(first.ConnectionStorePath) is { IsSignedOut: true, SignedOutReason: "disconnected" } && first.ClientForCheck is null;
            report.Checks["disconnect_lands_on_first_run_with_the_address"] = first.SignInVisible && view.Model.State == SignInState.FirstRun && view.BannerText.StartsWith("Disconnected at", StringComparison.Ordinal) && view.Addr.Value == fixture.Origin;
            view.Addr.Value = fixture.Origin;
            await Submit(first);
            view.Pass.Value = "fixture-changed";
            await Submit(first);
            fixture.DisconnectMode = "abort";
            var refused = await first.DisconnectTray();
            report.Checks["disconnect_unreachable_keeps_the_pairing"] = refused?.StartsWith("Can't reach the dashboard", StringComparison.Ordinal) == true && first.ClientForCheck is { Paired: true } && SecureStore.Load(first.ConnectionStorePath)!.IsPaired;
            fixture.DisconnectMode = "204";

            // ---- a rotation still out when Disconnect lands never writes the old device's key back (review B5N finding 4)
            fixture.RotateDelayMs = 400;
            fixture.ClearLog();
            var maintaining = first.MaintainDeviceKey(force: true);
            for (int i = 0; i < 100 && fixture.Count("POST /api/auth/devices/me/rotate") == 0; i++) await Task.Delay(20);
            var raceGone = await first.DisconnectTray();
            await maintaining;
            fixture.RotateDelayMs = 0;
            report.Checks["rotation_in_flight_never_overwrites_a_disconnect"] = raceGone is null && fixture.Count("POST /api/auth/devices/me/rotate") == 1
                && SecureStore.Load(first.ConnectionStorePath) is { IsSignedOut: true, SignedOutReason: "disconnected" } && first.ClientForCheck is null && first.SignInVisible;
            fixture.Password = "fixture-new";

            // ---- first-run setup with the code
            fixture.AccessMode = "setup"; fixture.TriesLeft = 5;
            var setup = Panel(Store());
            setup.ShowSignIn(SignInState.FirstRun);
            var sv = setup.SignIn;
            sv.Addr.Value = fixture.Origin;
            await Submit(setup);
            var setupShown = sv.Model.State == SignInState.SetupCode && sv.TitleText == "Set up sign-in" && sv.RegionOpen("setup");
            sv.User.Value = "fixture-admin"; sv.Pass.Value = "summit-ledger-4242"; sv.Confirm.Value = "summit-ledger-424"; sv.Code.Value = "ABCD-2345";
            await Submit(setup);
            var mismatch = sv.MessageText == "The confirmation doesn't match the password." && sv.Confirm.Bad;
            sv.Confirm.Value = "summit-ledger-4242"; sv.Code.Value = "WXYZ-9999";
            await Submit(setup);
            var wrongCode = sv.MessageText.StartsWith("That setup code isn't right.", StringComparison.Ordinal) && sv.Code.Bad;
            sv.Code.Value = "abcd-2345";
            await Submit(setup);
            report.Checks["signin_setup_code_creates_the_sign_in_and_pairs"] = setupShown && mismatch && wrongCode && !setup.SignInVisible && setup.ClientForCheck is { Paired: true }
                && Keys(setup.ConnectionStorePath).SequenceEqual(expectedV2) && fixture.Username == "fixture-admin";
            report.Checks["setup_validation_and_strength"] = MainWindow.SetupProblem("1bad", "summit-ledger-4242", "summit-ledger-4242", "ABCD-2345")?.Field == "user"
                && MainWindow.SetupProblem("fixture", "short", "short", "ABCD-2345")?.Field == "pass" && MainWindow.SetupProblem("fixture", "summit-ledger-4242", "summit-ledger-4242", "")?.Field == "code"
                && SignInView.Strength("summit-ledger-42!X").Word == "Strong" && SignInView.Strength("abc").Word == "Too short" && SignInView.Strength("password123").Word == "Weak";
            fixture.Username = "fixture"; fixture.Password = "fixture-new"; fixture.AccessMode = "login";

            // ---- a dashboard without pairing: the old password sign-in, verified before it is saved
            fixture.TrustSwitch = null; fixture.PairRouteExists = false;
            var legacy = Panel(Store());
            legacy.ShowSignIn(SignInState.FirstRun);
            legacy.SignIn.Addr.Value = fixture.Origin;
            await Submit(legacy);
            legacy.SignIn.User.Value = "fixture"; legacy.SignIn.Pass.Value = "fixture-new";
            await Submit(legacy);
            report.Checks["older_dashboard_keeps_the_password_sign_in"] = !legacy.SignInVisible && legacy.ClientForCheck is { Paired: false } && Keys(legacy.ConnectionStorePath).SequenceEqual(new[] { "baseURL", "password", "username" });
            fixture.TrustSwitch = true; fixture.PairRouteExists = true;


            // ---- F4: a blank password is never sent to a new address
            var otherDashboard = ClosedLoopbackOrigin();
            fixture.ClearLog();
            var blank = await legacy.SubmitConnection(otherDashboard, "fixture", "");
            var sameBlank = await legacy.SubmitConnection(fixture.Origin, "fixture", "");
            report.Checks["blank_password_reused_only_for_the_same_dashboard"] = blank?.StartsWith("Enter the password for this dashboard.", StringComparison.Ordinal) == true && sameBlank is null && fixture.Count("POST /api/auth/login") == 1;

            // ---- a version 1 tray pairing from Settings speaks of its saved password, not a device key (review B5N finding 7)
            var lv = legacy.SignIn;
            var legacyClient = legacy.ClientForCheck;
            legacy.ShowSignIn(SignInState.Password, repair: true);
            var upgradeWords = lv.Model.Upgrade && !lv.Model.Repair && lv.TitleText == "Sign in to pair this tray" && lv.BannerText.StartsWith("Pairing replaces the saved password", StringComparison.Ordinal)
                && legacy.SignInStatus == "Pairing · the saved password still works" && lv.AltVisible;
            Press(lv.AltButton);
            report.Checks["version_1_pair_from_settings_speaks_of_the_saved_password"] = upgradeWords && !legacy.SignInVisible && ReferenceEquals(legacy.ClientForCheck, legacyClient);

            // ---- pairing turned off, on a tray without a key: the dashboard password changed, and the tray can still take
            // the new one, verified before it is saved (review B5N finding 3)
            fixture.TrustSwitch = false; fixture.Password = "fixture-rotated";
            legacy.ShowSignIn(SignInState.Password, repair: true);
            lv.Pass.Value = "fixture-rotated";
            await Submit(legacy);
            var offeredPassword = lv.Model.State == SignInState.PairingOff && lv.Model.PasswordFallback && lv.LedeText.Contains("Or use your password for now", StringComparison.Ordinal);
            if (FixtureRender.FindUid(lv, "si-use-password") is System.Windows.Controls.Button usePassword) Press(usePassword);
            var passwordForm = lv.Model.State == SignInState.Password && lv.Model.Legacy && lv.TitleText == "Sign in with your password" && lv.PrimaryLabel == "Sign in" && !lv.RegionOpen("device");
            lv.Pass.Value = "fixture-rotated";
            fixture.ClearLog();
            await Submit(legacy);
            report.Notes["pairing_off_password_flash"] = legacy.StatusFlashForCheck ?? "";
            report.Checks["pairing_off_offers_the_password_to_a_tray_without_a_key"] = offeredPassword && passwordForm && !legacy.SignInVisible && legacy.ClientForCheck is { Paired: false } && legacy.Dashboard is not null
                && Keys(legacy.ConnectionStorePath).SequenceEqual(new[] { "baseURL", "password", "username" }) && fixture.Count("POST /api/auth/login") == 1 && fixture.Count("POST /api/auth/devices/pair") == 0
                && legacy.StatusFlashForCheck == "Signed in with your password. Turn on Trust this local network to pair this tray.";
            fixture.TrustSwitch = true; fixture.Password = "fixture-new";

            // ---- migration from a version 1 password (section 8): Securing, then the key; the rollback is deleted
            var v1 = Store();
            SecureStore.Save(new ConnectionSettings { BaseURL = fixture.Origin, Username = "fixture", Password = "fixture-new" }, v1);
            var migrating = Panel(v1);
            fixture.ClearLog();
            await migrating.MigrateIfDue(force: true);
            report.Checks["migration_trades_the_password_for_a_key"] = migrating.ClientForCheck is { Paired: true } && Keys(v1).SequenceEqual(expectedV2) && SecureStore.RollbackAge(v1) is null
                && SecureStore.Load(v1) is { HasPassword: false } && fixture.Requests.Take(3).SequenceEqual(new[] { "GET /api/auth/check 200", "POST /api/auth/devices/pair 201", "GET /api/auth/devices/me 200 bearer" })
                && !migrating.SignInVisible && migrating.StatusFlashForCheck == "Secured · this tray now signs in with a device key";
            var v1Restore = Store();
            SecureStore.Save(new ConnectionSettings { BaseURL = fixture.Origin, Username = "fixture", Password = "fixture-new" }, v1Restore);
            var v1Bytes = File.ReadAllBytes(v1Restore);
            fixture.DeviceMeMode = "401:invalid_token";
            var restoring = Panel(v1Restore);
            await restoring.MigrateIfDue(force: true);
            report.Checks["migration_restores_version_1_when_the_key_is_refused"] = File.ReadAllBytes(v1Restore).AsSpan().SequenceEqual(v1Bytes) && SecureStore.RollbackAge(v1Restore) is null
                && restoring.ClientForCheck is { Paired: false } && !restoring.SignInVisible;
            var v1Offline = Store();
            SecureStore.Save(new ConnectionSettings { BaseURL = fixture.Origin, Username = "fixture", Password = "fixture-new" }, v1Offline);
            fixture.DeviceMeMode = "abort";
            var offline = Panel(v1Offline);
            await offline.MigrateIfDue(force: true);
            var bothKept = SecureStore.Load(v1Offline) is { IsPaired: true, HasPassword: false } && SecureStore.LoadRollback(v1Offline) is { HasPassword: true } && offline.ClientForCheck is { Paired: true };
            fixture.DeviceMeMode = "200";
            await offline.Refresh(true);
            report.Checks["migration_network_error_keeps_both_then_settles"] = bothKept && SecureStore.RollbackAge(v1Offline) is null && SecureStore.Load(v1Offline) is { IsPaired: true };
            // A version 1 file written 40 days ago: the rollback's 24 hours still count from the migration, so a 401 on the
            // new key's first use brings version 1 back (review B5N finding 2).
            var v1Aged = Store();
            SecureStore.Save(new ConnectionSettings { BaseURL = fixture.Origin, Username = "fixture", Password = "fixture-new" }, v1Aged);
            var v1AgedBytes = File.ReadAllBytes(v1Aged);
            File.SetLastWriteTimeUtc(v1Aged, DateTime.UtcNow.AddDays(-40));
            fixture.DeviceMeMode = "abort";
            var aged = Panel(v1Aged);
            await aged.MigrateIfDue(force: true);
            fixture.DeviceMeMode = "200";
            var agedRollback = SecureStore.RollbackAge(v1Aged);
            report.Notes["migration_rollback_age_seconds"] = agedRollback?.TotalSeconds.ToString("0", System.Globalization.CultureInfo.InvariantCulture) ?? "none";
            fixture.RevokeAll();
            await aged.Refresh(true);
            report.Checks["migration_rollback_counts_from_the_migration_not_the_file"] = agedRollback is { } young && young < TimeSpan.FromMinutes(10) && young > TimeSpan.FromMinutes(-10);
            report.Checks["migration_restores_an_old_version_1_file_on_a_401"] = File.ReadAllBytes(v1Aged).AsSpan().SequenceEqual(v1AgedBytes) && SecureStore.RollbackAge(v1Aged) is null
                && aged.ClientForCheck is { Paired: false } && !aged.SignInVisible && aged.StatusFlashForCheck == "This tray went back to its saved password sign-in.";
            fixture.TrustSwitch = false;
            var v1Off = Store();
            SecureStore.Save(new ConnectionSettings { BaseURL = fixture.Origin, Username = "fixture", Password = "fixture-new" }, v1Off);
            var v1OffBytes = File.ReadAllBytes(v1Off);
            var notYet = Panel(v1Off);
            await notYet.MigrateIfDue(force: true);
            report.Checks["migration_waits_while_the_dashboard_cannot_pair"] = File.ReadAllBytes(v1Off).AsSpan().SequenceEqual(v1OffBytes) && SecureStore.RollbackAge(v1Off) is null && notYet.ClientForCheck is { Paired: false } && !notYet.SignInVisible;
            fixture.TrustSwitch = true;

            // ---- --configure-stdin pairs at once, or stores version 1 when pairing is not possible
            var configured = Store();
            var pairedNow = await Pairing.ConfigureAsync(new ConnectionSettings { BaseURL = fixture.Origin, Username = "fixture", Password = "fixture-new" }, configured);
            var stored = Store();
            var notPaired = await Pairing.ConfigureAsync(new ConnectionSettings { BaseURL = ClosedLoopbackOrigin(), Username = "fixture", Password = "fixture-new" }, stored);
            report.Checks["configure_stdin_pairs_or_keeps_version_1"] = pairedNow && Keys(configured).SequenceEqual(expectedV2) && !notPaired && Keys(stored).SequenceEqual(new[] { "baseURL", "password", "username" });
            // Run again (a reinstall): the same install record is replaced, so no orphan device is left (review B5N finding 8).
            var firstConfigured = SecureStore.Load(configured)!;
            await Pairing.ConfigureAsync(new ConnectionSettings { BaseURL = fixture.Origin, Username = "fixture", Password = "fixture-new" }, configured);
            var againConfigured = SecureStore.Load(configured)!;
            report.Checks["configure_stdin_again_replaces_its_own_device_record"] = againConfigured.InstallId == firstConfigured.InstallId && fixture.LastInstallId == firstConfigured.InstallId
                && againConfigured.DeviceToken != firstConfigured.DeviceToken && !fixture.Accepts(firstConfigured.DeviceToken!) && fixture.Accepts(againConfigured.DeviceToken!);

            report.Checks["pairing_requests_reach_only_the_loopback_fixture"] = fixture.Unexpected == 0;
            var bare = new MainWindow(new Preferences(), loadConnection: false);
            windows.Add(bare);
            report.Checks["pairing_checks_never_write_the_real_preferences"] = Stamp(realPreferences) == realPreferencesBefore && Preferences.Load(preferencesFile).LastPairAttempt is { Length: > 0 }
                && bare.PreferencesForCheck.Detached && !windows[0].PreferencesForCheck.Detached;
        }
        catch (Exception error)
        {
            report.Checks["pairing_checks_completed"] = false;
            report.Notes["pairing_checks"] = error.GetType().Name + ": " + error.Message + " @ " + (error.StackTrace ?? "").Split('\n').FirstOrDefault()?.Trim();
        }
        finally
        {
            MainWindow.Launch = realLaunch;
            Motion.Enabled = motion;
            foreach (var window in windows) { window.AllowClose = true; window.Close(); }
            try { Directory.Delete(folder, true); } catch { }
        }
    }
}
