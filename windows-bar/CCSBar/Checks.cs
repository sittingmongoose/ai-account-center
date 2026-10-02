using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace CCSBar;

public sealed class CheckReport
{
    public string CheckedAt { get; set; } = DateTimeOffset.UtcNow.ToString("O");
    public bool Passed { get; set; }
    public Dictionary<string, bool> Checks { get; set; } = new();
    public Dictionary<string, double> Measurements { get; set; } = new();
    /// <summary>Exception types from offline checks only (fixtures, never connection data).</summary>
    public Dictionary<string, string> Notes { get; set; } = new();
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
        report.Checks["displayed_fractions_round_to_two_decimals"] = Formatting.Amount(123.4567, "USD") == (123.46).ToString("N2", System.Globalization.CultureInfo.CurrentCulture) + " USD" && Formatting.Amount(1000, null) == (1000).ToString("N0", System.Globalization.CultureInfo.CurrentCulture);
        var rawQuota = new QuotaWindow { Key = "monthly", Label = "Monthly", Used = 49.8765, Limit = 500.4321, UsedPercent = 9.966683, Remaining = 450.5556 };
        Formatting.Amount(rawQuota.Remaining.Value, null);
        report.Checks["presentation_rounding_preserves_raw_precision"] = rawQuota.Used == 49.8765 && rawQuota.Limit == 500.4321 && rawQuota.UsedPercent == 9.966683 && rawQuota.Remaining == 450.5556;
        var pro = new DashboardAccount { Provider = "codex", Capabilities = new AccountCapabilities { CodexProfile = "party" }, Windows = new List<QuotaWindow> { new() { Key = "five_hour", Label = "5h", WindowMinutes = 300 }, new() { Key = "weekly", Label = "Weekly", UsedPercent = 8 }, new() { Key = "extra_additional_2", Label = "Chat pass weekly", UsedPercent = 20 } } };
        report.Checks["pro_unreported_fivehour_and_chatpass_hidden"] = Formatting.VisibleWindows(pro).Select(window => window.Key).SequenceEqual(new[] { "weekly" });
        pro.Capabilities.CodexProfile = "lex"; pro.Windows[0].UsedPercent = 120;
        report.Checks["reported_lex_fivehour_retained"] = Formatting.VisibleWindows(pro).Any(window => window.Key == "five_hour" && window.DisplayPercent == 120);
        var fullCodex = JsonSerializer.Deserialize<DashboardAccount>("""
            {"provider":"codex","capabilities":{"codexProfile":"lex"},"windows":[
                {"key":"extra_additional_1","label":"Additional 5-hour quota","kind":"rate_limit","windowMinutes":300,"usedPercent":92,"resetAt":"2027-01-01T12:00:00Z"},
                {"key":"five_hour","label":"5h","kind":"rate_limit","windowMinutes":300,"usedPercent":0,"resetAt":"2027-01-01T13:00:00Z"},
                {"key":"seven_day","label":"Weekly","kind":"rate_limit","windowMinutes":10080,"usedPercent":27,"resetAt":"2027-01-05T12:00:00Z"},
                {"key":"extra_monthly","label":"Additional monthly quota","kind":"rate_limit","windowMinutes":43200,"usedPercent":71},
                {"key":"extra_balance","label":"Extra credits","kind":"balance","remaining":42.75},
                {"key":"extra_chat","label":"Chat pass quota","kind":"rate_limit","windowMinutes":300,"usedPercent":80}]}
            """, Formatting.Json)!;
        report.Checks["codex_full_dto_primary_only_exact_canonical_core_keys"] = Formatting.CodexPrimaryWindows(fullCodex).Select(window => window.Key).SequenceEqual(new[] { "five_hour", "seven_day" });
        report.Checks["codex_canonical_zero_is_real_and_extra_quotas_stay_in_details"] = Formatting.CodexPrimaryWindows(fullCodex)[0].DisplayPercent == 0 && fullCodex.Windows.Count == 6 && Formatting.VisibleWindows(fullCodex).Select(window => window.Key).SequenceEqual(new[] { "extra_additional_1", "five_hour", "seven_day", "extra_monthly", "extra_balance" });
        fullCodex.Windows.RemoveAll(window => window.Key is "five_hour" or "seven_day");
        report.Checks["codex_no_core_dto_does_not_promote_additional_duration_or_balance"] = Formatting.CodexPrimaryWindows(fullCodex).Length == 0 && Formatting.VisibleWindows(fullCodex).Length == 3 && fullCodex.Windows.Any(window => window.WindowMinutes == 300 && window.DisplayPercent == 92);
        foreach (var profile in new[] { "gmail", "party" })
        {
            fullCodex.Capabilities.CodexProfile = profile;
            fullCodex.Windows.Add(new() { Key = "five_hour", Label = "5h", WindowMinutes = 300 });
            fullCodex.Windows.Add(new() { Key = "seven_day", Label = "Weekly", WindowMinutes = 10080, UsedPercent = 0 });
            report.Checks["codex_" + profile + "_absent_pro_fivehour_not_substituted_by_extra"] = Formatting.CodexPrimaryWindows(fullCodex).Select(window => window.Key).SequenceEqual(new[] { "seven_day" }) && Formatting.CodexPrimaryWindows(fullCodex)[0].DisplayPercent == 0;
            fullCodex.Windows.RemoveAll(window => window.Key is "five_hour" or "seven_day");
        }
        var claude = new DashboardAccount { Provider = "claude", Plan = "max", Windows = new List<QuotaWindow> { new() { Key = "seven_day_fable", Label = "Weekly Fable usage", Kind = "rate_limit", WindowMinutes = 10080, UsedPercent = 0, ResetAt = "2026-10-08T12:00:00Z" }, new() { Key = "seven_day_opus", Label = "Weekly Opus usage", UsedPercent = 32 } } };
        report.Checks["max_fable_retains_actual_zero_percentage_and_reset"] = Formatting.VisibleWindows(claude).Any(window => window.Key == "seven_day_fable" && window.DisplayPercent == 0 && window.ResetAt == "2026-10-08T12:00:00Z");
        claude.Plan = "pro";
        report.Checks["fable_does_not_invent_pro_placeholder_or_opus_alias"] = Formatting.VisibleWindows(claude).Select(window => window.Key).SequenceEqual(new[] { "seven_day_opus" }) && Formatting.WindowLabel(claude, claude.Windows[1]) == "Weekly Opus usage";
        var qwen = new DashboardAccount { Provider = "qwen", Windows = new List<QuotaWindow> { rawQuota, new() { Key = "subscription", Label = "Plan subscription" }, new() { Key = "plan_subscription", Label = "Plan subscription" } } };
        report.Checks["qwen_usage_retains_numeric_quota_without_subscription"] = Formatting.VisibleWindows(qwen).Length == 1 && Formatting.WindowLabel(qwen, rawQuota) == "Monthly";
        qwen.Windows.AddRange(new[] { new QuotaWindow { Key = "addon-pack-a", Remaining = 10, ExpiresAt = "2027-01-01T00:00:00Z" }, new QuotaWindow { Key = "addon-pack-b", Remaining = 0 }, new QuotaWindow { Key = "addon-pack-c", Remaining = 9, ExpiresAt = "2025-01-01T00:00:00Z" }, new QuotaWindow { Key = "addon-pack-d" }, new QuotaWindow { Key = "addon-listed-packs", Remaining = 4 } });
        var packs = Formatting.QwenPacks(qwen);
        report.Checks["qwen_individual_pack_inventory_includes_zero_and_unknown"] = packs.Length == 4 && packs.Select(pack => pack.Key).SequenceEqual(new[] { "addon-pack-a", "addon-pack-b", "addon-pack-c", "addon-pack-d" });
        report.Checks["qwen_pack_status_uses_actual_amount_and_expiry"] = packs.Select(pack => Formatting.PackStatus(pack, DateTimeOffset.Parse("2026-01-01T00:00:00Z"))).SequenceEqual(new[] { "Available", "Depleted", "Expired", "Status unavailable" });
        report.Checks["qwen_unknown_pack_expiry_not_inferred"] = Formatting.Expiration(packs[1].ExpiresAt) == "Expiration date unavailable" && packs[1].ExpiresAt is null;
        var zai = new DashboardAccount { Provider = "zai", Windows = new List<QuotaWindow> { new() { Key = "reset-packs-5h", Remaining = 0 }, new() { Key = "reset-packs-weekly" }, new() { Key = "reset-packs-positive", Remaining = 2 }, new() { Key = "pack-record", Remaining = 0, ExpiresAt = "2027-01-01T00:00:00Z" } } };
        report.Checks["zai_empty_pack_summaries_hidden_actual_packs_retained"] = Formatting.VisibleWindows(zai).Select(window => window.Key).SequenceEqual(new[] { "reset-packs-positive", "pack-record" });
        var balance = JsonSerializer.Deserialize<QuotaWindow>("{\"kind\":\"balance\",\"remaining\":42.75,\"expiresAt\":\"2027-01-01T00:00:00Z\",\"unlimited\":true,\"enabled\":false}", Formatting.Json)!;
        report.Checks["optional_usage_metadata_decoded"] = balance.Kind == "balance" && balance.Remaining == 42.75 && balance.ExpiresAt == "2027-01-01T00:00:00Z" && balance.Unlimited && balance.Enabled == false && balance.DisplayPercent is null;
        var cachedWindow = JsonSerializer.Deserialize<QuotaWindow>("{\"kind\":\"balance\",\"remaining\":42.75,\"status\":\"cached\",\"sampledAt\":\"2026-09-28T13:00:00Z\"}", Formatting.Json)!;
        report.Checks["retained_window_cache_decodes_original_sample_timestamp"] = cachedWindow.Status == "cached" && cachedWindow.SampledAt == "2026-09-28T13:00:00Z" && cachedWindow.Remaining == 42.75;
        report.Checks["cached_window_missing_sample_time_stays_unknown"] = Formatting.WindowSample(new QuotaWindow { Status = "cached" }.SampledAt) == "Sample time unavailable" && Formatting.WindowSample("invalid") == "Sample time unavailable";
        report.Checks["profile_path_and_uri_injection_rejected"] = !Formatting.IsSafeProfile("../gmail") && !Formatting.IsSafeProfile("gmail?x=1") && !Formatting.IsSafeClaudeProfile("gmail/../../") && !Formatting.IsSafeClaudeProfile("me?x=1") && !Formatting.IsSafeClaudeProfile("") && !Formatting.IsSafeClaudeProfile(null) && !Formatting.IsSafeClaudeProfile("-leading") && !Formatting.IsSafeClaudeProfile(new string('a', 65));
        // No allowlist: any id the dashboard reports is launched when it is URI and path safe.
        report.Checks["claude_launch_ids_follow_the_data"] = new[] { "platyr", "gmail", "party", "me", "claude-example-1", "work_2" }.All(Formatting.IsSafeClaudeProfile);
        NewBehaviourChecks(report);
        report.Checks["connection_with_path_rejected"] = RejectConnection("http://127.0.0.1:3000/account");
        report.Checks["cleartext_remote_connection_rejected"] = RejectConnection("http://example.com");
        report.Checks["dpapi_round_trip"] = SecureStore.CheckRoundTrip(Encoding.UTF8.GetBytes("test-only-secret-value"));
        await PublicErrorChecks(report);
        await ConfirmationChecks(report);
        report.Checks["authenticated_cookie_origin_contract"] = await MockServer();
        ResetPendingChecks(report);
        await SignInChangeChecks(report);
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
            report.Checks["provider_ids_are_safe"] = dashboard.Accounts.All(a => Formatting.IsSafeId(a.Provider));
            report.Checks["safe_codex_action_ids"] = dashboard.Accounts.Where(a => a.Capabilities.CodexProfile is not null).All(a => a.Provider == "codex" && Formatting.IsSafeProfile(a.Capabilities.CodexProfile));
            report.Checks["safe_claude_action_ids"] = dashboard.Accounts.Where(a => a.Capabilities.ClaudeProfileId is not null).All(a => a.Provider == "claude" && Formatting.IsSafeClaudeProfile(a.Capabilities.ClaudeProfileId));
            report.Checks["safe_antigravity_action_ids"] = dashboard.Accounts.Where(a => a.Capabilities.AntigravityProfileId is not null).All(a => a.Provider == "antigravity" && Formatting.IsSafeId(a.Capabilities.AntigravityProfileId));
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
            for (int i = 0; i < 13; i++)
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
                else if (path == "/api/antigravity/auto-switch")
                {
                    using var reader = new StreamReader(request.InputStream); using var json = JsonDocument.Parse(await reader.ReadToEndAsync());
                    valid &= request.HttpMethod == "PUT" && request.ContentType?.StartsWith("application/json", StringComparison.Ordinal) == true
                        && json.RootElement.EnumerateObject().Count() == 1 && !json.RootElement.GetProperty("enabled").GetBoolean();
                    payload = new { enabled = false, thresholdUsedPercent = 95, pollIntervalSeconds = 60, outcome = "disabled", message = "Off", activationInProgress = false };
                }
                else if (path == "/api/antigravity/profiles/example-2/activate")
                {
                    using var reader = new StreamReader(request.InputStream); using var json = JsonDocument.Parse(await reader.ReadToEndAsync());
                    valid &= request.HttpMethod == "POST" && json.RootElement.EnumerateObject().Count() == 1 && json.RootElement.GetProperty("hostId").GetString() == "ubuntu";
                    context.Response.StatusCode = 409;
                    payload = new { status = "confirmation-required", profileId = "example-2", hostId = "ubuntu", reason = "running-processes", confirmation = AntigravityProposal() };
                }
                else if (path == "/api/antigravity/profiles/example-2/confirm")
                {
                    using var reader = new StreamReader(request.InputStream); using var json = JsonDocument.Parse(await reader.ReadToEndAsync());
                    valid &= request.HttpMethod == "POST" && json.RootElement.EnumerateObject().Count() == 2 && json.RootElement.GetProperty("hostId").GetString() == "ubuntu"
                        && json.RootElement.GetProperty("confirmationToken").GetString() == "fixture_antigravity_token_0001";
                    payload = new { status = "active", profileId = "example-2", hostId = "ubuntu" };
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
            valid &= !(await client.SetAntigravityAutoSwitch(enabled: false)).Enabled;
            int antigravityApprovals = 0;
            valid &= await AntigravitySwitchFlow.Run("example-2", async token => { await client.ActivateAntigravity("example-2", token); }, confirmation => { antigravityApprovals++; return Task.FromResult(confirmation.Processes.Count == 1 && confirmation.Processes[0].Label == "Antigravity CLI"); });
            valid &= antigravityApprovals == 1;
            await loop.WaitAsync(TimeSpan.FromSeconds(10));
        }
        catch { valid = false; }
        finally { listener.Stop(); }
        return valid;
    }

    private static object AntigravityProposal() => new
    {
        token = "fixture_antigravity_token_0001", expiresAt = DateTimeOffset.UtcNow.AddMinutes(1).ToString("O"), profileId = "example-2", hostId = "ubuntu",
        email = "antigravity-2@example.com", warning = "Fixture only.", processes = new[] { new { pid = 4321, role = "cli", label = "Antigravity CLI" } },
    };

    private static void NewBehaviourChecks(CheckReport report)
    {
        report.Checks["max_plan_detection_for_fable"] = new[] { "max", "Max", "max_5x", "Max 20x", "claude_max" }.All(Formatting.IsMaxPlan) && !new[] { "pro", "maximum", "", null }.Any(Formatting.IsMaxPlan);
        var mixed = new[] { "zai", "newcode", "codex", "claude", "antigravity", "zai" }.Select(provider => new DashboardAccount { Provider = provider }).ToArray();
        report.Checks["provider_order_keeps_any_provider"] = Formatting.ProviderOrder(mixed).SequenceEqual(new[] { "claude", "codex", "antigravity", "zai", "newcode" });
        var hiddenTop = JsonSerializer.Deserialize<AccountDashboard>("{\"schemaVersion\":1,\"accounts\":[],\"codexAutoSwitch\":{},\"hiddenProviders\":[\"kimi-code\",\"../x\"]}", Formatting.Json)!;
        var hiddenNested = JsonSerializer.Deserialize<AccountDashboard>("{\"schemaVersion\":1,\"accounts\":[],\"codexAutoSwitch\":{},\"settings\":{\"refreshIntervalSeconds\":60,\"hiddenProviders\":[\"qwen\"]}}", Formatting.Json)!;
        var hiddenNone = JsonSerializer.Deserialize<AccountDashboard>("{\"schemaVersion\":1,\"accounts\":[],\"codexAutoSwitch\":{}}", Formatting.Json)!;
        report.Checks["hidden_providers_honoured_when_present"] = hiddenTop.Hidden.SetEquals(new[] { "kimi-code" }) && hiddenNested.Hidden.SetEquals(new[] { "qwen" }) && hiddenNone.Hidden.Count == 0 && !hiddenNone.ReportsHidden && hiddenTop.ReportsHidden;
        var antigravity = JsonSerializer.Deserialize<AccountDashboard>("{\"schemaVersion\":1,\"accounts\":[{\"id\":\"antigravity:a\",\"provider\":\"antigravity\",\"isActive\":true,\"capabilities\":{\"antigravityProfileId\":\"a\",\"antigravityHostIds\":[\"ubuntu\"],\"antigravityCanActivate\":true}}],\"codexAutoSwitch\":{},\"antigravityAutoSwitch\":{\"enabled\":true,\"thresholdUsedPercent\":90,\"activationInProgress\":false}}", Formatting.Json)!;
        report.Checks["antigravity_dto_decoded"] = antigravity.Accounts[0].Capabilities.AntigravityProfileId == "a" && antigravity.Accounts[0].Capabilities.AntigravityCanActivate && antigravity.AntigravityAutoSwitch?.ThresholdUsedPercent == 90 && antigravity.AntigravityAutoSwitch.Enabled;
        var proposal = JsonSerializer.Deserialize<AntigravityConfirmation>(JsonSerializer.Serialize(AntigravityProposal(), Formatting.Json), Formatting.Json)!;
        var badToken = JsonSerializer.Deserialize<AntigravityConfirmation>(JsonSerializer.Serialize(AntigravityProposal(), Formatting.Json), Formatting.Json)!; badToken.Token = "short";
        var otherHost = JsonSerializer.Deserialize<AntigravityConfirmation>(JsonSerializer.Serialize(AntigravityProposal(), Formatting.Json), Formatting.Json)!; otherHost.HostId = "mac";
        report.Checks["antigravity_confirmation_validated"] = proposal.IsValidFor("example-2") && !proposal.IsValidFor("example-1") && !badToken.IsValidFor("example-2") && !otherHost.IsValidFor("example-2") && !proposal.Expired;
        int calls = 0; bool tokenSent = false;
        var cancelled = AntigravitySwitchFlow.Run("example-2", token => { calls++; tokenSent |= token is not null; throw new AntigravityConfirmationRequiredException(proposal); }, _ => Task.FromResult(false)).GetAwaiter().GetResult();
        report.Checks["antigravity_cancel_never_posts_token"] = !cancelled && calls == 1 && !tokenSent;
        report.Checks["antigravity_errors_use_fixed_public_messages"] =
            DashboardClient.AntigravityError(HttpStatusCode.Conflict, "busy", "activation-running") == "Another Antigravity switch is already running. Wait for it to finish."
            && DashboardClient.AntigravityError(HttpStatusCode.Conflict, "stale-confirmation", null).StartsWith("The running Antigravity programs", StringComparison.Ordinal)
            && DashboardClient.AntigravityError(HttpStatusCode.BadRequest, "invalid-profile", null) == "The selected Antigravity account has no valid saved login."
            && DashboardClient.AntigravityError(HttpStatusCode.InternalServerError, "recovery-required", null).Contains("needs recovery", StringComparison.Ordinal)
            && !DashboardClient.AntigravityError(HttpStatusCode.Conflict, "FIXTURE_ONLY_PRIVATE", "token=fixture").Contains("FIXTURE", StringComparison.Ordinal);
        var dashboard = new AccountDashboard { Accounts = { new DashboardAccount { Provider = "codex", IsActive = true, Email = "codex-2@example.com", Capabilities = new AccountCapabilities { CodexProfile = "b" }, Windows = { new QuotaWindow { Key = "seven_day", Label = "Weekly", UsedPercent = 9.25, WindowMinutes = 10080 } } } } };
        var tooltip = Formatting.TrayTooltip(dashboard);
        report.Checks["tray_tooltip_shows_usage_percent"] = tooltip == "AI Account Center · Codex: codex-2, 90.75% weekly left" && Formatting.TrayTooltip(dashboard, stale: true).EndsWith("· last sample", StringComparison.Ordinal)
            && Formatting.TrayTooltip(null, configured: false) == "AI Account Center · not connected" && Formatting.TrayTooltip(new AccountDashboard()) == "AI Account Center"
            && Formatting.TrayTooltip(new AccountDashboard { Accounts = { new DashboardAccount { Provider = "codex", IsActive = true, Email = new string('x', 200) + "@example.com", Windows = { new QuotaWindow { Key = "seven_day", UsedPercent = 1 } } } } }).Length <= 127;
        report.Checks["value_easing_never_overshoots"] = Motion.NeverOvershoots(Motion.Out) && Motion.NeverOvershoots(Motion.InOut) && !Motion.NeverOvershoots(Motion.Spring);
        report.Checks["severity_ramp_matches_dashboard"] = Theme.Severity(79.99) == "calm" && Theme.Severity(80) == "warn" && Theme.Severity(95) == "crit" && Theme.Severity(100) == "crit" && Theme.Severity(100.01) == "over" && Theme.Severity(null) == "na" && Theme.Severity(double.NaN) == "na";
        var dot = System.Globalization.CultureInfo.CurrentCulture.NumberFormat.NumberDecimalSeparator;
        report.Checks["percent_is_one_run_two_decimals_max"] = Formatting.Percent(0.0886) == "0" + dot + "09%" && Formatting.Percent(9) == "9%" && Formatting.Percent(25.06101624978289) == "25" + dot + "06%" && Formatting.PercentWith(91, Formatting.Decimals(91)) == "91%";
        var saved = Formatting.Now;
        try
        {
            var now = DateTimeOffset.Parse("2026-10-01T15:00:00Z");
            Formatting.Now = () => now;
            report.Checks["reset_short_forms"] = Formatting.ResetShort("2026-10-08T05:00:00Z") == "6d 14h" && Formatting.ResetShort("2026-10-01T20:15:00Z") == Formatting.Clock(DateTimeOffset.Parse("2026-10-01T20:15:00Z")) && Formatting.ResetShort("2026-10-01T14:00:00Z") == "due" && Formatting.ResetShort(null) == ""
                && Formatting.Relative("2026-10-01T14:59:21Z") == "39s ago" && Formatting.Relative("2026-10-01T14:59:58Z") == "just now"
                && Formatting.ResetFallbacks("2026-10-01T20:15:00Z").SequenceEqual(new[] { "5h 15m", "5h" }) && Formatting.ResetFallbacks("2026-10-01T15:45:00Z").SequenceEqual(new[] { "45m" })
                && Formatting.ResetFallbacks("2026-10-08T05:00:00Z").SequenceEqual(new[] { "6d" }) && Formatting.ResetFallbacks("2026-10-01T14:00:00Z").Length == 0;
        }
        finally { Formatting.Now = saved; }
        var temporary = Path.Combine(Path.GetTempPath(), "aac-preferences-check-" + Guid.NewGuid().ToString("N") + ".json");
        try
        {
            new Preferences { Theme = "dark", Hotkey = false }.Save(temporary);
            var loaded = Preferences.Load(temporary);
            File.WriteAllText(temporary, "{\"theme\":\"neon\"}");
            report.Checks["preferences_round_trip_and_reject_unknown_theme"] = loaded.Mode == ThemeMode.Dark && !loaded.Hotkey && Preferences.Load(temporary).Mode == ThemeMode.Auto && Preferences.Load(temporary + ".missing").Hotkey;
        }
        finally { File.Delete(temporary); }
        bool icons = true;
        foreach (var data in Icons.Lucide.Values.Concat(Icons.Platform.Values.Select(item => item.Path)).Append(Icons.ApexInk).Append(Icons.ApexMeter))
        {
            try { icons &= !Icons.Geometry(data).Bounds.IsEmpty; } catch { icons = false; }
        }
        report.Checks["vector_icons_parse"] = icons;
        report.Checks["tabular_instrument_sans_embedded"] = FixtureRender.FontResolves();
        try
        {
            using var light = TrayIcon.LoadIcon(true, 16); using var dark = TrayIcon.LoadIcon(false, 32);
            report.Checks["apex_tray_icons_load_light_and_dark"] = light.Width == 16 && dark.Width == 32;
            // Light-taskbar art is dark ink and dark-taskbar art is light ink (mean luminance of the opaque pixels).
            static double Ink(bool lightTaskbar)
            {
                using var icon = TrayIcon.LoadIcon(lightTaskbar, 16); using var bitmap = icon.ToBitmap();
                double sum = 0, weight = 0;
                for (int y = 0; y < bitmap.Height; y++) for (int x = 0; x < bitmap.Width; x++)
                {
                    var c = bitmap.GetPixel(x, y); if (c.A < 128) continue;
                    sum += (0.2126 * c.R + 0.7152 * c.G + 0.0722 * c.B) / 255; weight++;
                }
                return weight == 0 ? double.NaN : sum / weight;
            }
            var lightInk = Ink(true); var darkInk = Ink(false);
            report.Measurements["tray_icon_ink_light_taskbar"] = Math.Round(lightInk, 3); report.Measurements["tray_icon_ink_dark_taskbar"] = Math.Round(darkInk, 3);
            report.Checks["tray_icon_ink_contrasts_with_taskbar"] = lightInk < 0.35 && darkInk > 0.65;
        }
        catch (Exception error) { report.Checks["apex_tray_icons_load_light_and_dark"] = false; report.Notes["apex_tray_icons"] = error.GetType().Name + ": " + error.Message; }
        try
        {
            // The plated app icon (exe, window, shortcuts): every Windows shell size, PNG frames.
            using var stream = TrayIcon.AppIconStream();
            using var reader = new BinaryReader(stream);
            reader.ReadUInt16(); var type = reader.ReadUInt16(); var count = reader.ReadUInt16();
            var sizes = new List<int>();
            for (int i = 0; i < count; i++) { var width = reader.ReadByte(); reader.ReadBytes(15); sizes.Add(width == 0 ? 256 : width); }
            report.Checks["app_icon_is_plated_apex_at_shell_sizes"] = type == 1 && new[] { 16, 20, 24, 32, 40, 48, 64, 256 }.All(sizes.Contains);
        }
        catch (Exception error) { report.Checks["app_icon_is_plated_apex_at_shell_sizes"] = false; report.Notes["app_icon"] = error.GetType().Name; }
        report.Checks["menu_font_is_instrument_sans"] = PrivateFonts.Family.Name == "Instrument Sans";
        var fixture = FixtureRender.LoadFixture(out _);
        report.Checks["fixture_is_sanitized"] = fixture.Accounts.Count > 0 && fixture.Accounts.All(account => account.Email is null || account.Email.EndsWith("@example.com", StringComparison.Ordinal));
    }

    /// <summary>F6: after a window's reset, a reading sampled before it is not shown (no number, no fill, never 0%).</summary>
    private static void ResetPendingChecks(CheckReport report)
    {
        var saved = Formatting.Now;
        try
        {
            var now = DateTimeOffset.Parse("2026-10-02T12:00:00Z", System.Globalization.CultureInfo.InvariantCulture);
            Formatting.Now = () => now;
            const string past = "2026-10-02T11:00:00Z", future = "2026-10-02T15:00:00Z";
            static QuotaWindow Window(string reset, string? sampled = null) => new() { Key = "five_hour", Label = "5h", Kind = "rate_limit", WindowMinutes = 300, UsedPercent = 37, ResetAt = reset, SampledAt = sampled };
            var older = new DashboardAccount { Provider = "claude", Plan = "max", SampledAt = "2026-10-02T10:00:00Z" };
            var newer = new DashboardAccount { Provider = "claude", Plan = "max", SampledAt = "2026-10-02T11:30:00Z" };
            var unknown = new DashboardAccount { Provider = "claude", Plan = "max" };
            var resetAt = DateTimeOffset.Parse(past, System.Globalization.CultureInfo.InvariantCulture);
            report.Checks["reset_passed_with_older_sample_is_pending"] = Formatting.PendingReset(older, Window(past)) == resetAt;
            report.Checks["reset_passed_with_newer_sample_shows_normally"] = Formatting.PendingReset(newer, Window(past)) is null;
            report.Checks["reset_in_future_shows_normally"] = Formatting.PendingReset(older, Window(future)) is null && Formatting.PendingReset(unknown, Window(future)) is null;
            report.Checks["reset_passed_with_unknown_sample_is_pending"] = Formatting.PendingReset(unknown, Window(past)) == resetAt && Formatting.PendingReset(unknown, Window(past, "not-a-time")) == resetAt;
            report.Checks["reset_pending_reads_window_sample_before_account_sample"] = Formatting.PendingReset(older, Window(past, "2026-10-02T11:45:00Z")) is null && Formatting.PendingReset(newer, Window(past, "2026-10-02T10:30:00Z")) == resetAt;
            report.Checks["reset_pending_leaves_amounts_unlimited_and_disabled"] = Formatting.PendingReset(older, new QuotaWindow { Key = "credits", Kind = "balance", Remaining = 4, ResetAt = past }) is null
                && Formatting.PendingReset(older, new QuotaWindow { Key = "x", Unlimited = true, ResetAt = past }) is null && Formatting.PendingReset(older, new QuotaWindow { Key = "y", Enabled = false, ResetAt = past }) is null;
            var forms = Formatting.PendingForms(resetAt);
            report.Checks["reset_pending_text_names_the_reset_and_no_number"] = forms[0] == Formatting.ResetAt(resetAt) + " · new reading pending" && forms[0].StartsWith("Reset at ", StringComparison.Ordinal)
                && forms.All(form => !form.Contains('%')) && Formatting.ResetPendingLong(resetAt).EndsWith(" · new reading pending", StringComparison.Ordinal);
            report.Notes["reset_pending_text"] = forms[0];
            var codex = new AccountDashboard { Accounts = { new DashboardAccount { Provider = "codex", IsActive = true, Email = "codex-2@example.com", SampledAt = "2026-10-02T10:00:00Z", Windows = { new QuotaWindow { Key = "seven_day", Label = "Weekly", UsedPercent = 9.25, WindowMinutes = 10080, ResetAt = past } } } } };
            var pendingTip = Formatting.TrayTooltip(codex);
            codex.Accounts[0].SampledAt = "2026-10-02T11:30:00Z";
            report.Checks["tray_tooltip_hides_weekly_percent_after_its_reset"] = pendingTip == "AI Account Center · Codex: codex-2, weekly reset, new reading pending" && Formatting.TrayTooltip(codex) == "AI Account Center · Codex: codex-2, 90.75% weekly left";
        }
        finally { Formatting.Now = saved; }
    }

    /// <summary>
    /// Sign-in and Change verify before they save. Every case runs the tray's own Change path
    /// (<see cref="MainWindow.SubmitConnection"/>) against loopback fixture servers, with the connection stored in an
    /// isolated temporary folder, never the tray's real store. Each failure must leave the stored file's bytes and
    /// the running client exactly as they were; success must replace both, through the same DPAPI writer.
    /// </summary>
    private static async Task SignInChangeChecks(CheckReport report)
    {
        var folder = Path.Combine(Path.GetTempPath(), "aac-signin-check-" + Guid.NewGuid().ToString("N"));
        var store = Path.Combine(folder, "connection.dpapi");
        var firstRunStore = Path.Combine(folder, "first-run", "connection.dpapi");
        var real = Path.GetFullPath(SecureStore.StateDirectory).TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar;
        var isolated = !Path.GetFullPath(store).StartsWith(real, StringComparison.OrdinalIgnoreCase) && !Path.GetFullPath(firstRunStore).StartsWith(real, StringComparison.OrdinalIgnoreCase)
            && !string.Equals(Path.GetFullPath(store), Path.GetFullPath(SecureStore.SettingsPath), StringComparison.OrdinalIgnoreCase);
        report.Checks["sign_in_checks_use_an_isolated_store"] = isolated;
        if (!isolated) return; // never risk the tray's real connection
        using var fixture = new SignInFixture();
        using var elsewhere = new NotDashboardFixture();
        MainWindow? window = null, first = null, third = null;
        try
        {
            Directory.CreateDirectory(folder);
            SecureStore.Save(new ConnectionSettings { BaseURL = fixture.Origin, Username = "fixture", Password = "fixture-old" }, store);
            var panel = window = new MainWindow(new Preferences { Theme = "light", Hotkey = false }, loadConnection: false);
            panel.UseConnectionStoreForCheck(store);
            var bytes = File.ReadAllBytes(store);
            var liveClient = panel.ClientForCheck; var liveConnection = panel.ConnectionForCheck;
            bool Unchanged() => liveClient is not null && File.ReadAllBytes(store).AsSpan().SequenceEqual(bytes) && ReferenceEquals(panel.ClientForCheck, liveClient)
                && ReferenceEquals(panel.ConnectionForCheck, liveConnection) && !Directory.EnumerateFiles(folder, "*.tmp-*", SearchOption.AllDirectories).Any() && !panel.CheckingConnection;

            var refused = await panel.SubmitConnection(ClosedLoopbackOrigin(), "fixture", "fixture-new");
            report.Notes["sign_in_wrong_address_message"] = refused ?? "";
            report.Checks["sign_in_wrong_address_keeps_saved_connection_and_client"] = refused == "Could not reach a dashboard at that address. The saved connection was not changed." && Unchanged();
            var wrongServer = await panel.SubmitConnection(elsewhere.Origin, "fixture", "fixture-new");
            report.Checks["sign_in_address_that_is_not_the_dashboard_keeps_saved_connection"] = wrongServer == "That address answered, but not as an AI Account Center dashboard. The saved connection was not changed." && Unchanged();

            var rejected = await panel.SubmitConnection(fixture.Origin, "fixture", "wrong-password");
            report.Notes["sign_in_wrong_login_message"] = rejected ?? "";
            report.Checks["sign_in_wrong_login_keeps_saved_connection_and_client"] = rejected == "The dashboard did not accept that username and password. The saved connection was not changed." && Unchanged() && fixture.SettingsReads == 0;

            // Cancel while the server is still answering the login: the same call the Cancel button and Escape make.
            fixture.ArmSlowLogin();
            var slow = panel.SubmitConnection(fixture.Origin, "fixture-slow", "fixture-new");
            var arrived = await Task.WhenAny(fixture.SlowLoginArrived, Task.Delay(10000)) == fixture.SlowLoginArrived;
            var checking = panel.CheckingConnection;
            panel.CancelConnectionCheck();
            var cancelled = await slow;
            report.Notes["sign_in_cancel_message"] = cancelled ?? "";
            report.Checks["sign_in_cancel_mid_check_keeps_saved_connection_and_client"] = arrived && checking && cancelled == "Connection check cancelled. The saved connection was not changed." && Unchanged();

            fixture.ArmSlowLogin();
            panel.ConnectionCheckTimeout = TimeSpan.FromMilliseconds(500);
            var late = await panel.SubmitConnection(fixture.Origin, "fixture-slow", "fixture-new");
            panel.ConnectionCheckTimeout = TimeSpan.FromSeconds(15);
            report.Checks["sign_in_timeout_keeps_saved_connection_and_client"] = late == "The dashboard took too long to answer. The saved connection was not changed." && Unchanged();

            var invalid = await panel.SubmitConnection("http://127.0.0.1:3000/account", "fixture", "fixture-new");
            report.Checks["sign_in_invalid_address_never_contacts_or_saves"] = invalid is not null && Unchanged();

            var before = fixture.Requests.Count;
            var ok = await panel.SubmitConnection(fixture.Origin, " fixture ", "fixture-new");
            var saved = SecureStore.Load(store);
            var sequence = fixture.Requests.Skip(before).ToArray();
            report.Checks["sign_in_success_logs_in_then_reads_with_the_session"] = sequence.SequenceEqual(new[] { "POST /api/auth/login 200", "GET /api/accounts/settings 200" });
            report.Checks["sign_in_success_replaces_saved_connection_and_client"] = ok is null && !File.ReadAllBytes(store).AsSpan().SequenceEqual(bytes)
                && saved is { Username: "fixture", Password: "fixture-new" } && saved.BaseURL == fixture.Origin
                && panel.ClientForCheck is { } replaced && !ReferenceEquals(replaced, liveClient) && replaced.BaseURL == new Uri(fixture.Origin + "/")
                && panel.ConnectionForCheck is { Password: "fixture-new" } && !Directory.EnumerateFiles(folder, "*.tmp-*", SearchOption.AllDirectories).Any();
            report.Checks["sign_in_success_keeps_the_dpapi_json_shape"] = SecureStore.StoredKeysForCheck(store).SequenceEqual(new[] { "baseURL", "username", "password" });

            var verifiedClient = panel.ClientForCheck;
            var blank = await panel.SubmitConnection(fixture.Origin, "fixture", "");
            report.Checks["sign_in_blank_password_rechecks_the_saved_password"] = blank is null && SecureStore.Load(store)?.Password == "fixture-new" && !ReferenceEquals(panel.ClientForCheck, verifiedClient);

            // First run: nothing stored yet. A failure saves nothing and leaves no client; success saves and connects.
            var fresh = first = new MainWindow(new Preferences { Theme = "light", Hotkey = false }, loadConnection: false);
            fresh.UseConnectionStoreForCheck(firstRunStore);
            var firstRejected = await fresh.SubmitConnection(fixture.Origin, "fixture", "wrong-password");
            var firstRefused = await fresh.SubmitConnection(ClosedLoopbackOrigin(), "fixture", "fixture-new");
            report.Checks["first_run_failure_saves_nothing"] = firstRejected == "The dashboard did not accept that username and password. Nothing was saved."
                && firstRefused == "Could not reach a dashboard at that address. Nothing was saved."
                && !File.Exists(firstRunStore) && fresh.ClientForCheck is null && fresh.ConnectionForCheck is null;
            var firstOk = await fresh.SubmitConnection(fixture.Origin, "fixture", "fixture-new");
            report.Checks["first_run_success_saves_the_verified_connection"] = firstOk is null && SecureStore.Load(firstRunStore) is { Password: "fixture-new" } && fresh.ClientForCheck is not null;
            // Other stored members (a device token from a later pairing) survive a verified Change exactly.
            var withToken = Path.Combine(folder, "with-token", "connection.dpapi");
            using (var token = JsonDocument.Parse("{\"deviceToken\":\"fixture-device-token\",\"pairedAt\":1}"))
                SecureStore.Save(new ConnectionSettings { BaseURL = fixture.Origin, Username = "fixture", Password = "fixture-old", Extra = token.RootElement.EnumerateObject().ToDictionary(item => item.Name, item => item.Value.Clone()) }, withToken);
            var paired = third = new MainWindow(new Preferences { Theme = "light", Hotkey = false }, loadConnection: false);
            paired.UseConnectionStoreForCheck(withToken);
            var pairedFailed = await paired.SubmitConnection(fixture.Origin, "fixture", "wrong-password");
            var pairedOk = await paired.SubmitConnection(fixture.Origin, "fixture", "fixture-new");
            var reloaded = SecureStore.Load(withToken);
            report.Checks["sign_in_change_keeps_other_stored_members_exactly"] = pairedFailed is not null && pairedOk is null && reloaded is { Password: "fixture-new" }
                && reloaded.Extra?["deviceToken"].GetString() == "fixture-device-token" && reloaded.Extra?["pairedAt"].GetInt32() == 1
                && SecureStore.StoredKeysForCheck(withToken).OrderBy(key => key, StringComparer.Ordinal).SequenceEqual(new[] { "baseURL", "deviceToken", "pairedAt", "password", "username" });
            report.Checks["sign_in_requests_reach_only_the_loopback_fixtures"] = fixture.Requests.Count > 0 && fixture.Unexpected == 0;
        }
        catch (Exception error)
        {
            report.Checks["sign_in_change_checks_completed"] = false;
            report.Notes["sign_in_change_checks"] = error.GetType().Name + ": " + error.Message;
        }
        finally
        {
            foreach (var shown in new[] { window, first, third }) if (shown is not null) { shown.AllowClose = true; shown.Close(); }
            try { Directory.Delete(folder, true); } catch { }
        }
    }

    private static string ClosedLoopbackOrigin()
    {
        var socket = new TcpListener(IPAddress.Loopback, 0); socket.Start();
        var port = ((IPEndPoint)socket.LocalEndpoint).Port; socket.Stop();
        return $"http://127.0.0.1:{port}";
    }

    /// <summary>A loopback stand-in for the dashboard's sign-in: POST /api/auth/login accepts only fixture/fixture-new
    /// and sets an HttpOnly session cookie; GET /api/accounts/settings needs that cookie. "fixture-slow" logins are
    /// held open until the check cancels or times out.</summary>
    private sealed class SignInFixture : IDisposable
    {
        private readonly HttpListener listener = new();
        private readonly CancellationTokenSource stop = new();
        private TaskCompletionSource slowArrived = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public string Origin { get; }
        public List<string> Requests { get; } = new();
        public int SettingsReads, Unexpected;
        public Task SlowLoginArrived => slowArrived.Task;

        public SignInFixture()
        {
            var socket = new TcpListener(IPAddress.Loopback, 0); socket.Start();
            var port = ((IPEndPoint)socket.LocalEndpoint).Port; socket.Stop();
            Origin = $"http://127.0.0.1:{port}";
            listener.Prefixes.Add(Origin + "/"); listener.Start();
            _ = Task.Run(Loop);
        }

        public void ArmSlowLogin() => slowArrived = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);

        private async Task Loop()
        {
            while (!stop.IsCancellationRequested)
            {
                HttpListenerContext context;
                try { context = await listener.GetContextAsync(); } catch { return; }
                _ = Task.Run(() => Handle(context));
            }
        }

        private async Task Handle(HttpListenerContext context)
        {
            try
            {
                var request = context.Request; var path = request.Url!.AbsolutePath;
                int status; object payload;
                if (request.HttpMethod == "POST" && path == "/api/auth/login")
                {
                    using var reader = new StreamReader(request.InputStream);
                    using var json = JsonDocument.Parse(await reader.ReadToEndAsync());
                    var username = json.RootElement.GetProperty("username").GetString();
                    var password = json.RootElement.GetProperty("password").GetString();
                    if (username == "fixture-slow") { slowArrived.TrySetResult(); await Task.Delay(Timeout.Infinite, stop.Token); return; }
                    if (username == "fixture" && password == "fixture-new")
                    {
                        context.Response.SetCookie(new Cookie("fixture-session", "yes", "/") { HttpOnly = true });
                        status = 200; payload = new { success = true, username };
                    }
                    else { status = 401; payload = new { error = "Invalid credentials" }; }
                }
                else if (request.HttpMethod == "GET" && path == "/api/accounts/settings")
                {
                    Interlocked.Increment(ref SettingsReads);
                    if (request.Cookies["fixture-session"]?.Value == "yes") { status = 200; payload = new { refreshIntervalSeconds = 60 }; }
                    else { status = 401; payload = new { error = "Authentication required" }; }
                }
                else { Interlocked.Increment(ref Unexpected); status = 404; payload = new { error = "Not found" }; }
                lock (Requests) Requests.Add($"{request.HttpMethod} {path} {status}");
                var bytes = JsonSerializer.SerializeToUtf8Bytes(payload, Formatting.Json);
                context.Response.StatusCode = status; context.Response.ContentType = "application/json"; context.Response.ContentLength64 = bytes.Length;
                await context.Response.OutputStream.WriteAsync(bytes); context.Response.Close();
            }
            catch { try { context.Response.Abort(); } catch { } }
        }

        public void Dispose() { stop.Cancel(); try { listener.Stop(); listener.Close(); } catch { } }
    }

    /// <summary>A loopback server that is not the dashboard: every path is a 404 web page.</summary>
    private sealed class NotDashboardFixture : IDisposable
    {
        private readonly HttpListener listener = new();
        public string Origin { get; }
        public NotDashboardFixture()
        {
            var socket = new TcpListener(IPAddress.Loopback, 0); socket.Start();
            var port = ((IPEndPoint)socket.LocalEndpoint).Port; socket.Stop();
            Origin = $"http://127.0.0.1:{port}";
            listener.Prefixes.Add(Origin + "/"); listener.Start();
            _ = Task.Run(async () =>
            {
                while (true)
                {
                    HttpListenerContext context;
                    try { context = await listener.GetContextAsync(); } catch { return; }
                    try
                    {
                        var bytes = Encoding.UTF8.GetBytes("<html><body>Not found</body></html>");
                        context.Response.StatusCode = 404; context.Response.ContentType = "text/html"; context.Response.ContentLength64 = bytes.Length;
                        await context.Response.OutputStream.WriteAsync(bytes); context.Response.Close();
                    }
                    catch { }
                }
            });
        }
        public void Dispose() { try { listener.Stop(); listener.Close(); } catch { } }
    }

    private static CodexSwitchConfirmation Proposal() => new()
    {
        Token = "fixture-confirmation-token", ExpiresAt = DateTimeOffset.UtcNow.AddMinutes(1).ToString("O"), TargetProfile = "party",
        Processes = new List<CodexSwitchProcess> { new() { Label = "Fixture Codex desktop", Pid = 1234, Role = "desktop" } },
        Warning = "Fixture only. Active work may be interrupted."
    };

    private static async Task PublicErrorChecks(CheckReport report)
    {
        const string canary = "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile";
        // Reviewed golden table SHA256947f5a63a2c83fcd35296ec3be8ee195328b2852aeb29f8b16eba5965407fd86.
        var cases = new (int Status, DashboardRequestKind Kind, string? Code, string? Reason, string BodyKind, string Expected)[]
        {
            (400, DashboardRequestKind.Usage, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard rejected this request. Refresh and try again."),
            (400, DashboardRequestKind.ClaudeOpen, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard rejected this request. Refresh and try again."),
            (400, DashboardRequestKind.AutoSwitch, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The automatic switching settings were rejected. Refresh and try again."),
            (401, DashboardRequestKind.Usage, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "Dashboard sign-in needs attention. Check Settings."),
            (401, DashboardRequestKind.ClaudeOpen, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "Dashboard sign-in needs attention. Check Settings."),
            (401, DashboardRequestKind.AutoSwitch, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "Dashboard sign-in needs attention. Check Settings."),
            (403, DashboardRequestKind.Usage, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard rejected this action. Check the connection origin."),
            (403, DashboardRequestKind.ClaudeOpen, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard rejected this action. Check the connection origin."),
            (403, DashboardRequestKind.AutoSwitch, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard rejected this action. Check the connection origin."),
            (404, DashboardRequestKind.Usage, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The requested account action is not available on this server."),
            (404, DashboardRequestKind.ClaudeOpen, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The selected Claude profile is not available on that computer."),
            (404, DashboardRequestKind.AutoSwitch, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The requested account action is not available on this server."),
            (408, DashboardRequestKind.Usage, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard took too long to respond. Try Refresh."),
            (408, DashboardRequestKind.ClaudeOpen, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard took too long to respond. Try Refresh."),
            (408, DashboardRequestKind.AutoSwitch, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard took too long to respond. Try Refresh."),
            (409, DashboardRequestKind.Usage, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "Another account action is running. Try again when it finishes."),
            (409, DashboardRequestKind.ClaudeOpen, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "Another account action is running. Try again when it finishes."),
            (409, DashboardRequestKind.AutoSwitch, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "Another account action is running. Try again when it finishes."),
            (415, DashboardRequestKind.Usage, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard rejected this request. Refresh and try again."),
            (415, DashboardRequestKind.ClaudeOpen, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard rejected this request. Refresh and try again."),
            (415, DashboardRequestKind.AutoSwitch, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The automatic switching settings were rejected. Refresh and try again."),
            (422, DashboardRequestKind.Usage, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard rejected this request. Refresh and try again."),
            (422, DashboardRequestKind.ClaudeOpen, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard rejected this request. Refresh and try again."),
            (422, DashboardRequestKind.AutoSwitch, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The automatic switching settings were rejected. Refresh and try again."),
            (429, DashboardRequestKind.Usage, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard is limiting requests. Wait before trying again."),
            (429, DashboardRequestKind.ClaudeOpen, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard is limiting requests. Wait before trying again."),
            (429, DashboardRequestKind.AutoSwitch, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard is limiting requests. Wait before trying again."),
            (500, DashboardRequestKind.Usage, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "Usage could not be refreshed. Try Refresh."),
            (500, DashboardRequestKind.ClaudeOpen, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard could not complete this request. Try again later."),
            (500, DashboardRequestKind.AutoSwitch, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard could not complete this request. Try again later."),
            (502, DashboardRequestKind.Usage, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The requested account service is unavailable. Try Refresh."),
            (502, DashboardRequestKind.ClaudeOpen, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "Claude could not be opened on the selected computer. Check its profile setup and connection."),
            (502, DashboardRequestKind.AutoSwitch, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The requested account service is unavailable. Try Refresh."),
            (503, DashboardRequestKind.Usage, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The requested account service is unavailable. Try Refresh."),
            (503, DashboardRequestKind.ClaudeOpen, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "Claude could not be opened on the selected computer. Check its profile setup and connection."),
            (503, DashboardRequestKind.AutoSwitch, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The requested account service is unavailable. Try Refresh."),
            (504, DashboardRequestKind.Usage, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard took too long to respond. Try Refresh."),
            (504, DashboardRequestKind.ClaudeOpen, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard took too long to respond. Try Refresh."),
            (504, DashboardRequestKind.AutoSwitch, "unknown-fixture-code", "FIXTURE_ONLY_PRIVATE_ERROR_CANARY token=fixture-token path=/fixture/private/profile", "json", "The dashboard took too long to respond. Try Refresh."),
            (409, DashboardRequestKind.CodexActivation, "busy", "activation_running", "json", "Another Codex account activation is already running. Wait for it to finish."),
            (409, DashboardRequestKind.CodexActivation, "busy", "unsupported_process", "json", "A running Codex program cannot be restarted safely. Close it and try again."),
            (409, DashboardRequestKind.CodexActivation, "busy", "running_processes", "json", "Codex is busy. Try switching after its work finishes."),
            (409, DashboardRequestKind.CodexActivation, "confirmation_stale", null, "json", "The running Codex programs or account changed. Activate again to review a new warning."),
            (400, DashboardRequestKind.CodexActivation, "invalid_profile", null, "json", "The selected profile has no valid saved login."),
            (400, DashboardRequestKind.CodexActivation, "invalid_codex_home", null, "json", "Account activation needs the shared Codex configuration."),
            (500, DashboardRequestKind.CodexActivation, "restart_failed", null, "json", "Codex could not restart. Check its processes before retrying activation."),
            (500, DashboardRequestKind.CodexActivation, "verification_failed", null, "json", "The activated account could not be verified. Refresh accounts before retrying."),
            (500, DashboardRequestKind.CodexActivation, "auth_read_failed", null, "json", "The saved Codex login could not be read safely."),
            (500, DashboardRequestKind.CodexActivation, "auth_write_failed", null, "json", "The Codex login could not be installed safely."),
            (400, DashboardRequestKind.Usage, null, null, "malformed", "The dashboard rejected this request. Refresh and try again."),
            (502, DashboardRequestKind.Usage, null, null, "html", "The requested account service is unavailable. Try Refresh."),
            (500, DashboardRequestKind.Usage, null, null, "large", "Usage could not be refreshed. Try Refresh."),
        };
        bool allFixed = true;
        foreach (var example in cases)
        {
            var body = example.BodyKind switch
            {
                "malformed" => "{broken" + canary,
                "html" => "<html>" + canary + "</html>",
                "large" => JsonSerializer.Serialize(new { error = string.Concat(Enumerable.Repeat(canary, 100)) }),
                _ => JsonSerializer.Serialize(new { error = canary, message = canary, code = example.Code, reason = example.Reason })
            };
            using var response = new System.Net.Http.HttpResponseMessage((HttpStatusCode)example.Status) { Content = new System.Net.Http.StringContent(body) };
            try { await DashboardClient.Decode<JsonElement>(response, example.Kind == DashboardRequestKind.CodexActivation ? "party" : null, example.Kind); allFixed = false; }
            catch (InvalidOperationException error) { allFixed &= error.Message == example.Expected && !error.Message.Contains("FIXTURE_ONLY") && !error.Message.Contains("fixture-token") && !error.Message.Contains("/fixture/private/profile"); }
        }
        report.Checks["all_52_server_error_canaries_use_fixed_public_messages"] = allFixed;
        bool scoped = true;
        foreach (var example in new[] { (Status: HttpStatusCode.BadRequest, Kind: DashboardRequestKind.Usage), (Status: HttpStatusCode.Conflict, Kind: DashboardRequestKind.AutoSwitch), (Status: HttpStatusCode.BadGateway, Kind: DashboardRequestKind.CodexActivation) })
        {
            using var response = new System.Net.Http.HttpResponseMessage(example.Status) { Content = new System.Net.Http.StringContent(JsonSerializer.Serialize(new { code = "invalid_profile", reason = "activation_running", error = canary })) };
            try { await DashboardClient.Decode<JsonElement>(response, example.Kind == DashboardRequestKind.CodexActivation ? "party" : null, example.Kind); scoped = false; }
            catch (InvalidOperationException error) { scoped &= error.Message != "The selected profile has no valid saved login." && !error.Message.Contains(canary); }
        }
        report.Checks["codex_public_codes_apply_only_to_approved_context_and_status"] = scoped;
    }

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
