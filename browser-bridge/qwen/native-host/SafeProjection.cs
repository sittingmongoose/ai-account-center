using System.Globalization;
using System.Text.Json;
using System.Text.RegularExpressions;

namespace CCS.QwenUsageBridge;

internal sealed record AccountCapabilities(string? CodexProfile = null, string? ClaudeProfileId = null,
                                           string[]? ClaudePlatforms = null);
internal sealed record UsageSample(string Id, string Provider, string ProviderLabel, string Label,
                                  string? Email, string? Plan, string Platform, string Source,
                                  string Status, string? Message, string FetchedAt, string SampledAt,
                                  bool IsActive, IReadOnlyList<UsageWindow> Windows,
                                  AccountCapabilities Capabilities);
internal sealed record UsageWindow(string Key, string Label, double? UsedPercent,
                                  double? RemainingPercent, string? ResetAt, double? WindowMinutes,
                                  double? Used, double? Limit, string? Unit, string Kind,
                                  double? Remaining = null, string? ExpiresAt = null);

internal static class SafeProjection
{
    internal const int MaxWindows = 107;
    internal const int MaxCreditPacks = 100;
    private static readonly Regex CreditPackKey = new(@"\Aaddon-pack-[0-9a-f]{12}\z",
        RegexOptions.CultureInvariant | RegexOptions.NonBacktracking);
    private sealed record WindowDefinition(string Label, double? Minutes, string? Unit, string Kind);
    private static readonly Dictionary<string, WindowDefinition> WindowDefinitions = new(StringComparer.Ordinal)
    {
        ["5h"] = new("5 hours", 300, "credits", "rate_limit"),
        ["weekly"] = new("Weekly", 10080, "credits", "rate_limit"),
        ["monthly"] = new("Monthly", null, "credits", "rate_limit"),
        ["subscription"] = new("Plan subscription", null, null, "rate_limit"),
        ["addon-credits"] = new("Additional credits", null, "credits", "balance"),
        ["addon-packs"] = new("Active credit packs", null, "packs", "balance"),
        ["addon-listed-packs"] = new("Listed active credit packs", null, "packs", "balance")
    };
    private static readonly HashSet<string> Plans = new(StringComparer.Ordinal)
        { "free", "lite", "basic", "standard", "pro", "professional", "premium", "max", "ultra",
          "team", "business", "enterprise", "go" };

    internal static UsageSample Parse(string json, DateTimeOffset now)
    {
        try
        {
            using var document = JsonDocument.Parse(json, new JsonDocumentOptions { MaxDepth = 8 });
            var root = document.RootElement;
            if (root.ValueKind != JsonValueKind.Object || Text(root, "provider") != "qwen" ||
                Text(root, "platform") != "windows")
                throw new SafeFailure("invalid_response");
            var status = Text(root, "status");
            if (status != "ok")
                throw new SafeFailure(status is "needs_sign_in" or "unavailable" ? status : "collector_error");
            var sampled = Timestamp(Property(root, "sampledAt"));
            var fetched = Timestamp(Property(root, "fetchedAt"));
            if (sampled is null || fetched is null || !Fresh(sampled, now) || !Fresh(fetched, now) ||
                Property(root, "windows") is not { ValueKind: JsonValueKind.Array } items ||
                items.GetArrayLength() is < 1 or > MaxWindows)
                throw new SafeFailure("invalid_response");
            var windows = new List<UsageWindow>();
            var seen = new HashSet<string>(StringComparer.Ordinal);
            var creditPacks = 0;
            foreach (var item in items.EnumerateArray())
            {
                var key = Text(item, "key");
                if (key is null || !seen.Add(key))
                    throw new SafeFailure("invalid_response");
                var isCreditPack = false;
                if (!WindowDefinitions.TryGetValue(key, out var definition))
                {
                    if (key.Length != 23 || !CreditPackKey.IsMatch(key) || ++creditPacks > MaxCreditPacks)
                        throw new SafeFailure("invalid_response");
                    isCreditPack = true;
                    definition = new WindowDefinition("Additional credit pack " + creditPacks.ToString(CultureInfo.InvariantCulture),
                                                      null, "credits", "balance");
                }
                var percent = Number(Property(item, "usedPercent"));
                if (percent > 100)
                    percent = null;
                var used = Number(Property(item, "used"));
                var limit = Number(Property(item, "limit"));
                var remaining = Number(Property(item, "remaining"));
                var reset = Timestamp(Property(item, "resetAt"));
                var expires = Timestamp(Property(item, "expiresAt"));
                ValidateCounts(percent, used, limit, remaining);
                // Plan and individual-pack expiration is not a quota reset.
                if (key == "subscription" || isCreditPack)
                    reset = null;
                else
                    expires = null;
                if (key == "addon-listed-packs")
                    reset = null;
                if (percent is null && used is null && limit is null && remaining is null && reset is null && expires is null)
                    continue;
                var unit = definition.Unit;
                if (key is "5h" or "weekly" or "monthly" && limit is null)
                    unit = null;
                windows.Add(new UsageWindow(key, definition.Label, percent,
                    percent is { } valid ? Math.Round(100 - valid, 8) : null, reset, definition.Minutes,
                    used, limit, unit, definition.Kind, remaining, expires));
            }
            if (windows.Count == 0)
                throw new SafeFailure("invalid_response");
            var plan = Text(root, "plan");
            plan = plan is not null && Plans.Contains(plan) ? plan : null;
            // Construct every public string from constants or scalar validators.
            // Never forward raw child labels, source/message, identities, windows
            // or unknown fields (including upstream keys and error bodies).
            return new UsageSample("plan-qwen-windows", "qwen", "Qwen token plan", "Qwen token plan",
                null, plan, "windows", "Browser session on Windows", "ok", null, fetched, sampled,
                false, windows, new AccountCapabilities(ClaudePlatforms: []));
        }
        catch (SafeFailure) { throw; }
        catch { throw new SafeFailure("invalid_response"); }
    }

    private static bool Fresh(string timestamp, DateTimeOffset now)
    {
        var instant = DateTimeOffset.Parse(timestamp, CultureInfo.InvariantCulture);
        return instant >= now.AddMinutes(-5) && instant <= now.AddSeconds(30);
    }

    private static void ValidateCounts(double? percent, double? used, double? limit, double? remaining)
    {
        if (limit is not { } maximum)
            return;
        var epsilon = Math.Max(0.000001, maximum * 0.0000001);
        if (used > maximum + epsilon || remaining > maximum + epsilon ||
            used is { } consumed && remaining is { } left && Math.Abs(consumed + left - maximum) > epsilon)
            throw new SafeFailure("invalid_response");
        var consumedForCheck = used ?? (remaining is { } available ? Math.Max(0, maximum - available) : null);
        // Permit half a hundredth of a percentage point for a producer that
        // reports two decimals; counts remain authoritative and unknown fields
        // are checked without being fabricated in the public DTO.
        if (maximum > 0 && consumedForCheck is { } amount && percent is { } reported &&
            Math.Abs(100 * (amount / maximum) - reported) > 0.005001)
            throw new SafeFailure("invalid_response");
    }

    private static JsonElement? Property(JsonElement element, string key) =>
        element.ValueKind == JsonValueKind.Object && element.TryGetProperty(key, out var result) ? result : null;
    private static string? Text(JsonElement element, string key) =>
        Property(element, key) is { ValueKind: JsonValueKind.String } value ? value.GetString() : null;

    private static double? Number(JsonElement? element)
    {
        if (element is { ValueKind: JsonValueKind.Number } value && value.TryGetDouble(out var number) &&
            double.IsFinite(number) && number >= 0)
            return number;
        return null;
    }

    internal static string? Timestamp(JsonElement? element)
    {
        if (element is not { ValueKind: JsonValueKind.String } value)
            return null;
        var text = value.GetString()!;
        if (text.Length > 64 || !Regex.IsMatch(text, @"(?:[zZ]|[+-][0-9]{2}:[0-9]{2})\z",
                                               RegexOptions.CultureInvariant) ||
            !DateTimeOffset.TryParse(text, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var instant) ||
            instant.Year is < 2000 or > 2200)
            return null;
        return instant.ToUniversalTime().ToString("yyyy-MM-dd'T'HH:mm:ss'Z'", CultureInfo.InvariantCulture);
    }
}
