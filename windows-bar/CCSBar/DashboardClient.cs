using System;
using System.Collections.Generic;
using System.Net;
using System.Net.Http;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace CCSBar;

internal enum DashboardRequestKind { General, Usage, ClaudeOpen }

/// <summary>
/// The dashboard's tray routes. A paired tray (connection version 2) sends its device key as
/// <c>Authorization: Bearer</c> on every request and never uses the cookie sign-in: a 401 with a device code
/// (device_revoked, device_expired, invalid_token) is a sign-out (<see cref="DeviceSignedOutException"/>), never retried
/// and never answered with the password. A version 1 tray keeps today's cookie sign-in with its saved password.
/// </summary>
public sealed class DashboardClient : IDisposable
{
    private readonly ConnectionSettings settings;
    private readonly HttpClient http;
    private readonly SemaphoreSlim loginGate = new(1, 1);
    private DateTimeOffset lastLoginAttempt = DateTimeOffset.MinValue;
    private readonly Uri origin;
    private volatile string? deviceToken;
    public Uri BaseURL => origin;
    /// <summary>Signs in with a device key rather than a password.</summary>
    public bool Paired => deviceToken is not null;

    public DashboardClient(ConnectionSettings settings)
    {
        this.settings = settings;
        origin = settings.Validate();
        deviceToken = settings.IsPaired ? settings.DeviceToken : null;
        var handler = new HttpClientHandler { CookieContainer = new CookieContainer(), AllowAutoRedirect = false, UseCookies = !settings.IsPaired };
        http = new HttpClient(handler) { BaseAddress = origin, Timeout = TimeSpan.FromSeconds(90) };
        http.DefaultRequestHeaders.Add("Origin", origin.GetLeftPart(UriPartial.Authority));
        http.DefaultRequestHeaders.Add("Accept", "application/json");
        http.DefaultRequestHeaders.Add("User-Agent", "CCSBar-Windows/1.0");
    }

    public async Task<AccountDashboard> Dashboard(bool refresh)
    {
        var dashboard = await Request<AccountDashboard>(HttpMethod.Get,
            "api/accounts/dashboard?platform=windows&refresh=" + (refresh ? "true" : "false"), requestKind: DashboardRequestKind.Usage);
        if (!SupportedDashboard(dashboard))
            throw new InvalidOperationException("The dashboard API version is not supported by this AI Account Center.");
        return dashboard;
    }

    /// <summary>The dashboard still sends a fixed <c>codexAutoSwitch</c> (always off) for trays built before account
    /// switching was removed (2026-11-14); this tray no longer reads or requires it.</summary>
    internal static bool SupportedDashboard(AccountDashboard dashboard) =>
        dashboard.SchemaVersion == 1 && dashboard.Accounts is not null;

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
            // A paired tray's 401 is a sign-out: the Open is never sent again. A version 1 tray renews its cookie once;
            // the refused POST was answered by the sign-in check before the Open route ran, so this is not a replay.
            if (Paired) { using (response) throw await DeviceSignedOutException.From(response); }
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

    /// <summary>GET /api/auth/devices/me: the key works, and when it should next be rotated.</summary>
    public async Task<DeviceRecord> DeviceMe()
    {
        if (!Paired) throw new InvalidOperationException("This tray is not paired.");
        return await Request<DeviceRecord>(HttpMethod.Get, "api/auth/devices/me");
    }

    /// <summary>POST /api/auth/devices/me/rotate (section 7). Null when the dashboard defers it (403
    /// secure_transport_required: this connection is not trusted right now); the current key keeps working.</summary>
    public async Task<RotatedKey?> Rotate()
    {
        if (!Paired) throw new InvalidOperationException("This tray is not paired.");
        using var response = await Send(HttpMethod.Post, "api/auth/devices/me/rotate", null);
        if (response.StatusCode == HttpStatusCode.Unauthorized) throw await DeviceSignedOutException.From(response);
        var answer = await AuthApi.Read(response);
        if (response.StatusCode == HttpStatusCode.Forbidden && answer.Code == "secure_transport_required") return null;
        if (!answer.Ok) throw new InvalidOperationException("The device key could not be rotated now. The current key keeps working.");
        var token = answer.Text("token");
        if (!DeviceTokenFormat.IsToken(token)) throw new InvalidOperationException("The dashboard returned an unreadable device key.");
        return new RotatedKey(token!, answer.Text("rotateAfter"));
    }

    /// <summary>DELETE /api/auth/devices/me: Disconnect. True when the dashboard revoked the key, or it was already
    /// signed out (a 401 device code).</summary>
    public async Task<bool> Disconnect()
    {
        if (!Paired) return true;
        using var response = await Send(HttpMethod.Delete, "api/auth/devices/me", null);
        if (response.StatusCode == HttpStatusCode.NoContent) return true;
        if (response.StatusCode == HttpStatusCode.Unauthorized)
        {
            return await DeviceSignedOutException.From(response) is DeviceSignedOutException { IsDeviceCode: true };
        }
        return false;
    }

    /// <summary>After a rotation is saved: the next request uses the new key (written to disk before its first use).</summary>
    public void UseToken(string token)
    {
        if (!DeviceTokenFormat.IsToken(token)) throw new ArgumentException("The device key is not valid.");
        deviceToken = token;
    }

    private async Task<T> Request<T>(HttpMethod method, string path, object? body = null, DashboardRequestKind requestKind = DashboardRequestKind.General)
    {
        using var first = await Send(method, path, body);
        if (first.StatusCode == HttpStatusCode.Unauthorized && Paired) throw await DeviceSignedOutException.From(first);
        if (first.StatusCode != HttpStatusCode.Unauthorized) return await Decode<T>(first, requestKind);
        await Login();
        using var second = await Send(method, path, body);
        return await Decode<T>(second, requestKind);
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
        if (deviceToken is { } token) request.Headers.Authorization = new System.Net.Http.Headers.AuthenticationHeaderValue("Bearer", token);
        return SendAndDispose(request, cancel);
    }

    private async Task<HttpResponseMessage> SendAndDispose(HttpRequestMessage request, CancellationToken cancel)
    {
        // These small JSON responses are buffered so HttpClient's timeout covers the body.
        using (request)
        {
            await LocalNetwork.RequireLocalHTTP(origin);
            return await http.SendAsync(request, HttpCompletionOption.ResponseContentRead, cancel);
        }
    }

    private async Task Login()
    {
        if (Paired) throw new InvalidOperationException("Dashboard sign-in needs attention. Check Settings.");
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

    internal static async Task<T> Decode<T>(HttpResponseMessage response, DashboardRequestKind requestKind = DashboardRequestKind.General)
    {
        if (!response.IsSuccessStatusCode)
        {
            string? publicCode = null;
            if (requestKind is DashboardRequestKind.ClaudeOpen)
            {
                try
                {
                    using var body = JsonDocument.Parse(await response.Content.ReadAsStringAsync());
                    if (body.RootElement.ValueKind == JsonValueKind.Object)
                    {
                        if (body.RootElement.TryGetProperty("code", out var code) && code.ValueKind == JsonValueKind.String) publicCode = code.GetString();
                    }
                }
                catch (JsonException) { }
            }
            // Public status/code mappings are owned by the client. Arbitrary server
            // error/message strings may contain private paths or authentication data.
            throw new InvalidOperationException(PublicError(response.StatusCode, requestKind, publicCode));
        }
        try
        {
            await using var stream = await response.Content.ReadAsStreamAsync();
            var parsed = await JsonSerializer.DeserializeAsync<T>(stream, Formatting.Json);
            return parsed is null ? throw new JsonException() : parsed;
        }
        catch (JsonException) { throw new InvalidOperationException("The dashboard returned an unreadable accounts response."); }
    }

    private static string PublicError(HttpStatusCode status, DashboardRequestKind kind, string? code)
    {
        if (kind == DashboardRequestKind.ClaudeOpen)
        {
            // The guarded history copy could not be confirmed, so Claude was not opened (CONTRACT-serving-misc 4.4).
            if (status == HttpStatusCode.Conflict && code == "history_unconfirmed") return ClaudeOpenFlow.HistoryUnconfirmed;
            if (status is HttpStatusCode.BadGateway or HttpStatusCode.ServiceUnavailable) return "Claude could not be opened on the selected computer. Check its profile setup and connection.";
            if (status == HttpStatusCode.NotFound) return "The selected Claude profile is not available on that computer.";
        }
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

/// <summary>GET /api/auth/devices/me.</summary>
public sealed class DeviceRecord
{
    public string Id { get; set; } = "";
    public string? Name { get; set; }
    public string? Platform { get; set; }
    public string? PairedAt { get; set; }
    public string? RotateAfter { get; set; }
    public string? IdleExpiresAt { get; set; }
}

public sealed record RotatedKey(string Token, string? RotateAfter);

/// <summary>
/// A 401 on a device key (contract section 9): device_revoked, device_expired or invalid_token. The tray deletes the
/// key, stops polling and shows its signed-out screen; it never retries on its own. A 401 without a device code (an
/// older server's auth_required) is not one of these: <see cref="IsDeviceCode"/> is false and nothing is deleted.
/// </summary>
public sealed class DeviceSignedOutException : Exception
{
    public string Code { get; }
    /// <summary>Who and when, if a later dashboard says so in the 401 body (revokedReason, revokedAt, revokedBy).</summary>
    public string? RevokedReason { get; }
    public string? RevokedAt { get; }
    public string? RevokedBy { get; }
    public bool IsDeviceCode => Code is "device_revoked" or "device_expired" or "invalid_token";

    public DeviceSignedOutException(string code, string? reason = null, string? at = null, string? by = null)
        : base(code == "device_expired" ? "This tray's device key expired. Pair it again." : "This tray was signed out. Pair it again.")
    { Code = code; RevokedReason = reason; RevokedAt = at; RevokedBy = by; }

    public static async Task<Exception> From(HttpResponseMessage response)
    {
        var answer = await AuthApi.Read(response);
        if (answer.Code is "device_revoked" or "device_expired" or "invalid_token")
        {
            var by = answer.Text("revokedBy");
            return new DeviceSignedOutException(answer.Code, answer.Text("revokedReason"), answer.Text("revokedAt"), by is { Length: > 0 and <= 64 } ? by : null);
        }
        return new InvalidOperationException("Dashboard sign-in needs attention. Check Settings.");
    }
}

/// <summary>A connection check that failed before anything was saved; the message is fixed client text.</summary>
public sealed class ConnectionCheckException : Exception
{
    public ConnectionCheckException(string message) : base(message) { }
}
