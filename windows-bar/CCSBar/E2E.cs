using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Threading.Tasks;
using System.Windows;

namespace CCSBar;

/// <summary>
/// Live end-to-end steps against a sandbox dashboard (--e2e STEP DIR), driven through the tray's own sign-in screen:
/// the same fields, the same primary button and the same flows a click runs. It refuses to run unless
/// AAC_TRAY_STATE_DIR names an isolated folder (never the tray's real CCS Bar folder), so the connection it pairs,
/// signs out and migrates is a throwaway one. Credentials arrive on stdin and are never written to a report; reports
/// carry states, titles, store key names, status codes and counts only. Each state the screen shows is rendered to
/// DIR\shots for review (deleted afterwards).
/// </summary>
public static class E2E
{
    private sealed class Input { public string BaseURL { get; set; } = ""; public string Username { get; set; } = ""; public string Password { get; set; } = ""; }

    public static async Task<CheckReport> Run(string step, string directory)
    {
        var report = new CheckReport { ReadOnly = false };
        Directory.CreateDirectory(directory);
        var real = Path.GetFullPath(Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "CCS Bar")).TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar;
        var state = Path.GetFullPath(SecureStore.StateDirectory).TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar;
        var isolated = Environment.GetEnvironmentVariable("AAC_TRAY_STATE_DIR") is { Length: > 0 } && !state.StartsWith(real, StringComparison.OrdinalIgnoreCase);
        report.Checks["e2e_isolated_state_folder"] = isolated;
        if (!isolated) return report;
        Motion.Enabled = false;
        var store = SecureStore.SettingsPath;
        Input? input = null;
        if (step is "pair" or "pair-again" or "migrate" or "migrate-aged" or "wrong-password" or "pairing-off" or "repair-held" or "password-off" or "configure-twice")
        {
            var text = await Console.In.ReadToEndAsync();
            input = JsonSerializer.Deserialize<Input>(text, Formatting.Json);
        }
        var shots = Path.Combine(directory, "shots");
        Directory.CreateDirectory(shots);
        var window = new MainWindow(new Preferences { Theme = "light", Hotkey = false }, loadConnection: false)
        { ShowActivated = false, Left = 40, Top = 40, Width = 760, Height = 850, StepPace = TimeSpan.FromMilliseconds(150), SuccessPause = TimeSpan.FromMilliseconds(400) };
        var states = new List<string>();
        var shot = 0;
        try
        {
            window.UseConnectionStoreForCheck(store);
            window.Show();
            window.SignIn.StateApplied += applied =>
            {
                states.Add(applied.ToString());
                var name = Path.Combine(shots, $"{step}-{++shot:00}-{applied}.png".ToLowerInvariant());
                // After the layer, the header and the footer have settled for this state.
                window.Dispatcher.BeginInvoke(new Action(() => { try { window.UpdateLayout(); FixtureRender.SavePng(window, name); } catch { } }), System.Windows.Threading.DispatcherPriority.Loaded);
            };
            switch (step)
            {
                case "wrong-password":
                    await StartAndAsk(window, input!.BaseURL, report);
                    window.SignIn.User.Value = input.Username; window.SignIn.Pass.Value = input.Password + "-wrong";
                    await Checks.SubmitForE2E(window);
                    report.Checks["wrong_password_screen"] = window.SignIn.Model.State == SignInState.WrongPassword && window.SignIn.MessageText.Contains("tries left", StringComparison.Ordinal) && !File.Exists(store);
                    report.Notes["wrong_password_message"] = window.SignIn.MessageText;
                    break;
                case "pairing-off":
                    await StartAndAsk(window, input!.BaseURL, report);
                    report.Checks["pairing_off_screen"] = window.SignIn.Model.State == SignInState.PairingOff && window.SignIn.TitleText == "Pairing is turned off for remote computers" && !File.Exists(store);
                    break;
                case "pair":
                    await StartAndAsk(window, input!.BaseURL, report);
                    report.Checks["address_leads_to_the_password"] = window.SignIn.Model.State == SignInState.Password;
                    window.SignIn.User.Value = input.Username; window.SignIn.Pass.Value = input.Password;
                    await Checks.SubmitForE2E(window);
                    await Paired(window, store, report, "pair");
                    break;
                case "reads":
                    await Reads(window, store, report);
                    break;
                case "rotate":
                {
                    var old = SecureStore.Load(store)!.DeviceToken!;
                    await window.MaintainDeviceKey(force: true);
                    var now = SecureStore.Load(store)!;
                    await window.Refresh(true);
                    report.Checks["rotation_saved_a_new_key"] = now.DeviceToken != old && DeviceTokenFormat.IsToken(now.DeviceToken) && Hash(now.DeviceToken!) != Hash(old);
                    report.Checks["rotation_new_key_reads"] = window.Dashboard is not null && !window.IsStale && window.ClientForCheck is { Paired: true };
                    using var api = new AuthApi(new Uri(now.BaseURL + "/"), TimeSpan.FromSeconds(10));
                    var stale = await api.DeviceMe(old);
                    report.Notes["rotation_old_key_status"] = ((int)stale.Status).ToString(System.Globalization.CultureInfo.InvariantCulture) + " " + stale.Code;
                    report.Checks["rotation_old_key_stops_working_once_the_new_one_is_used"] = (int)stale.Status == 401 && stale.Code == "invalid_token";
                    report.Notes["rotate_after"] = now.RotateAfter ?? "";
                    break;
                }
                case "revoked":
                {
                    await window.Refresh(true);
                    var view = window.SignIn;
                    report.Notes["revoked_title"] = view.TitleText; report.Notes["revoked_lede"] = view.LedeText; report.Notes["revoked_state"] = view.Model.State.ToString();
                    report.Checks["revoked_shows_the_signed_out_screen"] = window.SignInVisible && view.Model.State is SignInState.SignedOut or SignInState.SignedOutAll && view.TitleText == "This tray was signed out" && view.PrimaryLabel == "Pair again";
                    report.Checks["revoked_deleted_the_key_and_kept_address_and_username"] = SecureStore.Load(store) is { IsSignedOut: true, Version: 2 } stub && stub.Username.Length > 0 && stub.BaseURL.Length > 0 && !Keys(store).Contains("deviceToken") && !Keys(store).Contains("password");
                    report.Checks["revoked_status_and_tooltip_say_signed_out"] = window.SignInStatus == "Signed out" && Formatting.TrayTooltip(null, configured: false, signInState: window.SignInStatus) == "AI Account Center · Signed out";
                    report.Notes["store_keys"] = string.Join(",", Keys(store));
                    break;
                }
                case "pair-again":
                {
                    window.ShowSignInForStoredConnection();
                    var view = window.SignIn;
                    report.Checks["signed_out_screen_after_restart"] = view.Model.State is SignInState.SignedOut or SignInState.SignedOutAll && view.User.Value == input!.Username;
                    view.Pass.Value = input!.Password;
                    await Checks.SubmitForE2E(window);
                    await Paired(window, store, report, "pair_again");
                    break;
                }
                case "disconnect":
                {
                    await window.Refresh(true);
                    var failure = await window.DisconnectTray();
                    var view = window.SignIn;
                    report.Checks["disconnect_revoked_and_forgot_the_key"] = failure is null && SecureStore.Load(store) is { IsSignedOut: true, SignedOutReason: "disconnected" } && window.ClientForCheck is null;
                    report.Checks["disconnect_first_run_with_the_address"] = view.Model.State == SignInState.FirstRun && view.BannerText.StartsWith("Disconnected at", StringComparison.Ordinal) && view.Addr.Value.Length > 0;
                    report.Notes["disconnect_banner"] = view.BannerText;
                    break;
                }
                case "repair-held":
                {
                    // Re-pair, then Cancel and Escape while the pair request is out (review B5N finding 1): the answer is
                    // finished, the new key saved and used, and the old key is dead at the dashboard.
                    await window.Refresh(true);
                    var old = SecureStore.Load(store)!.DeviceToken!;
                    window.ShowSignIn(SignInState.Password, repair: true);
                    var view = window.SignIn;
                    view.Pass.Value = input!.Password;
                    Checks.PressForE2E(view.PrimaryButton);
                    var held = window.PairHeld && view.AltHeld && window.SignInStatus == "Re-pairing";
                    Checks.PressForE2E(view.AltButton);
                    window.EscapeForCheck();
                    var waits = await window.DisconnectTray();
                    var stillShown = window.SignInVisible;
                    await window.SignInFlowTask;
                    await window.Dispatcher.InvokeAsync(() => { }, System.Windows.Threading.DispatcherPriority.Background);
                    var now = SecureStore.Load(store)!;
                    report.Checks["repair_held_cancel_escape_and_disconnect_rest"] = held && stillShown && waits == "Pairing is finishing. Try again in a moment.";
                    report.Checks["repair_held_new_key_saved_and_used"] = !window.SignInVisible && window.ClientForCheck is { Paired: true } && window.Dashboard is not null && now.DeviceToken != old
                        && Keys(store).SequenceEqual(new[] { "baseURL", "deviceId", "deviceToken", "installId", "pairedAt", "rotateAfter", "username", "version" });
                    using var api = new AuthApi(new Uri(now.BaseURL + "/"), TimeSpan.FromSeconds(10));
                    var dead = await api.DeviceMe(old);
                    report.Notes["repair_held_old_key_status"] = ((int)dead.Status).ToString(System.Globalization.CultureInfo.InvariantCulture) + " " + dead.Code;
                    report.Checks["repair_held_old_key_replaced_at_the_dashboard"] = (int)dead.Status == 401;
                    report.Notes["repair_held_flash"] = window.StatusFlashForCheck ?? "";
                    break;
                }
                case "password-off":
                {
                    // Pairing turned off on the dashboard, on a tray without a key: the password option signs in (verified
                    // before it is saved) and stores version 1 (review B5N finding 3).
                    await StartAndAsk(window, input!.BaseURL, report);
                    var view = window.SignIn;
                    report.Checks["password_off_offers_the_password"] = view.Model.State == SignInState.PairingOff && view.Model.PasswordFallback && FixtureRender.FindUid(view, "si-use-password") is not null;
                    if (FixtureRender.FindUid(view, "si-use-password") is System.Windows.Controls.Button use) Checks.PressForE2E(use);
                    report.Checks["password_off_password_form"] = view.Model.State == SignInState.Password && view.TitleText == "Sign in with your password" && view.PrimaryLabel == "Sign in";
                    view.User.Value = input.Username; view.Pass.Value = input.Password;
                    await Checks.SubmitForE2E(window);
                    report.Checks["password_off_signed_in_with_version_1"] = !window.SignInVisible && window.ClientForCheck is { Paired: false } && window.Dashboard is not null
                        && Keys(store).SequenceEqual(new[] { "baseURL", "password", "username" });
                    report.Notes["password_off_flash"] = window.StatusFlashForCheck ?? "";
                    break;
                }
                case "configure-twice":
                {
                    // --configure-stdin run twice (a reinstall): one device record, the first key replaced (finding 8).
                    var settings = new ConnectionSettings { BaseURL = input!.BaseURL, Username = input.Username, Password = input.Password };
                    var once = await Pairing.ConfigureAsync(settings, store);
                    var first = SecureStore.Load(store)!;
                    var twice = await Pairing.ConfigureAsync(new ConnectionSettings { BaseURL = input.BaseURL, Username = input.Username, Password = input.Password }, store);
                    var second = SecureStore.Load(store)!;
                    using var api = new AuthApi(new Uri(second.BaseURL + "/"), TimeSpan.FromSeconds(10));
                    var firstNow = await api.DeviceMe(first.DeviceToken!);
                    var secondNow = await api.DeviceMe(second.DeviceToken!);
                    report.Checks["configure_twice_paired_both_times"] = once && twice && first.IsPaired && second.IsPaired && !second.HasPassword;
                    report.Checks["configure_twice_same_install"] = first.InstallId is { Length: 36 } && second.InstallId == first.InstallId && second.DeviceToken != first.DeviceToken;
                    report.Notes["configure_twice_key_status"] = ((int)firstNow.Status).ToString(System.Globalization.CultureInfo.InvariantCulture) + " " + firstNow.Code + " / " + ((int)secondNow.Status).ToString(System.Globalization.CultureInfo.InvariantCulture);
                    report.Checks["configure_twice_first_key_replaced"] = (int)firstNow.Status == 401 && (int)secondNow.Status == 200;
                    break;
                }
                case "migrate":
                case "migrate-aged":
                {
                    // A fake version 1 file: the old shape, the same DPAPI scope and entropy, written by the tray's own writer.
                    // migrate-aged dates it 40 days back, as a real version 1 file is (review B5N finding 2).
                    SecureStore.Save(new ConnectionSettings { BaseURL = input!.BaseURL, Username = input.Username, Password = input.Password }, store);
                    if (step == "migrate-aged") File.SetLastWriteTimeUtc(store, DateTime.UtcNow.AddDays(-40));
                    report.Checks["fake_version_1_written"] = Keys(store).SequenceEqual(new[] { "baseURL", "password", "username" });
                    window.UseConnectionStoreForCheck(store);
                    await window.Refresh(true);
                    var legacyReads = window.Dashboard is not null && window.ClientForCheck is { Paired: false };
                    // The rollback copy is watched while the migration runs: its 24 hours must count from now.
                    TimeSpan? rollbackAge = null;
                    var migrating = window.MigrateIfDue(force: true);
                    while (!migrating.IsCompleted)
                    {
                        if (SecureStore.RollbackAge(store) is TimeSpan age) rollbackAge ??= age;
                        await Task.Delay(5);
                    }
                    await migrating;
                    await window.SignInFlowTask;
                    report.Notes["migration_rollback_age_seen_seconds"] = rollbackAge?.TotalSeconds.ToString("0.0", System.Globalization.CultureInfo.InvariantCulture) ?? "not seen";
                    if (step == "migrate-aged") report.Checks["migration_rollback_counts_from_the_migration"] = rollbackAge is { } seen && seen < TimeSpan.FromMinutes(10) && seen > TimeSpan.FromMinutes(-10);
                    report.Checks["version_1_read_with_the_password_first"] = legacyReads;
                    report.Checks["migration_showed_securing_then_success"] = states.Contains(nameof(SignInState.Securing)) && states.Contains(nameof(SignInState.Success));
                    report.Checks["migration_stored_the_key_without_the_password"] = Keys(store).SequenceEqual(new[] { "baseURL", "deviceId", "deviceToken", "installId", "pairedAt", "rotateAfter", "username", "version" }) && SecureStore.Load(store) is { Version: 2, HasPassword: false, IsPaired: true };
                    report.Checks["migration_deleted_the_rollback"] = SecureStore.RollbackAge(store) is null;
                    report.Checks["migration_reads_with_the_key"] = window.ClientForCheck is { Paired: true } && window.Dashboard is not null && !window.SignInVisible;
                    break;
                }
                default:
                    report.Checks["e2e_known_step"] = false;
                    break;
            }
        }
        catch (Exception error)
        {
            report.Checks["e2e_" + step.Replace('-', '_') + "_completed"] = false;
            report.Notes["e2e_error"] = error.GetType().Name + ": " + error.Message;
        }
        finally
        {
            report.Notes["states"] = string.Join(" > ", states);
            window.AllowClose = true; window.Close();
        }
        report.Passed = report.Checks.Values.All(value => value);
        return report;
    }

    private static string[] Keys(string path) => File.Exists(path) ? SecureStore.StoredKeysForCheck(path).OrderBy(key => key, StringComparer.Ordinal).ToArray() : Array.Empty<string>();
    private static string Hash(string text) => Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(text)));

    private static async Task StartAndAsk(MainWindow window, string address, CheckReport report)
    {
        window.ShowSignIn(SignInState.FirstRun);
        report.Checks["first_run_starts_empty"] = window.SignIn.Addr.Value.Length == 0 && window.SignIn.TitleText == "Connect this PC";
        window.SignIn.Addr.Value = address;
        await Checks.SubmitForE2E(window);
        report.Notes["after_address"] = window.SignIn.Model.State.ToString();
    }

    private static async Task Paired(MainWindow window, string store, CheckReport report, string tag)
    {
        report.Checks[tag + "_handed_off_to_the_list"] = !window.SignInVisible && window.ClientForCheck is { Paired: true } && window.Dashboard is not null;
        report.Checks[tag + "_stored_version_2_without_the_password"] = Keys(store).SequenceEqual(new[] { "baseURL", "deviceId", "deviceToken", "installId", "pairedAt", "rotateAfter", "username", "version" })
            && SecureStore.Load(store) is { Version: 2, HasPassword: false, IsPaired: true };
        var me = await window.ClientForCheck!.DeviceMe();
        report.Checks[tag + "_device_record"] = me.Platform == "windows" && me.Name == Environment.MachineName && System.Text.RegularExpressions.Regex.IsMatch(me.Id, "^dev_[0-9a-f]{16}$");
        report.Notes[tag + "_device_id"] = me.Id;
        report.Notes[tag + "_flash"] = window.StatusFlashForCheck ?? "";
    }

    private static async Task Reads(MainWindow window, string store, CheckReport report)
    {
        var before = SecureStore.Load(store);
        await window.Refresh(true);
        var client = window.ClientForCheck;
        report.Checks["bearer_dashboard_read"] = client is { Paired: true } && window.Dashboard is not null && !window.IsStale && !window.SignInVisible;
        report.Measurements["accounts"] = window.Dashboard?.Accounts.Count ?? -1;
        if (client is null) return;
        var me = await client.DeviceMe();
        report.Checks["bearer_devices_me"] = me.Id.StartsWith("dev_", StringComparison.Ordinal);
        var profiles = await client.ClaudeDesktopProfiles();
        report.Checks["bearer_claude_profile_list"] = profiles is not null;
        report.Checks["still_the_same_key"] = SecureStore.Load(store)?.DeviceToken == before?.DeviceToken;
    }
}
