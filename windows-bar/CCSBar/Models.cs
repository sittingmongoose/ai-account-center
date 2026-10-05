using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Net;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace CCSBar;

/// <summary>
/// The stored dashboard connection (DPAPI, <see cref="SecureStore"/>). Version 1 (no "version" key) holds the dashboard
/// username and password and signs in with a cookie. Version 2 (CONTRACT-auth-devices section 8) holds a paired device
/// key instead and no password; a version 2 file without a key is what a remote sign-out or Disconnect leaves behind:
/// the address and username stay, so the sign-in screen can say why and fill them in.
/// </summary>
public sealed class ConnectionSettings
{
    public int? Version { get; set; }
    /// <summary>Empty on a new tray: the first-run screen starts with an empty address (placeholder http://).</summary>
    public string BaseURL { get; set; } = "";
    public string Username { get; set; } = "";
    /// <summary>Version 1 only. A paired tray never stores the password.</summary>
    public string? Password { get; set; }
    public string? DeviceId { get; set; }
    public string? DeviceToken { get; set; }
    public string? InstallId { get; set; }
    public string? PairedAt { get; set; }
    /// <summary>When the device key should be rotated (section 7); refreshed from pair, rotate and devices/me.</summary>
    public string? RotateAfter { get; set; }
    /// <summary>Why a version 2 file has no key: device_revoked, device_expired, invalid_token or disconnected.</summary>
    public string? SignedOutReason { get; set; }
    public string? SignedOutAt { get; set; }
    /// <summary>Who and when, when the dashboard's 401 says so (a later server); the screen falls back to its own words.</summary>
    public string? RevokedBy { get; set; }
    /// <summary>Any other members the stored connection holds. They are written back exactly as they were.</summary>
    [JsonExtensionData] public Dictionary<string, JsonElement>? Extra { get; set; }

    [JsonIgnore] public bool IsPaired => !string.IsNullOrEmpty(DeviceToken);
    [JsonIgnore] public bool HasPassword => !string.IsNullOrEmpty(Password);
    /// <summary>A version 2 file whose key was removed (signed out remotely, or Disconnect): nothing to sign in with.</summary>
    [JsonIgnore] public bool IsSignedOut => !IsPaired && !HasPassword;

    /// <summary>The origin, checked: http or https, no user info, path, query or fragment; plain HTTP only to a local
    /// network address or a local name (the address is resolved and checked again before anything is sent).</summary>
    public Uri ValidateAddress()
    {
        if (!Uri.TryCreate(BaseURL.Trim(), UriKind.Absolute, out var uri) ||
            uri.Scheme is not ("http" or "https") ||
            !string.IsNullOrEmpty(uri.UserInfo) || uri.AbsolutePath != "/" ||
            !string.IsNullOrEmpty(uri.Query) || !string.IsNullOrEmpty(uri.Fragment))
            throw new ArgumentException("Enter the dashboard's http or https origin, without a path.");
        if (uri.Scheme == "http" && !LocalNetwork.IsLocalHostName(uri.Host))
            throw new ArgumentException("HTTP is supported for local network dashboards. Use HTTPS for other servers.");
        return uri;
    }

    /// <summary>A connection the tray can use: a device key, or (version 1) a username and password.</summary>
    public Uri Validate()
    {
        var uri = ValidateAddress();
        if (IsPaired)
        {
            if (!DeviceTokenFormat.IsToken(DeviceToken)) throw new ArgumentException("The stored device key is not valid. Pair again.");
            return uri;
        }
        if (string.IsNullOrWhiteSpace(Username) || string.IsNullOrEmpty(Password))
            throw new ArgumentException("Enter your dashboard username and password.");
        return uri;
    }

    /// <summary>A stored file the tray can read: a usable connection, or a signed-out version 2 file.</summary>
    public void ValidateStored()
    {
        if (IsSignedOut) { ValidateAddress(); if (Version != 2) throw new ArgumentException("The stored connection is incomplete."); return; }
        Validate();
    }

    /// <summary>Two addresses name the same dashboard (scheme, host and port), however they were typed.</summary>
    public static bool SameOrigin(string? a, string? b) =>
        Uri.TryCreate((a ?? "").Trim(), UriKind.Absolute, out var x) && Uri.TryCreate((b ?? "").Trim(), UriKind.Absolute, out var y)
        && string.Equals(x.GetLeftPart(UriPartial.Authority), y.GetLeftPart(UriPartial.Authority), StringComparison.OrdinalIgnoreCase);
}

/// <summary>The device key's shape: "aacd_" and 43 base64url characters (CONTRACT-auth-devices section 5).</summary>
public static class DeviceTokenFormat
{
    private static readonly System.Text.RegularExpressions.Regex Shape = new("^aacd_[A-Za-z0-9_-]{43}$", System.Text.RegularExpressions.RegexOptions.CultureInvariant);
    public static bool IsToken(string? token) => token is not null && Shape.IsMatch(token);
}

/// <summary>
/// What counts as this tray's local network: 10/8, 172.16/12, 192.168/16, 127/8, fc00::/7 and ::1, with IPv4-mapped
/// IPv6 normalized first. Public, CGNAT (100.64/10), link-local and unknown addresses are not local, matching the
/// dashboard's default trusted networks (section 2a, rule 4) and the trays concept's state 4.
/// </summary>
public static class LocalNetwork
{
    public static bool IsLocalAddress(IPAddress address)
    {
        if (address.IsIPv4MappedToIPv6) address = address.MapToIPv4();
        if (IPAddress.IsLoopback(address)) return true;
        var bytes = address.GetAddressBytes();
        if (address.AddressFamily == System.Net.Sockets.AddressFamily.InterNetworkV6) return (bytes[0] & 0xfe) == 0xfc;
        if (address.AddressFamily != System.Net.Sockets.AddressFamily.InterNetwork) return false;
        return bytes[0] == 10 || (bytes[0] == 172 && bytes[1] is >= 16 and <= 31) || (bytes[0] == 192 && bytes[1] == 168);
    }

    /// <summary>An address literal on the local network, or a local name: localhost, a single label, or a name under
    /// .local, .lan, .home.arpa or .internal. Any other name is checked by resolving it (<see cref="ResolvesLocally"/>).</summary>
    public static bool IsLocalHostName(string host)
    {
        var name = host.Trim().TrimStart('[').TrimEnd(']').TrimEnd('.');
        if (name.Length == 0) return false;
        if (IPAddress.TryParse(name, out var address)) return IsLocalAddress(address);
        if (name.Equals("localhost", StringComparison.OrdinalIgnoreCase) || !name.Contains('.')) return true;
        var lower = name.ToLowerInvariant();
        return lower.EndsWith(".local", StringComparison.Ordinal) || lower.EndsWith(".lan", StringComparison.Ordinal)
            || lower.EndsWith(".home.arpa", StringComparison.Ordinal) || lower.EndsWith(".internal", StringComparison.Ordinal);
    }

    /// <summary>Every address the host resolves to is local. An address literal is not looked up. Null when the name
    /// could not be resolved (treated as unreachable), so nothing is ever sent to a public address over plain HTTP.</summary>
    public static async System.Threading.Tasks.Task<bool?> ResolvesLocally(string host, TimeSpan timeout)
    {
        var name = host.Trim().TrimStart('[').TrimEnd(']');
        if (IPAddress.TryParse(name, out var literal)) return IsLocalAddress(literal);
        try
        {
            var lookup = Dns.GetHostAddressesAsync(name);
            if (await System.Threading.Tasks.Task.WhenAny(lookup, System.Threading.Tasks.Task.Delay(timeout)) != lookup) return null;
            var addresses = await lookup;
            if (addresses.Length == 0) return null;
            return Array.TrueForAll(addresses, IsLocalAddress);
        }
        catch { return null; }
    }
}

public sealed class AccountDashboard
{
    public int SchemaVersion { get; set; }
    public string UpdatedAt { get; set; } = "";
    public List<DashboardAccount> Accounts { get; set; } = new();
    public AutoSwitchStatus CodexAutoSwitch { get; set; } = new();
    public AntigravityAutoStatus? AntigravityAutoSwitch { get; set; }
    public AccountRefreshSettings? Settings { get; set; }
    /// <summary>The dashboard's provider list (B3a): label, order, and the "Show on dashboard" and "Show in tray" switches.</summary>
    public List<DashboardProvider>? Providers { get; set; }

    /// <summary>Whether the server reports the trays' own visibility ("Show in tray"): providers[].trayVisible or
    /// settings.trayHiddenProviders. Older servers report neither, and every provider then shows.</summary>
    [JsonIgnore] public bool ReportsTrayVisibility => Providers?.Exists(provider => provider.TrayVisible is not null) == true || Settings?.TrayHiddenProviders is not null
        || Settings?.TrayHiddenAccountIds is not null || Accounts.Exists(account => account.TrayHidden is not null);

    /// <summary>Providers hidden in the trays: only "Show in tray" counts (providers[].trayVisible false, or listed in
    /// settings.trayHiddenProviders). "Show on dashboard" (providers[].visible, settings.hiddenProviders) is the
    /// dashboard's own switch and never hides anything here.</summary>
    [JsonIgnore] public IReadOnlySet<string> Hidden => new HashSet<string>(
        (Providers ?? new List<DashboardProvider>()).Where(provider => provider.TrayVisible == false).Select(provider => provider.Id)
            .Concat(Settings?.TrayHiddenProviders ?? new List<string>()).Where(Formatting.IsSafeId), StringComparer.Ordinal);

    /// <summary>Providers hidden on the dashboard (read only, for Settings' facts).</summary>
    [JsonIgnore] public IReadOnlySet<string> HiddenOnDashboard => new HashSet<string>(
        (Providers ?? new List<DashboardProvider>()).Where(provider => provider.Visible == false).Select(provider => provider.Id)
            .Concat(Settings?.HiddenProviders ?? new List<string>()).Where(Formatting.IsSafeId), StringComparer.Ordinal);

    /// <summary>The accounts this tray shows: an account hidden in the trays (accounts[].trayHidden, its own "Show in
    /// tray" or its provider's) is left out. The dashboard's per-account switch (accounts[].hidden) is never read: the
    /// two are independent (Jared, 2026-10-02).</summary>
    [JsonIgnore] public IEnumerable<DashboardAccount> ShownAccounts => Accounts.Where(account => !account.HiddenInTray);

    /// <summary>Accounts hidden in the trays one by one (their provider is still shown), for Settings' facts.</summary>
    [JsonIgnore] public int TrayHiddenAccountCount => Accounts.Count(account => account.HiddenInTray && !Hidden.Contains(account.Provider));
}

/// <summary>One providers[] entry of GET /api/accounts/dashboard (CLIENT API SHEET 4.4).</summary>
public sealed class DashboardProvider
{
    public string Id { get; set; } = "";
    public string? Label { get; set; }
    public int? Order { get; set; }
    public bool? Visible { get; set; }
    /// <summary>"Show in tray"; missing means shown.</summary>
    public bool? TrayVisible { get; set; }
}

public sealed class AccountRefreshSettings
{
    public int RefreshIntervalSeconds { get; set; } = 60;
    public List<string>? HiddenProviders { get; set; }
    public List<string>? TrayHiddenProviders { get; set; }
    public List<string>? HiddenAccountIds { get; set; }
    /// <summary>Accounts hidden in the trays one by one ("Show in tray" off for that account).</summary>
    public List<string>? TrayHiddenAccountIds { get; set; }
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
    /// <summary>"Show in tray" off for this account or its provider (accounts[].trayHidden); missing means shown. The
    /// dashboard's accounts[].hidden is deliberately not read.</summary>
    public bool? TrayHidden { get; set; }
    public List<QuotaWindow> Windows { get; set; } = new();
    public AccountCapabilities Capabilities { get; set; } = new();
    [JsonIgnore] public bool HasUsableUsage => Windows.Exists(window => window.HasUsableUsage);
    [JsonIgnore] public bool HiddenInTray => TrayHidden == true;
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
    /// <summary>F6 from the server (MISC): the reset has passed and the reading predates it. Only ever true when sent.</summary>
    public bool? ResetPassed { get; set; }
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
    public string? AntigravityProfileId { get; set; }
    public List<string>? AntigravityHostIds { get; set; }
    public bool AntigravityCanActivate { get; set; }
}

/// <summary>The Open progress of one Claude profile, exactly as GET /api/claude/desktop-profiles reports it
/// (CONTRACT-serving-misc 4.4). Counts and states only: no UUIDs, titles, transcript text, ssh details or paths.</summary>
public sealed class ClaudeOpenOperation
{
    public string Id { get; set; } = "";
    public string Platform { get; set; } = "";
    public string State { get; set; } = "";
    public int? ConfirmedCount { get; set; }
    public int? TotalCount { get; set; }
    /// <summary>The server's fixed sentence for blocked_uncertain and failed; null otherwise.</summary>
    public string? Message { get; set; }
    /// <summary>opened, failed and blocked_uncertain end the poll; anything else keeps it running.</summary>
    [JsonIgnore] public bool IsTerminal => State is "opened" or "failed" or "blocked_uncertain";
    [JsonIgnore] public bool IsOpened => State == "opened";
}

/// <summary>One row of GET /api/claude/desktop-profiles. A profile without a manifest id carries no openOperation.</summary>
public sealed class ClaudeDesktopProfile
{
    public string? Id { get; set; }
    public ClaudeOpenOperation? OpenOperation { get; set; }
}

public sealed class ClaudeDesktopProfileList
{
    public List<ClaudeDesktopProfile> Profiles { get; set; } = new();
}

/// <summary>The Open POST's body: 200 {opened, id, platform} or 202 {id, platform, state, operationId}.</summary>
public sealed class ClaudeOpenReply
{
    public bool Opened { get; set; }
    public string? Id { get; set; }
    public string? Platform { get; set; }
    public string? State { get; set; }
    public string? OperationId { get; set; }
}

/// <summary>Antigravity's own automatic switching (thresholdUsedPercent is % USED, unlike Codex).</summary>
public sealed class AntigravityAutoStatus
{
    public bool Enabled { get; set; }
    public double ThresholdUsedPercent { get; set; } = 95;
    public int PollIntervalSeconds { get; set; } = 60;
    public string Outcome { get; set; } = "";
    public string Message { get; set; } = "";
    public bool ActivationInProgress { get; set; }
    public string? LastCheckedAt { get; set; }
    public string? LastSwitchedAt { get; set; }
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
    /// <summary>Profile the monitor chose but could not switch to yet. Present only for waiting_idle.</summary>
    public string? Candidate { get; set; }
    /// <summary>True when the blocked message warns about paid credits being spent.</summary>
    public bool UsingCredits { get; set; }
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
        if (account.Provider == "claude" && IsFable(window) && (!IsMaxPlan(account.Plan) || window.DisplayPercent is null)) return false;
        if (account.Provider == "codex")
        {
            if (IsChatPass(window.Key) || IsChatPass(window.Label)) return false;
            // No Codex 5-hour cell unless one is reported: decided from the data, not from profile names.
            bool fiveHour = window.WindowMinutes == 300 || key is "fivehour" or "5h" or "fivehours";
            if (fiveHour && !window.HasUsableUsage && !DateTimeOffset.TryParse(window.ResetAt, out _)) return false;
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

    /// <summary>A Claude desktop profile id from the dashboard data. No allowlist: any id the server reports is used
    /// when it is path safe ([A-Za-z0-9_-], 1-64), so it cannot be injected into the Open route.</summary>
    public static bool IsSafeClaudeProfile(string? id) => IsSafeProfile(id);

    /// <summary>Server-reported ids (providers, Antigravity profiles): [A-Za-z0-9][A-Za-z0-9_-]{0,63}.</summary>
    public static bool IsSafeId(string? id) => IsSafeProfile(id);

    /// <summary>Claude Max plans ("max", "max_5x", "Max 20x"); Fable is shown only for these.</summary>
    public static bool IsMaxPlan(string? plan) => System.Text.RegularExpressions.Regex.IsMatch(
        (plan ?? "").Replace('_', ' ').Replace('-', ' ').Trim(), @"(?:^|\s)max(?:\s*(?:5|20)\s*x)?(?:$|\s)", System.Text.RegularExpressions.RegexOptions.IgnoreCase);

    public static bool IsFable(QuotaWindow window) => window.Key == "seven_day_fable" || window.Label.Contains("Fable", StringComparison.OrdinalIgnoreCase);

    /// <summary>Claude, Codex and Antigravity first, then the known usage providers, then any other provider the
    /// server reports, in the order it first appears. Nothing is filtered out by name.</summary>
    public static readonly string[] PreferredOrder = { "claude", "codex", "antigravity", "cursor", "muse", "kimi-code", "qwen", "zai", "opencode-go" };

    public static string[] ProviderOrder(IEnumerable<DashboardAccount> accounts)
    {
        var seen = accounts.Select(account => account.Provider).Where(provider => !string.IsNullOrWhiteSpace(provider)).Distinct(StringComparer.Ordinal).ToList();
        return PreferredOrder.Where(seen.Contains).Concat(seen.Where(provider => !PreferredOrder.Contains(provider))).ToArray();
    }

    public static string ProviderName(string provider, string? reported = null) => provider switch
    {
        "claude" => "Claude", "codex" => "Codex", "antigravity" => "Antigravity", "cursor" => "Cursor", "muse" => "Muse Code",
        "kimi-code" => "Kimi Code", "qwen" => "Qwen Token Plan", "zai" => "Z.ai Coding Plan", "opencode-go" => "OpenCode Go",
        _ => string.IsNullOrWhiteSpace(reported) ? provider : reported!
    };

    public static string PlanLabel(string? plan) => string.IsNullOrWhiteSpace(plan) ? "" : plan.Length <= 5 ? char.ToUpperInvariant(plan[0]) + plan[1..] : plan;
    public static string PlatformName(string? platform) => platform switch { "mac" => "Mac", "windows" => "Windows", "ubuntu" => "Ubuntu", "linux" => "Linux", null or "" => "", _ => char.ToUpperInvariant(platform[0]) + platform[1..] };

    /// <summary>Clock for relative times; fixture renders pin it to the fixture's capture time.</summary>
    public static Func<DateTimeOffset> Now { get; set; } = () => DateTimeOffset.UtcNow;

    public static string Duration(TimeSpan span)
    {
        if (span < TimeSpan.Zero) span = TimeSpan.Zero;
        if (span.TotalDays >= 1) return $"{(int)span.TotalDays}d {span.Hours}h";
        if (span.TotalHours >= 1) return $"{(int)span.TotalHours}h {span.Minutes}m";
        if (span.TotalMinutes >= 1) return $"{(int)span.TotalMinutes}m";
        return $"{Math.Max(1, (int)span.TotalSeconds)}s";
    }

    public static string Relative(string? timestamp)
    {
        if (!DateTimeOffset.TryParse(timestamp, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var at)) return "";
        var age = Now() - at;
        return age < TimeSpan.FromSeconds(10) ? "just now" : Duration(age) + " ago";
    }

    public static string Clock(DateTimeOffset at) => at.ToLocalTime().ToString("h:mm tt", CultureInfo.CurrentCulture);

    /// <summary>Row form: a countdown, or the clock time once the reset is under 24 hours away; "due" once passed.</summary>
    public static string ResetShort(string? timestamp)
    {
        if (!DateTimeOffset.TryParse(timestamp, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var reset)) return "";
        var left = reset - Now();
        if (left <= TimeSpan.Zero) return "due";
        return left < TimeSpan.FromDays(1) ? Clock(reset) : Duration(left);
    }

    /// <summary>Shorter forms of <see cref="ResetShort"/> for a compact meter too narrow for it, longest first:
    /// "15h 40m" then "15h" under a day ("45m" under an hour), "6d" from a day. The exact time stays in the tooltip.</summary>
    public static string[] ResetFallbacks(string? timestamp)
    {
        if (!DateTimeOffset.TryParse(timestamp, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var reset)) return Array.Empty<string>();
        var left = reset - Now();
        if (left <= TimeSpan.Zero) return Array.Empty<string>();
        if (left >= TimeSpan.FromDays(1)) return new[] { $"{(int)left.TotalDays}d" };
        return left >= TimeSpan.FromHours(1) ? new[] { Duration(left), $"{(int)left.TotalHours}h" } : new[] { Duration(left) };
    }

    /// <summary>Details form: "Resets Thu, Oct 8, 8:00 AM · 6d 20h".</summary>
    public static string ResetLong(string? timestamp)
    {
        if (!DateTimeOffset.TryParse(timestamp, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var reset)) return "No reset reported";
        var left = reset - Now();
        return "Resets " + reset.ToLocalTime().ToString("ddd, MMM d, h:mm tt", CultureInfo.CurrentCulture) + " · " + (left <= TimeSpan.Zero ? "due" : Duration(left));
    }

    /// <summary>
    /// F6: once a window's reset has passed, a reading sampled before it no longer describes the window. Returns the
    /// reset time when <c>resetAt</c> is in the past and the reading was sampled before it, or when its sample time is
    /// unknown; the tray then shows "Reset at ... · new reading pending" with no number and no fill, never 0%.
    /// The sample time is the window's own (a retained window) or else the account's. Null when the reading stands.
    /// </summary>
    public static DateTimeOffset? PendingReset(DashboardAccount account, QuotaWindow window)
    {
        if (window.Unlimited || window.Enabled == false || window.Kind is "balance" or "extra_usage" or "spend") return null;
        if (!DateTimeOffset.TryParse(window.ResetAt, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var reset)) return null;
        // The server's own mark (MISC's resetPassed) is honoured even when this computer's clock is behind it.
        if (window.ResetPassed == true) return reset;
        if (reset > Now()) return null;
        var sampled = window.SampledAt ?? account.SampledAt;
        return DateTimeOffset.TryParse(sampled, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var at) && at >= reset ? null : reset;
    }

    /// <summary>"Reset at 5:15 AM" today, "Reset at Oct 1, 11:00 PM" on another day.</summary>
    public static string ResetAt(DateTimeOffset reset) => "Reset at " + ResetWhen(reset);

    private static string ResetWhen(DateTimeOffset reset)
    {
        var local = reset.ToLocalTime();
        return local.Date == Now().ToLocalTime().Date ? Clock(reset) : local.ToString("MMM d, h:mm tt", CultureInfo.CurrentCulture);
    }

    /// <summary>Details and tooltips: "Reset at Thu, Oct 1, 11:00 PM · new reading pending".</summary>
    public static string ResetPendingLong(DateTimeOffset reset) => "Reset at " + reset.ToLocalTime().ToString("ddd, MMM d, h:mm tt", CultureInfo.CurrentCulture) + " · new reading pending";

    /// <summary>Compact cells, longest first: the whole sentence, then shorter forms for a narrow cell. The tooltip
    /// always has the whole sentence.</summary>
    public static string[] PendingForms(DateTimeOffset reset)
    {
        var when = ResetWhen(reset);
        return new[] { $"Reset at {when} · new reading pending", $"Reset at {when} · pending", $"Reset {when} · pending", $"Reset at {when}", "New reading pending", "Pending" };
    }

    /// <summary>At most two decimals, trailing zeros dropped; "%" is part of the same text run.</summary>
    public static string Percent(double value) => value.ToString(value == Math.Round(value) ? "0" : "0.##", CultureInfo.CurrentCulture) + "%";
    public static int Decimals(double value)
    {
        var rounded = Math.Round(value, 2);
        return rounded == Math.Round(rounded) ? 0 : Math.Round(rounded, 1) == rounded ? 1 : 2;
    }
    public static string PercentWith(double value, int decimals) => value.ToString("F" + decimals, CultureInfo.CurrentCulture) + "%";

    private static readonly IReadOnlySet<string> BlockedAutoSwitch = new HashSet<string> { "waiting_idle", "no_quota", "no_candidate", "error" };

    /// <summary>Why Codex automatic switching is stuck, in plain words — or null when the switch is
    /// healthy, disabled or unreported and no line should show.</summary>
    public static string? CodexAutoStatusText(AutoSwitchStatus? status, List<DashboardAccount>? accounts)
    {
        if (status is null || !status.Enabled) return null;
        if (!BlockedAutoSwitch.Contains(status.Outcome)) return null;
        if (status.Outcome == "waiting_idle" && !string.IsNullOrWhiteSpace(status.Candidate))
        {
            var match = accounts?.Find(account => account.Provider == "codex" && account.Capabilities.CodexProfile == status.Candidate);
            var identity = match?.Email ?? match?.Label ?? status.Candidate;
            return $"{status.Message} Will switch to {identity} when Codex goes idle. Activate {identity} to switch now.";
        }
        return status.Message;
    }

    /// <summary>Notification-area tooltip (max 127 characters): the active Codex account's weekly % left.</summary>
    public static string TrayTooltip(AccountDashboard? dashboard, bool stale = false, bool configured = true, string? signInState = null)
    {
        const string name = "AI Account Center";
        if (!configured) return name + " · " + (string.IsNullOrEmpty(signInState) ? "Not paired" : signInState);
        var active = dashboard is null || dashboard.Hidden.Contains("codex") ? null : dashboard.ShownAccounts.FirstOrDefault(account => account.Provider == "codex" && account.IsActive);
        var weekly = active is null ? null : CodexPrimaryWindows(active).FirstOrDefault(window => window.Key == "seven_day");
        string text = name;
        if (active is not null && weekly is not null && PendingReset(active, weekly) is not null)
            text = $"{name} · Codex: {(active.Email ?? active.Label).Split('@')[0]}, weekly reset, new reading pending";
        else if (active is not null && weekly?.DisplayPercent is double used)
        {
            var left = Math.Max(0, 100 - used);
            var who = (active.Email ?? active.Label).Split('@')[0];
            text = $"{name} · Codex: {who}, {PercentWith(left, Decimals(left))} weekly left";
        }
        if (stale) text += " · last sample";
        return text.Length <= 127 ? text : text[..126] + "…";
    }
}
