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
        if (step is "pair" or "pair-again" or "migrate" or "wrong-password" or "pairing-off")
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
                case "migrate":
                {
                    // A fake version 1 file: the old shape, the same DPAPI scope and entropy, written by the tray's own writer.
                    SecureStore.Save(new ConnectionSettings { BaseURL = input!.BaseURL, Username = input.Username, Password = input.Password }, store);
                    report.Checks["fake_version_1_written"] = Keys(store).SequenceEqual(new[] { "baseURL", "password", "username" });
                    window.UseConnectionStoreForCheck(store);
                    await window.Refresh(true);
                    var legacyReads = window.Dashboard is not null && window.ClientForCheck is { Paired: false };
                    await window.MigrateIfDue(force: true);
                    await window.SignInFlowTask;
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
