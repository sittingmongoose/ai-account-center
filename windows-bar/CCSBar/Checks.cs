using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using System.Threading.Tasks;

namespace CCSBar;

public sealed class CheckReport
{
    public string CheckedAt { get; set; } = DateTimeOffset.UtcNow.ToString("O");
    public bool Passed { get; set; }
    public Dictionary<string, bool> Checks { get; set; } = new();
    public int? Accounts { get; set; }
    public Dictionary<string, int>? Providers { get; set; }
    public bool? AutoSwitchEnabled { get; set; }
    public bool ReadOnly { get; set; } = true;
}

public static class Checks
{
    public static async Task<CheckReport> Run()
    {
        var report = new CheckReport();
        var interval = JsonSerializer.Deserialize<AccountRefreshSettings>("{\"refreshIntervalSeconds\":120}", Formatting.Json)!;
        report.Checks["saved_usage_refresh_interval_decoded"] = interval.ValidatedInterval == 120;
        report.Checks["invalid_usage_refresh_interval_defaults"] = new AccountRefreshSettings { RefreshIntervalSeconds = 1 }.ValidatedInterval == 60;
        report.Checks["unknown_quota_is_not_zero"] = new QuotaWindow().DisplayPercent is null;
        report.Checks["over_limit_quota_preserves_actual_percentage"] = new QuotaWindow { UsedPercent = 120 }.DisplayPercent == 120;
        report.Checks["invalid_quota_is_not_displayed"] = new[] { -1d, double.NaN, double.PositiveInfinity, double.NegativeInfinity }.All(value => new QuotaWindow { UsedPercent = value, RemainingPercent = 20 }.DisplayPercent is null);
        report.Checks["cached_numeric_usage_is_usable"] = new DashboardAccount { Status = "cached", Windows = new List<QuotaWindow> { new() { UsedPercent = 120 } } }.HasUsableUsage;
        report.Checks["empty_or_invalid_windows_are_not_usable"] = !new DashboardAccount { Status = "cached", Windows = new List<QuotaWindow> { new(), new() { UsedPercent = double.NaN, Remaining = double.PositiveInfinity } } }.HasUsableUsage;
        report.Checks["remaining_quota_converted_to_used"] = new QuotaWindow { RemainingPercent = 20 }.DisplayPercent == 80;
        report.Checks["unknown_reset_is_not_inferred"] = Formatting.Reset(null) == "Reset time unavailable" && Formatting.ShortReset(null) == "Reset unavailable";
        report.Checks["expired_reset_indicates_wait"] = Formatting.Reset("2026-01-01T00:00:00Z", DateTimeOffset.Parse("2026-01-02T00:00:00Z")).Contains("awaiting update", StringComparison.Ordinal);
        report.Checks["future_reset_has_countdown"] = Formatting.Reset("2026-01-02T02:00:00Z", DateTimeOffset.Parse("2026-01-02T00:00:00Z")).Contains("2h 0m", StringComparison.Ordinal);
        report.Checks["old_sample_displays_date"] = Formatting.Sampled("2025-01-01T00:00:00Z") == "Updated " + DateTimeOffset.Parse("2025-01-01T00:00:00Z").ToLocalTime().ToString("MMM d, h:mm tt", System.Globalization.CultureInfo.CurrentCulture);
        report.Checks["expiration_is_distinct_from_reset"] = Formatting.Expiration("2027-01-01T00:00:00Z").StartsWith("Expires ", StringComparison.Ordinal) && Formatting.Expiration(null) == "Expiration date unavailable";
        report.Checks["balance_fraction_preserved_without_currency_inference"] = Formatting.Amount(42.75, null).Contains(42.75.ToString("0.##", System.Globalization.CultureInfo.CurrentCulture), StringComparison.Ordinal) && !Formatting.Amount(42.75, null).Contains("USD", StringComparison.Ordinal);
        var balance = JsonSerializer.Deserialize<QuotaWindow>("{\"kind\":\"balance\",\"remaining\":42.75,\"expiresAt\":\"2027-01-01T00:00:00Z\",\"unlimited\":true,\"enabled\":false}", Formatting.Json)!;
        report.Checks["optional_usage_metadata_decoded"] = balance.Kind == "balance" && balance.Remaining == 42.75 && balance.ExpiresAt == "2027-01-01T00:00:00Z" && balance.Unlimited && balance.Enabled == false && balance.DisplayPercent is null;
        report.Checks["profile_path_and_uri_injection_rejected"] = !Formatting.IsSafeProfile("../gmail") && !Formatting.IsSafeProfile("gmail?x=1") && !Formatting.IsWindowsClaudeProfile("gmail/../../") && !Formatting.IsWindowsClaudeProfile("arbitrary");
        report.Checks["four_windows_launch_ids_allowed"] = new[] { "platyr", "gmail", "party", "me" }.All(Formatting.IsWindowsClaudeProfile);
        report.Checks["connection_with_path_rejected"] = RejectConnection("http://127.0.0.1:3000/account");
        report.Checks["cleartext_remote_connection_rejected"] = RejectConnection("http://example.com");
        report.Checks["dpapi_round_trip"] = SecureStore.CheckRoundTrip(Encoding.UTF8.GetBytes("test-only-secret-value"));
        await ConfirmationChecks(report);
        report.Checks["authenticated_cookie_origin_contract"] = await MockServer();
        report.Passed = report.Checks.Values.All(value => value);
        return report;
    }

    public static async Task<CheckReport> Live()
    {
        var report = new CheckReport();
        try
        {
            var settings = SecureStore.Load();
            report.Checks["private_connection_decrypted"] = settings is not null;
            if (settings is null) return report;
            using var client = new DashboardClient(settings);
            var dashboard = await client.Dashboard(false);
            report.Checks["schema_v1"] = dashboard.SchemaVersion == 1;
            report.Checks["account_inventory_present"] = dashboard.Accounts.Count > 0;
            report.Checks["only_requested_providers"] = dashboard.Accounts.All(a => a.Provider is "codex" or "claude" or "antigravity" or "muse" or "cursor" or "kimi-code" or "qwen" or "zai" or "opencode-go");
            report.Checks["safe_codex_action_ids"] = dashboard.Accounts.Where(a => a.Capabilities.CodexProfile is not null).All(a => a.Provider == "codex" && Formatting.IsSafeProfile(a.Capabilities.CodexProfile));
            report.Checks["safe_claude_action_ids"] = dashboard.Accounts.Where(a => a.Capabilities.ClaudeProfileId is not null).All(a => a.Provider == "claude" && Formatting.IsWindowsClaudeProfile(a.Capabilities.ClaudeProfileId));
            report.Accounts = dashboard.Accounts.Count;
            report.Providers = dashboard.Accounts.GroupBy(a => a.Provider).ToDictionary(g => g.Key, g => g.Count());
            report.AutoSwitchEnabled = dashboard.CodexAutoSwitch.Enabled;
            report.Passed = report.Checks.Values.All(value => value);
        }
        catch { report.Checks["live_dashboard_read"] = false; }
        return report;
    }

    private static bool RejectConnection(string url)
    {
        try { new ConnectionSettings { BaseURL = url, Username = "test", Password = "test" }.Validate(); return false; }
        catch (ArgumentException) { return true; }
    }

    private static async Task<bool> MockServer()
    {
        // A local transport fixture verifies login, HttpOnly session cookies, JSON bodies,
        // exact Origin and paths. It cannot change a real Codex or Claude account.
        var socket = new TcpListener(IPAddress.Loopback, 0); socket.Start();
        var port = ((IPEndPoint)socket.LocalEndpoint).Port; socket.Stop();
        var origin = $"http://127.0.0.1:{port}";
        using var listener = new HttpListener(); listener.Prefixes.Add(origin + "/"); listener.Start();
        var valid = true;
        var loop = Task.Run(async () =>
        {
            for (int i = 0; i < 10; i++)
            {
                var context = await listener.GetContextAsync();
                var request = context.Request;
                valid &= request.Headers["Origin"] == origin;
                var path = request.Url!.AbsolutePath;
                object payload;
                if (path == "/api/auth/login")
                {
                    using var reader = new StreamReader(request.InputStream);
                    using var json = JsonDocument.Parse(await reader.ReadToEndAsync());
                    valid &= request.HttpMethod == "POST" && json.RootElement.GetProperty("username").GetString() == "fixture" && json.RootElement.GetProperty("password").GetString() == "fixture-only";
                    context.Response.SetCookie(new Cookie("fixture-session", "yes", "/") { HttpOnly = true });
                    payload = new { success = true };
                }
                else if (request.Cookies["fixture-session"]?.Value != "yes")
                {
                    context.Response.StatusCode = 401; payload = new { error = "Authentication required" };
                }
                else if (path == "/api/accounts/dashboard")
                {
                    valid &= request.QueryString["platform"] == "windows" && request.QueryString["refresh"] == "true";
                    payload = new AccountDashboard { SchemaVersion = 1, UpdatedAt = DateTimeOffset.UtcNow.ToString("O"), CodexAutoSwitch = new AutoSwitchStatus { Enabled = true } };
                }
                else if (path == "/api/codex/profiles/auto-switch")
                {
                    using var reader = new StreamReader(request.InputStream); using var json = JsonDocument.Parse(await reader.ReadToEndAsync());
                    var fields = json.RootElement.EnumerateObject().Count();
                    valid &= request.HttpMethod == "PUT" && !json.RootElement.GetProperty("enabled").GetBoolean() && fields is 1 or 2;
                    var configured = json.RootElement.TryGetProperty("thresholdPercent", out var threshold);
                    valid &= configured ? fields == 2 && threshold.GetInt32() == 10 : fields == 1;
                    payload = new AutoSwitchStatus { Enabled = false, ThresholdPercent = configured ? 10 : 5 };
                }
                else if (path == "/api/codex/profiles/party/activate")
                {
                    using var reader = new StreamReader(request.InputStream); using var json = JsonDocument.Parse(await reader.ReadToEndAsync());
                    valid &= request.HttpMethod == "POST";
                    if (!json.RootElement.TryGetProperty("confirmationToken", out var token))
                    {
                        valid &= !json.RootElement.EnumerateObject().Any();
                        context.Response.StatusCode = 409;
                        payload = new { code = "busy", reason = "running_processes", confirmation = Proposal() };
                    }
                    else
                    {
                        valid &= json.RootElement.EnumerateObject().Count() == 1;
                        if (token.GetString() == "fixture-session-changed") { context.Response.StatusCode = 401; payload = new { error = "Session changed" }; }
                        else { valid &= token.GetString() == "fixture-confirmation-token"; payload = new { success = true }; }
                    }
                }
                else
                {
                    valid &= request.HttpMethod == "POST" && path is "/api/codex/profiles/gmail/activate" or "/api/claude/desktop-profiles/platyr/open";
                    payload = new { success = true };
                }
                var bytes = JsonSerializer.SerializeToUtf8Bytes(payload, Formatting.Json);
                context.Response.ContentType = "application/json"; context.Response.ContentLength64 = bytes.Length;
                await context.Response.OutputStream.WriteAsync(bytes); context.Response.Close();
            }
        });
        using var client = new DashboardClient(new ConnectionSettings { BaseURL = origin, Username = "fixture", Password = "fixture-only" });
        try
        {
            var summary = await client.Dashboard(true); valid &= summary.SchemaVersion == 1;
            valid &= !(await client.SetAutoSwitch(false)).Enabled;
            var configured = await client.SetAutoSwitch(false, 10);
            valid &= !configured.Enabled && configured.ThresholdPercent == 10;
            await client.Activate("gmail"); await client.OpenClaudeOnMac("platyr");
            valid &= await CodexSwitchFlow.Run("party", async token => { await client.Activate("party", token); }, confirmation => Task.FromResult(confirmation.Processes[0].Label == "Fixture Codex desktop"));
            bool authRejected = false;
            try { await client.Activate("party", "fixture-session-changed"); }
            catch (InvalidOperationException error) { authRejected = error.Message.Contains("session changed"); }
            valid &= authRejected;
            await loop.WaitAsync(TimeSpan.FromSeconds(10));
        }
        catch { valid = false; }
        finally { listener.Stop(); }
        return valid;
    }

    private static CodexSwitchConfirmation Proposal() => new()
    {
        Token = "fixture-confirmation-token", ExpiresAt = DateTimeOffset.UtcNow.AddMinutes(1).ToString("O"), TargetProfile = "party",
        Processes = new List<CodexSwitchProcess> { new() { Label = "Fixture Codex desktop", Pid = 1234, Role = "desktop" } },
        Warning = "Fixture only. Active work may be interrupted."
    };

    private static async Task ConfirmationChecks(CheckReport report)
    {
        int calls = 0; bool tokenSent = false;
        var cancelled = await CodexSwitchFlow.Run("party", token => { calls++; tokenSent |= token is not null; throw new CodexConfirmationRequiredException(Proposal()); }, _ => Task.FromResult(false));
        report.Checks["confirmation_cancel_never_posts_token"] = !cancelled && calls == 1 && !tokenSent;
        calls = 0;
        var approved = await CodexSwitchFlow.Run("party", token =>
        {
            calls++; if (token is null) throw new CodexConfirmationRequiredException(Proposal());
            return token == "fixture-confirmation-token" ? Task.CompletedTask : Task.FromException(new Exception());
        }, _ => Task.FromResult(true));
        report.Checks["confirmation_approval_posts_exact_token_once"] = approved && calls == 2;
        calls = 0; var expired = Proposal(); expired.ExpiresAt = DateTimeOffset.UtcNow.AddSeconds(-1).ToString("O");
        bool expiredRejected = false;
        try { await CodexSwitchFlow.Run("party", token => { calls++; throw new CodexConfirmationRequiredException(expired); }, _ => Task.FromResult(true)); }
        catch (InvalidOperationException) { expiredRejected = true; }
        report.Checks["expired_confirmation_requires_fresh_activate"] = expiredRejected && calls == 1;
        calls = 0; bool staleRejected = false;
        try
        {
            await CodexSwitchFlow.Run("party", token => { calls++; if (token is null) throw new CodexConfirmationRequiredException(Proposal()); throw new InvalidOperationException("confirmation_stale"); }, _ => Task.FromResult(true));
        }
        catch (InvalidOperationException) { staleRejected = true; }
        report.Checks["changed_process_confirmation_is_not_retried"] = staleRejected && calls == 2;
        var malformed = Proposal(); malformed.Token = null!;
        report.Checks["confirmation_target_and_shape_validated"] = !Proposal().IsValidFor("gmail") && !malformed.IsValidFor("party");
    }
}
