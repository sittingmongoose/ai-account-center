using System;
using System.Net;
using System.Net.Http;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace CCSBar;

public sealed class DashboardClient : IDisposable
{
    private readonly ConnectionSettings settings;
    private readonly HttpClient http;
    private readonly SemaphoreSlim loginGate = new(1, 1);
    private DateTimeOffset lastLoginAttempt = DateTimeOffset.MinValue;
    private readonly Uri origin;
    public Uri BaseURL => origin;

    public DashboardClient(ConnectionSettings settings)
    {
        this.settings = settings;
        origin = settings.Validate();
        var handler = new HttpClientHandler { CookieContainer = new CookieContainer(), AllowAutoRedirect = false };
        http = new HttpClient(handler) { BaseAddress = origin, Timeout = TimeSpan.FromSeconds(90) };
        http.DefaultRequestHeaders.Add("Origin", origin.GetLeftPart(UriPartial.Authority));
        http.DefaultRequestHeaders.Add("Accept", "application/json");
        http.DefaultRequestHeaders.Add("User-Agent", "CCSBar-Windows/1.0");
    }

    public async Task<AccountDashboard> Dashboard(bool refresh)
    {
        var dashboard = await Request<AccountDashboard>(HttpMethod.Get,
            "api/accounts/dashboard?platform=windows&refresh=" + (refresh ? "true" : "false"));
        if (dashboard.SchemaVersion != 1 || dashboard.Accounts is null || dashboard.CodexAutoSwitch is null)
            throw new InvalidOperationException("The dashboard API version is not supported by this CCS Bar.");
        return dashboard;
    }

    public Task<AutoSwitchStatus> SetAutoSwitch(bool enabled, int? thresholdPercent = null)
    {
        if (thresholdPercent is < 1 or > 99) throw new ArgumentException("Select a threshold between 1% and 99% remaining.");
        object body = thresholdPercent.HasValue ? new { enabled, thresholdPercent = thresholdPercent.Value } : new { enabled };
        return Request<AutoSwitchStatus>(HttpMethod.Put, "api/codex/profiles/auto-switch", body);
    }

    public Task<JsonElement> Activate(string profile, string? confirmationToken = null)
    {
        if (!Formatting.IsSafeProfile(profile)) throw new ArgumentException("Choose a configured Codex account.");
        object body = confirmationToken is null ? new { } : new { confirmationToken };
        return Request<JsonElement>(HttpMethod.Post, "api/codex/profiles/" + Uri.EscapeDataString(profile) + "/activate", body, profile, retryAuthentication: confirmationToken is null);
    }

    public Task<JsonElement> OpenClaudeOnMac(string profile)
    {
        if (!Formatting.IsWindowsClaudeProfile(profile)) throw new ArgumentException("Choose a configured Claude account.");
        return Request<JsonElement>(HttpMethod.Post, "api/claude/desktop-profiles/" + Uri.EscapeDataString(profile) + "/open", new { platform = "mac" });
    }

    private async Task<T> Request<T>(HttpMethod method, string path, object? body = null, string? confirmationProfile = null, bool retryAuthentication = true)
    {
        using var first = await Send(method, path, body);
        if (first.StatusCode != HttpStatusCode.Unauthorized) return await Decode<T>(first, confirmationProfile);
        if (!retryAuthentication)
            throw new InvalidOperationException("The dashboard session changed. Activate again to review a new confirmation.");
        await Login();
        using var second = await Send(method, path, body);
        return await Decode<T>(second, confirmationProfile);
    }

    private Task<HttpResponseMessage> Send(HttpMethod method, string path, object? body)
    {
        var request = new HttpRequestMessage(method, path);
        if (body is not null) request.Content = new StringContent(JsonSerializer.Serialize(body, Formatting.Json), Encoding.UTF8, "application/json");
        return SendAndDispose(request);
    }

    private async Task<HttpResponseMessage> SendAndDispose(HttpRequestMessage request)
    {
        // These small JSON responses are buffered so HttpClient's timeout covers the body.
        using (request) return await http.SendAsync(request, HttpCompletionOption.ResponseContentRead);
    }

    private async Task Login()
    {
        await loginGate.WaitAsync();
        try
        {
            // Avoid exhausting the server's login limit when a password changes.
            if (DateTimeOffset.UtcNow - lastLoginAttempt < TimeSpan.FromMinutes(5))
                throw new InvalidOperationException("Dashboard sign-in needs attention. Check Connection, then try again.");
            lastLoginAttempt = DateTimeOffset.UtcNow;
            using var response = await Send(HttpMethod.Post, "api/auth/login", new { username = settings.Username, password = settings.Password });
            if (!response.IsSuccessStatusCode)
                throw new InvalidOperationException(response.StatusCode == HttpStatusCode.TooManyRequests
                    ? "Dashboard sign-in is temporarily rate limited. Try again later."
                    : "Dashboard sign-in failed. Check Connection.");
            // A successful cookie login can be renewed later without a failure backoff.
            lastLoginAttempt = DateTimeOffset.MinValue;
        }
        finally { loginGate.Release(); }
    }

    private static async Task<T> Decode<T>(HttpResponseMessage response, string? confirmationProfile = null)
    {
        if (response.StatusCode == HttpStatusCode.Conflict && confirmationProfile is not null)
        {
            try
            {
                using var body = JsonDocument.Parse(await response.Content.ReadAsStringAsync());
                if (body.RootElement.TryGetProperty("code", out var code) && code.ValueKind == JsonValueKind.String && code.GetString() == "busy"
                    && body.RootElement.TryGetProperty("reason", out var reason) && reason.ValueKind == JsonValueKind.String && reason.GetString() == "running_processes"
                    && body.RootElement.TryGetProperty("confirmation", out var proposed))
                {
                    var confirmation = proposed.Deserialize<CodexSwitchConfirmation>(Formatting.Json);
                    if (confirmation is not null && confirmation.IsValidFor(confirmationProfile))
                        throw new CodexConfirmationRequiredException(confirmation);
                }
            }
            catch (JsonException) { }
        }
        if (!response.IsSuccessStatusCode)
        {
            var fallback = response.StatusCode switch
            {
                HttpStatusCode.Unauthorized => "Dashboard sign-in needs attention. Check Connection.",
                HttpStatusCode.NotFound => "The accounts dashboard is not available on this server yet.",
                HttpStatusCode.Conflict => "Codex is busy. Try switching after its work finishes.",
                HttpStatusCode.Forbidden => "The dashboard rejected this action. Check the connection origin.",
                _ => "The dashboard could not complete this request. Try again later."
            };
            // Sanitized server messages are useful for busy/invalid-action responses only.
            if ((int)response.StatusCode is 400 or 409 or 502 or 504)
            {
                try
                {
                    var error = JsonDocument.Parse(await response.Content.ReadAsStringAsync());
                    using (error)
                        if (error.RootElement.TryGetProperty("error", out var message) && message.ValueKind == JsonValueKind.String)
                        {
                            var text = message.GetString();
                            if (text is { Length: > 0 and < 301 }) fallback = text;
                        }
                }
                catch (JsonException) { }
            }
            throw new InvalidOperationException(fallback);
        }
        try
        {
            await using var stream = await response.Content.ReadAsStreamAsync();
            var parsed = await JsonSerializer.DeserializeAsync<T>(stream, Formatting.Json);
            return parsed is null ? throw new JsonException() : parsed;
        }
        catch (JsonException) { throw new InvalidOperationException("The dashboard returned an unreadable accounts response."); }
    }

    public void Dispose() { http.Dispose(); loginGate.Dispose(); }
}
