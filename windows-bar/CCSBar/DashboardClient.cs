using System;
using System.Collections.Generic;
using System.Net;
using System.Net.Http;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace CCSBar;

internal enum DashboardRequestKind { General, Usage, AutoSwitch, ClaudeOpen, CodexActivation, AntigravityActivation, AntigravityAutoSwitch }

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

    /// <summary>Antigravity's own policy (commit 9cf75fbe): PUT /api/antigravity/auto-switch with only the changed keys.</summary>
    public Task<AntigravityAutoStatus> SetAntigravityAutoSwitch(bool? enabled = null, int? thresholdUsedPercent = null)
    {
        if (thresholdUsedPercent is < 1 or > 99) throw new ArgumentException("Select a threshold between 1% and 99% used.");
        if (enabled is null && thresholdUsedPercent is null) throw new ArgumentException("Choose a setting to change.");
        var body = new System.Collections.Generic.Dictionary<string, object>();
        if (enabled is bool on) body["enabled"] = on;
        if (thresholdUsedPercent is int used) body["thresholdUsedPercent"] = used;
        return Request<AntigravityAutoStatus>(HttpMethod.Put, "api/antigravity/auto-switch", body, requestKind: DashboardRequestKind.AntigravityAutoSwitch);
    }

    /// <summary>POST /api/antigravity/profiles/{id}/activate, or /confirm with the reviewed token (Ubuntu host only).</summary>
    public Task<JsonElement> ActivateAntigravity(string profileId, string? confirmationToken = null)
    {
        if (!Formatting.IsSafeId(profileId)) throw new ArgumentException("Choose a configured Antigravity account.");
        if (confirmationToken is not null && !AntigravityConfirmation.IsToken(confirmationToken)) throw new ArgumentException("The switch confirmation is invalid. Refresh and try again.");
        object body = confirmationToken is null ? new { hostId = "ubuntu" } : new { hostId = "ubuntu", confirmationToken };
        var path = "api/antigravity/profiles/" + Uri.EscapeDataString(profileId) + (confirmationToken is null ? "/activate" : "/confirm");
        return Request<JsonElement>(HttpMethod.Post, path, body, profileId, retryAuthentication: confirmationToken is null, requestKind: DashboardRequestKind.AntigravityActivation);
    }

    /// <summary>Claude "Open on Mac" and "Open on Windows". `Prefer: respond-async` opts into the 202 progress answer;
    /// without it the server waits and answers 200. One Open is one POST: the only re-send is the session handshake
    /// after a 401, which the launcher never saw.</summary>
    public async Task<ClaudeOpenStart> OpenClaude(string profile, string platform)
    {
        if (!Formatting.IsSafeClaudeProfile(profile) || platform is not ("mac" or "windows"))
            throw new ArgumentException("Choose a configured Claude account.");
        var path = "api/claude/desktop-profiles/" + Uri.EscapeDataString(profile) + "/open";
        object body = new { platform };
        var response = await Send(HttpMethod.Post, path, body, prefer: RespondAsync);
        if (response.StatusCode == HttpStatusCode.Unauthorized)
        {
            response.Dispose();
            await Login();
            response = await Send(HttpMethod.Post, path, body, prefer: RespondAsync);
        }
        using (response)
        {
            var reply = await Decode<ClaudeOpenReply>(response, requestKind: DashboardRequestKind.ClaudeOpen);
            return new ClaudeOpenStart(response.StatusCode == HttpStatusCode.Accepted, reply.OperationId ?? "");
        }
    }

    /// <summary>The Claude desktop profile list, read while an Open runs for its openOperation progress. It is on the
    /// tray's device-token allowlist and carries no UUIDs, titles, transcript text, ssh details or paths.</summary>
    public async Task<IReadOnlyList<ClaudeDesktopProfile>> ClaudeDesktopProfiles()
    {
        var list = await Request<ClaudeDesktopProfileList>(HttpMethod.Get, "api/claude/desktop-profiles");
        return list.Profiles;
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

    /// <summary>
    /// Sign-in and Change: proves the entered address and login before anything is saved. One login (no retry and
    /// no failure backoff), then GET /api/accounts/settings, a small read that always needs the session and answers
    /// with the dashboard's refresh settings. A rejected login, an address that does not answer as the dashboard, or
    /// the time limit throws <see cref="ConnectionCheckException"/> with fixed client text; Cancel throws
    /// <see cref="OperationCanceledException"/>. Server strings never reach the screen.
    /// </summary>
    public async Task Verify(TimeSpan timeout, CancellationToken cancel)
    {
        using var limit = CancellationTokenSource.CreateLinkedTokenSource(cancel);
        limit.CancelAfter(timeout);
        try
        {
            using (var login = await Send(HttpMethod.Post, "api/auth/login", new { username = settings.Username, password = settings.Password }, limit.Token))
                if (!login.IsSuccessStatusCode) throw new ConnectionCheckException(LoginFailure(login.StatusCode));
            using var read = await Send(HttpMethod.Get, "api/accounts/settings", null, limit.Token);
            if (read.StatusCode == HttpStatusCode.Unauthorized) throw new ConnectionCheckException("The dashboard accepted the sign-in but did not keep the session. Try again.");
            if (!read.IsSuccessStatusCode || !IsRefreshSettings(await read.Content.ReadAsStringAsync(limit.Token)))
                throw new ConnectionCheckException(NotTheDashboard);
            lastLoginAttempt = DateTimeOffset.MinValue;
        }
        catch (OperationCanceledException) when (!cancel.IsCancellationRequested) { throw new ConnectionCheckException("The dashboard took too long to answer."); }
        catch (HttpRequestException) { throw new ConnectionCheckException("Could not reach a dashboard at that address."); }
    }

    private const string NotTheDashboard = "That address answered, but not as an AI Account Center dashboard.";

    internal static string LoginFailure(HttpStatusCode status) => status switch
    {
        HttpStatusCode.Unauthorized => "The dashboard did not accept that username and password.",
        HttpStatusCode.TooManyRequests => "The dashboard is limiting sign-in attempts. Wait 15 minutes, then try again.",
        HttpStatusCode.BadRequest => "The dashboard rejected this sign-in. Check that its password sign-in is set up.",
        HttpStatusCode.Forbidden => "The dashboard rejected a sign-in from this address. Check the address.",
        HttpStatusCode.NotFound or HttpStatusCode.MethodNotAllowed => NotTheDashboard,
        _ when (int)status is >= 300 and < 400 => NotTheDashboard,
        _ => "The dashboard could not check this sign-in. Try again."
    };

    private static bool IsRefreshSettings(string body)
    {
        try
        {
            using var json = JsonDocument.Parse(body);
            return json.RootElement.ValueKind == JsonValueKind.Object && json.RootElement.TryGetProperty("refreshIntervalSeconds", out var interval) && interval.ValueKind == JsonValueKind.Number;
        }
        catch (JsonException) { return false; }
    }

    /// <summary>The RFC 7240 preference that asks the Open route for its 202 progress answer.</summary>
    private const string RespondAsync = "respond-async";

    private Task<HttpResponseMessage> Send(HttpMethod method, string path, object? body, CancellationToken cancel = default, string? prefer = null)
    {
        var request = new HttpRequestMessage(method, path);
        if (body is not null) request.Content = new StringContent(JsonSerializer.Serialize(body, Formatting.Json), Encoding.UTF8, "application/json");
        if (prefer is not null) request.Headers.TryAddWithoutValidation("Prefer", prefer);
        return SendAndDispose(request, cancel);
    }

    private async Task<HttpResponseMessage> SendAndDispose(HttpRequestMessage request, CancellationToken cancel)
    {
        // These small JSON responses are buffered so HttpClient's timeout covers the body.
        using (request) return await http.SendAsync(request, HttpCompletionOption.ResponseContentRead, cancel);
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
        if (requestKind == DashboardRequestKind.AntigravityActivation && confirmationProfile is not null && !response.IsSuccessStatusCode)
        {
            string? status = null, reason = null;
            try
            {
                using var body = JsonDocument.Parse(await response.Content.ReadAsStringAsync());
                var root = body.RootElement;
                if (root.ValueKind == JsonValueKind.Object)
                {
                    if (root.TryGetProperty("status", out var statusValue) && statusValue.ValueKind == JsonValueKind.String) status = statusValue.GetString();
                    if (root.TryGetProperty("reason", out var reasonValue) && reasonValue.ValueKind == JsonValueKind.String) reason = reasonValue.GetString();
                    if (response.StatusCode == HttpStatusCode.Conflict && status == "confirmation-required" && root.TryGetProperty("confirmation", out var proposed))
                    {
                        var confirmation = proposed.Deserialize<AntigravityConfirmation>(Formatting.Json);
                        if (confirmation is not null && confirmation.IsValidFor(confirmationProfile))
                            throw new AntigravityConfirmationRequiredException(confirmation);
                        throw new InvalidOperationException("The switch confirmation is invalid. Refresh and try again.");
                    }
                }
            }
            catch (JsonException) { }
            throw new InvalidOperationException(AntigravityError(response.StatusCode, status, reason));
        }
        if (!response.IsSuccessStatusCode)
        {
            string? publicCode = null, publicReason = null;
            if (requestKind is DashboardRequestKind.CodexActivation or DashboardRequestKind.ClaudeOpen)
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

    /// <summary>Fixed public messages for Antigravity activation; server strings are never shown.</summary>
    internal static string AntigravityError(HttpStatusCode status, string? result, string? reason)
    {
        if (status == HttpStatusCode.Conflict) switch (result)
        {
            case "busy": return reason == "activation-running" ? "Another Antigravity switch is already running. Wait for it to finish." : "Antigravity is busy. Try switching after its work finishes.";
            case "stale-confirmation": return "The running Antigravity programs or account changed. Activate again to review a new warning.";
            case "deferred": return "Antigravity deferred the switch. Try again in a moment.";
            case "unsupported-runtime-probe": return "Antigravity switching is not available on this server yet.";
            case "confirmation-required": return "The switch confirmation is invalid. Refresh and try again.";
        }
        if (status == HttpStatusCode.BadRequest) return result == "invalid-profile" ? "The selected Antigravity account has no valid saved login." : "The dashboard rejected this switch. Refresh and try again.";
        if (status == HttpStatusCode.InternalServerError) return result switch
        {
            "failed-rolled-back" => "The Antigravity switch failed and was rolled back. Refresh before retrying.",
            "recovery-required" => "The Antigravity switch needs recovery. Open the dashboard before retrying.",
            _ => "Antigravity account activation failed safely. Refresh before retrying."
        };
        return PublicError(status, DashboardRequestKind.General, null, null);
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
            // The guarded history copy could not be confirmed, so Claude was not opened (CONTRACT-serving-misc 4.4).
            if (status == HttpStatusCode.Conflict && code == "history_unconfirmed") return ClaudeOpenFlow.HistoryUnconfirmed;
            if (status is HttpStatusCode.BadGateway or HttpStatusCode.ServiceUnavailable) return "Claude could not be opened on the selected computer. Check its profile setup and connection.";
            if (status == HttpStatusCode.NotFound) return "The selected Claude profile is not available on that computer.";
        }
        if (kind == DashboardRequestKind.AntigravityAutoSwitch && status is HttpStatusCode.BadRequest or HttpStatusCode.UnsupportedMediaType or HttpStatusCode.UnprocessableEntity) return "The Antigravity switching settings were rejected. Refresh and try again.";
        if (kind == DashboardRequestKind.AntigravityAutoSwitch && status == HttpStatusCode.InternalServerError) return "Antigravity automatic switching is not available on this server yet.";
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

/// <summary>A connection check that failed before anything was saved; the message is fixed client text.</summary>
public sealed class ConnectionCheckException : Exception
{
    public ConnectionCheckException(string message) : base(message) { }
}
