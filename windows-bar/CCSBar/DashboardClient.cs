using System;
using System.Net;
using System.Net.Http;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace CCSBar;

internal enum DashboardRequestKind { General, Usage, AutoSwitch, ClaudeOpen, CodexActivation }

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
            "api/accounts/dashboard?platform=windows&refresh=" + (refresh ? "true" : "false"), requestKind: DashboardRequestKind.Usage);
        if (dashboard.SchemaVersion != 1 || dashboard.Accounts is null || dashboard.CodexAutoSwitch is null)
            throw new InvalidOperationException("The dashboard API version is not supported by this AI Account Center.");
        return dashboard;
    }

    public Task<AutoSwitchStatus> SetAutoSwitch(bool enabled, int? thresholdPercent = null)
    {
        if (thresholdPercent is < 1 or > 99) throw new ArgumentException("Select a threshold between 1% and 99% remaining.");
        object body = thresholdPercent.HasValue ? new { enabled, thresholdPercent = thresholdPercent.Value } : new { enabled };
        return Request<AutoSwitchStatus>(HttpMethod.Put, "api/codex/profiles/auto-switch", body, requestKind: DashboardRequestKind.AutoSwitch);
    }

    public Task<JsonElement> Activate(string profile, string? confirmationToken = null)
    {
        if (!Formatting.IsSafeProfile(profile)) throw new ArgumentException("Choose a configured Codex account.");
        object body = confirmationToken is null ? new { } : new { confirmationToken };
        return Request<JsonElement>(HttpMethod.Post, "api/codex/profiles/" + Uri.EscapeDataString(profile) + "/activate", body, profile, retryAuthentication: confirmationToken is null, requestKind: DashboardRequestKind.CodexActivation);
    }

    public Task<JsonElement> OpenClaudeOnMac(string profile)
    {
        if (!Formatting.IsWindowsClaudeProfile(profile)) throw new ArgumentException("Choose a configured Claude account.");
        return Request<JsonElement>(HttpMethod.Post, "api/claude/desktop-profiles/" + Uri.EscapeDataString(profile) + "/open", new { platform = "mac" }, requestKind: DashboardRequestKind.ClaudeOpen);
    }

    private async Task<T> Request<T>(HttpMethod method, string path, object? body = null, string? confirmationProfile = null, bool retryAuthentication = true, DashboardRequestKind requestKind = DashboardRequestKind.General)
    {
        using var first = await Send(method, path, body);
        if (first.StatusCode != HttpStatusCode.Unauthorized) return await Decode<T>(first, confirmationProfile, requestKind);
        if (!retryAuthentication)
            throw new InvalidOperationException("The dashboard session changed. Activate again to review a new confirmation.");
        await Login();
        using var second = await Send(method, path, body);
        return await Decode<T>(second, confirmationProfile, requestKind);
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

    internal static async Task<T> Decode<T>(HttpResponseMessage response, string? confirmationProfile = null, DashboardRequestKind requestKind = DashboardRequestKind.General)
    {
        if (response.StatusCode == HttpStatusCode.Conflict && confirmationProfile is not null)
        {
            try
            {
                using var body = JsonDocument.Parse(await response.Content.ReadAsStringAsync());
                if (body.RootElement.ValueKind == JsonValueKind.Object && body.RootElement.TryGetProperty("code", out var code) && code.ValueKind == JsonValueKind.String && code.GetString() == "busy"
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
            string? publicCode = null, publicReason = null;
            if (requestKind == DashboardRequestKind.CodexActivation)
            {
                try
                {
                    using var body = JsonDocument.Parse(await response.Content.ReadAsStringAsync());
                    if (body.RootElement.ValueKind == JsonValueKind.Object)
                    {
                        if (body.RootElement.TryGetProperty("code", out var code) && code.ValueKind == JsonValueKind.String) publicCode = code.GetString();
                        if (body.RootElement.TryGetProperty("reason", out var reason) && reason.ValueKind == JsonValueKind.String) publicReason = reason.GetString();
                    }
                }
                catch (JsonException) { }
            }
            // Public status/code mappings are owned by the client. Arbitrary server
            // error/message strings may contain private paths or authentication data.
            throw new InvalidOperationException(PublicError(response.StatusCode, requestKind, publicCode, publicReason));
        }
        try
        {
            await using var stream = await response.Content.ReadAsStreamAsync();
            var parsed = await JsonSerializer.DeserializeAsync<T>(stream, Formatting.Json);
            return parsed is null ? throw new JsonException() : parsed;
        }
        catch (JsonException) { throw new InvalidOperationException("The dashboard returned an unreadable accounts response."); }
    }

    private static string PublicError(HttpStatusCode status, DashboardRequestKind kind, string? code, string? reason)
    {
        if (kind == DashboardRequestKind.CodexActivation)
        {
            if (status == HttpStatusCode.Conflict && code == "busy") return reason switch
            {
                "activation_running" => "Another Codex account activation is already running. Wait for it to finish.",
                "unsupported_process" => "A running Codex program cannot be restarted safely. Close it and try again.",
                _ => "Codex is busy. Try switching after its work finishes."
            };
            if (status == HttpStatusCode.Conflict && code == "confirmation_stale") return "The running Codex programs or account changed. Activate again to review a new warning.";
            if (status == HttpStatusCode.BadRequest && code == "invalid_profile") return "The selected profile has no valid saved login.";
            if (status == HttpStatusCode.BadRequest && code == "invalid_codex_home") return "Account activation needs the shared Codex configuration.";
            if (status == HttpStatusCode.InternalServerError) switch (code)
            {
                case "restart_failed": return "Codex could not restart. Check its processes before retrying activation.";
                case "verification_failed": return "The activated account could not be verified. Refresh accounts before retrying.";
                case "auth_read_failed": return "The saved Codex login could not be read safely.";
                case "auth_write_failed": return "The Codex login could not be installed safely.";
            }
        }
        if (kind == DashboardRequestKind.ClaudeOpen)
        {
            if (status is HttpStatusCode.BadGateway or HttpStatusCode.ServiceUnavailable) return "Claude could not be opened on the selected computer. Check its profile setup and connection.";
            if (status == HttpStatusCode.NotFound) return "The selected Claude profile is not available on that computer.";
        }
        if (kind == DashboardRequestKind.AutoSwitch && status is HttpStatusCode.BadRequest or HttpStatusCode.UnsupportedMediaType or HttpStatusCode.UnprocessableEntity) return "The automatic switching settings were rejected. Refresh and try again.";
        return status switch
        {
            HttpStatusCode.Unauthorized => "Dashboard sign-in needs attention. Check Settings.",
            HttpStatusCode.Forbidden => "The dashboard rejected this action. Check the connection origin.",
            HttpStatusCode.NotFound => "The requested account action is not available on this server.",
            HttpStatusCode.Conflict => "Another account action is running. Try again when it finishes.",
            HttpStatusCode.TooManyRequests => "The dashboard is limiting requests. Wait before trying again.",
            HttpStatusCode.RequestTimeout or HttpStatusCode.GatewayTimeout => "The dashboard took too long to respond. Try Refresh.",
            HttpStatusCode.BadGateway or HttpStatusCode.ServiceUnavailable => "The requested account service is unavailable. Try Refresh.",
            HttpStatusCode.BadRequest or HttpStatusCode.UnsupportedMediaType or HttpStatusCode.UnprocessableEntity => "The dashboard rejected this request. Refresh and try again.",
            _ => kind == DashboardRequestKind.Usage ? "Usage could not be refreshed. Try Refresh." : "The dashboard could not complete this request. Try again later."
        };
    }

    public void Dispose() { http.Dispose(); loginGate.Dispose(); }
}
