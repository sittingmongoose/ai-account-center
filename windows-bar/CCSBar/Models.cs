using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Net;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace CCSBar;

public sealed class ConnectionSettings
{
    public string BaseURL { get; set; } = "http://192.168.50.179:3000";
    public string Username { get; set; } = "";
    public string Password { get; set; } = "";

    public Uri Validate()
    {
        if (!Uri.TryCreate(BaseURL.Trim(), UriKind.Absolute, out var uri) ||
            uri.Scheme is not ("http" or "https") ||
            !string.IsNullOrEmpty(uri.UserInfo) || uri.AbsolutePath != "/" ||
            !string.IsNullOrEmpty(uri.Query) || !string.IsNullOrEmpty(uri.Fragment))
            throw new ArgumentException("Enter the dashboard's http or https origin, without a path.");
        if (uri.Scheme == "http" && !IsPrivateHost(uri.Host))
            throw new ArgumentException("HTTP is supported for local network dashboards. Use HTTPS for other servers.");
        if (string.IsNullOrWhiteSpace(Username) || string.IsNullOrEmpty(Password))
            throw new ArgumentException("Enter your dashboard username and password.");
        return uri;
    }

    private static bool IsPrivateHost(string host)
    {
        if (host.Equals("localhost", StringComparison.OrdinalIgnoreCase)) return true;
        if (!IPAddress.TryParse(host, out var address)) return false;
        if (IPAddress.IsLoopback(address)) return true;
        if (address.AddressFamily == System.Net.Sockets.AddressFamily.InterNetworkV6)
            return address.IsIPv6LinkLocal || (address.GetAddressBytes()[0] & 0xfe) == 0xfc;
        var bytes = address.GetAddressBytes();
        return bytes[0] == 10 || (bytes[0] == 172 && bytes[1] is >= 16 and <= 31) ||
               (bytes[0] == 192 && bytes[1] == 168) || (bytes[0] == 169 && bytes[1] == 254);
    }
}

public sealed class AccountDashboard
{
    public int SchemaVersion { get; set; }
    public string UpdatedAt { get; set; } = "";
    public List<DashboardAccount> Accounts { get; set; } = new();
    public AutoSwitchStatus CodexAutoSwitch { get; set; } = new();
    public AccountRefreshSettings? Settings { get; set; }
}

public sealed class AccountRefreshSettings
{
    public int RefreshIntervalSeconds { get; set; } = 60;
    public int ValidatedInterval => RefreshIntervalSeconds is >= 30 and <= 3600 ? RefreshIntervalSeconds : 60;
}

public sealed class DashboardAccount
{
    public string Id { get; set; } = "";
    public string Provider { get; set; } = "";
    public string ProviderLabel { get; set; } = "";
    public string Label { get; set; } = "";
    public string? Email { get; set; }
    public string? Plan { get; set; }
    public string Platform { get; set; } = "";
    public string Source { get; set; } = "";
    public string Status { get; set; } = "unavailable";
    public string? Message { get; set; }
    public string? FetchedAt { get; set; }
    public string? SampledAt { get; set; }
    public bool IsActive { get; set; }
    public List<QuotaWindow> Windows { get; set; } = new();
    public AccountCapabilities Capabilities { get; set; } = new();
    [JsonIgnore] public bool HasUsableUsage => Windows.Exists(window => window.HasUsableUsage);
}

public sealed class QuotaWindow
{
    public string Key { get; set; } = "";
    public string Label { get; set; } = "";
    public double? UsedPercent { get; set; }
    public double? RemainingPercent { get; set; }
    public string? ResetAt { get; set; }
    public double? WindowMinutes { get; set; }
    public double? Used { get; set; }
    public double? Limit { get; set; }
    public string? Unit { get; set; }
    public string? Kind { get; set; }
    public string? Status { get; set; }
    public string? SampledAt { get; set; }
    public double? Remaining { get; set; }
    public string? ExpiresAt { get; set; }
    public bool Unlimited { get; set; }
    public bool? Enabled { get; set; }
    [JsonIgnore] public double? DisplayPercent => UsedPercent is double used
        ? double.IsFinite(used) && used >= 0 ? used : null
        : RemainingPercent is >= 0 and <= 100 ? 100 - RemainingPercent : null;
    [JsonIgnore] public bool HasUsableUsage => DisplayPercent is not null || Unlimited || Enabled == false
        || UsableAmount(Used) || UsableAmount(Limit) || UsableAmount(Remaining);
    private static bool UsableAmount(double? amount) => amount is double value && double.IsFinite(value) && value >= 0;
}

public sealed class AccountCapabilities
{
    public string? CodexProfile { get; set; }
    public string? ClaudeProfileId { get; set; }
    public List<string> ClaudePlatforms { get; set; } = new();
}

public sealed class AutoSwitchStatus
{
    public bool Enabled { get; set; }
    public double ThresholdPercent { get; set; } = 5;
    public int PollIntervalSeconds { get; set; } = 60;
    public string Outcome { get; set; } = "";
    public string Message { get; set; } = "";
    public bool ActivationInProgress { get; set; }
    public string? LastCheckedAt { get; set; }
    public string? LastSwitchedAt { get; set; }
}

public static class Formatting
{
    public static readonly JsonSerializerOptions Json = new() { PropertyNameCaseInsensitive = true, PropertyNamingPolicy = JsonNamingPolicy.CamelCase };

    public static string Reset(string? timestamp, DateTimeOffset? now = null)
    {
        if (!DateTimeOffset.TryParse(timestamp, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var reset))
            return "Reset time unavailable";
        var remaining = reset - (now ?? DateTimeOffset.UtcNow);
        var local = reset.ToLocalTime();
        var at = local.ToString(local.Year == DateTime.Now.Year ? "ddd, MMM d h:mm tt" : "ddd, MMM d, yyyy h:mm tt", CultureInfo.CurrentCulture);
        if (remaining <= TimeSpan.Zero) return $"Reset {at} · awaiting update";
        var duration = remaining.TotalDays >= 1 ? $"{(int)remaining.TotalDays}d {remaining.Hours}h" :
            remaining.TotalHours >= 1 ? $"{(int)remaining.TotalHours}h {remaining.Minutes}m" : $"{Math.Max(1, (int)Math.Ceiling(remaining.TotalMinutes))}m";
        return $"Resets {at} · {duration}";
    }

    public static string ShortReset(string? timestamp)
    {
        if (!DateTimeOffset.TryParse(timestamp, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var reset))
            return "Reset unavailable";
        var local = reset.ToLocalTime();
        var at = local.ToString(local.Date == DateTime.Now.Date ? "H:mm" : local.Year == DateTime.Now.Year ? "MMM d H:mm" : "MMM d, yyyy H:mm", CultureInfo.CurrentCulture);
        return (reset <= DateTimeOffset.UtcNow ? "Reset " : "Resets ") + at;
    }

    public static string Expiration(string? timestamp)
    {
        return DateTimeOffset.TryParse(timestamp, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var date)
            ? "Expires " + date.ToLocalTime().ToString("MMM d, yyyy h:mm tt", CultureInfo.CurrentCulture)
            : "Expiration date unavailable";
    }

    public static string Amount(double amount, string? unit)
    {
        // Preserve reported fractions; do not label a currency or unit that the API omitted.
        return amount.ToString("N2", CultureInfo.CurrentCulture).TrimEnd('0').TrimEnd(CultureInfo.CurrentCulture.NumberFormat.NumberDecimalSeparator.ToCharArray()) +
            (string.IsNullOrWhiteSpace(unit) ? "" : " " + unit);
    }

    public static QuotaWindow[] CodexPrimaryWindows(DashboardAccount account)
    {
        // Core windows have exact canonical keys. An additional quota with the
        // same duration is still additional and remains available in Details.
        var visible = VisibleWindows(account);
        return new[] { "five_hour", "seven_day" }
            .Select(key => visible.FirstOrDefault(window => window.Key == key))
            .Where(window => window is not null).Select(window => window!).ToArray();
    }

    public static bool IsChatPass(string? text) => Normalize(text).Contains("chatpass", StringComparison.Ordinal);

    public static QuotaWindow[] VisibleWindows(DashboardAccount account) => account.Windows.Where(window =>
    {
        var key = Normalize(window.Key); var label = Normalize(window.Label);
        if (account.Provider == "claude" && window.Key == "seven_day_fable"
            && (!(account.Plan ?? "").StartsWith("max", StringComparison.OrdinalIgnoreCase) || window.DisplayPercent is null)) return false;
        if (account.Provider == "codex")
        {
            if (IsChatPass(window.Key) || IsChatPass(window.Label)) return false;
            bool fiveHour = window.WindowMinutes == 300 || key is "fivehour" or "5h" or "fivehours";
            bool proProfile = account.Capabilities.CodexProfile is "gmail" or "party";
            if (fiveHour && proProfile && !window.HasUsableUsage && !DateTimeOffset.TryParse(window.ResetAt, out _)) return false;
        }
        if (account.Provider == "qwen" && (key is "subscription" or "plansubscription" || label == "plansubscription")) return false;
        if (account.Provider == "zai" && (key.Contains("pack", StringComparison.Ordinal) || label.Contains("pack", StringComparison.Ordinal)))
        {
            bool positive = Positive(window.Remaining) || Positive(window.Used) || Positive(window.Limit);
            bool individual = DateTimeOffset.TryParse(window.ExpiresAt, out _)
                || (key.Contains("grant", StringComparison.Ordinal) || key.Contains("record", StringComparison.Ordinal)) && window.HasUsableUsage;
            if (!positive && !individual) return false;
        }
        return true;
    }).ToArray();

    public static string WindowLabel(DashboardAccount account, QuotaWindow window)
    {
        if (account.Provider != "qwen") return window.Label;
        return Normalize(window.Key) switch
        {
            "monthly" => "Monthly", "weekly" => "Weekly", "fivehour" or "5h" => "5-hour usage", _ => window.Label
        };
    }

    public static bool IsQwenDuplicateMetadata(QuotaWindow window) => Normalize(window.Key) is "creditsremaining" or "remainingcredits" or "resetdate";
    public static QuotaWindow[] QwenPacks(DashboardAccount account) => account.Provider == "qwen"
        ? VisibleWindows(account).Where(window => window.Key.StartsWith("addon-pack-", StringComparison.Ordinal)).ToArray()
        : Array.Empty<QuotaWindow>();

    public static string PackStatus(QuotaWindow pack, DateTimeOffset? now = null)
    {
        if (pack.Enabled == false) return "Disabled";
        if (DateTimeOffset.TryParse(pack.ExpiresAt, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var expiry)
            && expiry <= (now ?? DateTimeOffset.UtcNow)) return "Expired";
        if (pack.Unlimited) return "Unlimited";
        if (pack.Remaining == 0 || pack.DisplayPercent is >= 100) return "Depleted";
        if (Positive(pack.Remaining)) return "Available";
        return "Status unavailable";
    }
    private static bool Positive(double? value) => value is double number && double.IsFinite(number) && number > 0;
    private static string Normalize(string? text) => new((text ?? "").Where(char.IsLetterOrDigit).Select(char.ToLowerInvariant).ToArray());

    public static string Sampled(string? timestamp)
    {
        if (!DateTimeOffset.TryParse(timestamp, out var date)) return "No usage sample";
        var local = date.ToLocalTime();
        return "Updated " + local.ToString(local.Date == DateTime.Now.Date ? "h:mm tt" : "MMM d, h:mm tt", CultureInfo.CurrentCulture);
    }

    public static string WindowSample(string? timestamp) => DateTimeOffset.TryParse(timestamp, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var sample)
        ? "Sampled " + sample.ToLocalTime().ToString("MMM d, yyyy h:mm:ss tt zzz", CultureInfo.CurrentCulture)
        : "Sample time unavailable";

    public static bool IsSafeProfile(string? name)
    {
        if (string.IsNullOrEmpty(name) || name.Length > 64 || !char.IsAsciiLetterOrDigit(name[0])) return false;
        foreach (var c in name) if (!char.IsAsciiLetterOrDigit(c) && c != '_' && c != '-') return false;
        return true;
    }

    public static bool IsWindowsClaudeProfile(string? id) => id is "platyr" or "gmail" or "party" or "me";
}
