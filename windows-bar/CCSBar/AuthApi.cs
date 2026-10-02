using System;
using System.Net;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Security.Cryptography;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace CCSBar;

/// <summary>GET /api/auth/check (public): what the sign-in screen needs to choose its state (CLIENT API SHEET 4.1, 4.2).</summary>
public sealed class AuthCheck
{
    public string AccessMode { get; set; } = "";
    public bool AuthConfigured { get; set; }
    public bool SetupCodeRequired { get; set; }
    public bool SecureTransport { get; set; }
    /// <summary>Null on a dashboard that predates device pairing (no trusted-local-network rule): the tray then keeps
    /// today's password sign-in.</summary>
    public bool? TrustedLocalNetwork { get; set; }
    public AuthConnection? Connection { get; set; }
    /// <summary>The dashboard can pair this computer right now: a secure transport, or the trusted local network.</summary>
    public bool CanPair => SecureTransport || Connection?.Trusted == true;
    public bool SupportsPairing => TrustedLocalNetwork is not null;
}

public sealed class AuthConnection
{
    public string Peer { get; set; } = "";
    public bool Trusted { get; set; }
}

/// <summary>One answer from an auth route: its status, its fixed error code (never the error sentence) and body.</summary>
public sealed record AuthAnswer(HttpStatusCode Status, string? Code, JsonElement Body, TimeSpan? RetryAfter)
{
    public bool Ok => (int)Status is >= 200 and < 300;
    public int? Int(string name) => Body.ValueKind == JsonValueKind.Object && Body.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.Number && value.TryGetInt32(out var number) ? number : null;
    public string? Text(string name) => Body.ValueKind == JsonValueKind.Object && Body.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() : null;
}

/// <summary>The address did not answer as an AI Account Center dashboard: no answer (refused, not resolved or timed
/// out), or an answer that is not the dashboard's.</summary>
public sealed class DashboardReachException : Exception
{
    public bool NotDashboard { get; }
    public bool TimedOut { get; }
    public DashboardReachException(bool notDashboard, bool timedOut) : base(notDashboard ? "That address answered, but not as an AI Account Center dashboard." : "Could not reach a dashboard at that address.")
    { NotDashboard = notDashboard; TimedOut = timedOut; }
}

/// <summary>
/// The public and device-key auth routes the sign-in screen calls, for one candidate address. Nothing here is saved:
/// the caller saves a connection only after pair answered 201 and the new key worked once (verify before save).
/// Passwords and keys travel only in request bodies or the Authorization header, never in a URL, and the request bytes
/// that held them are zeroed after sending.
/// </summary>
public sealed class AuthApi : IDisposable
{
    private readonly HttpClient http;
    public Uri Origin { get; }

    public AuthApi(Uri origin, TimeSpan timeout)
    {
        Origin = origin;
        var handler = new HttpClientHandler { AllowAutoRedirect = false, UseCookies = false };
        http = new HttpClient(handler) { BaseAddress = origin, Timeout = timeout };
        // The dashboard accepts a pairing Origin that is absent or its own; the trays send their base URL.
        http.DefaultRequestHeaders.Add("Origin", origin.GetLeftPart(UriPartial.Authority));
        http.DefaultRequestHeaders.Add("Accept", "application/json");
        http.DefaultRequestHeaders.Add("User-Agent", "CCSBar-Windows/1.0");
    }

    /// <summary>GET /api/auth/check. Throws <see cref="DashboardReachException"/> when nothing answers or the answer is
    /// not the dashboard's (no accessMode).</summary>
    public async Task<AuthCheck> Check(CancellationToken cancel = default)
    {
        HttpResponseMessage response;
        try { response = await http.GetAsync("api/auth/check", HttpCompletionOption.ResponseContentRead, cancel); }
        catch (TaskCanceledException) when (!cancel.IsCancellationRequested) { throw new DashboardReachException(false, true); }
        catch (HttpRequestException) { throw new DashboardReachException(false, false); }
        using (response)
        {
            if (!response.IsSuccessStatusCode) throw new DashboardReachException(true, false);
            try
            {
                var check = JsonSerializer.Deserialize<AuthCheck>(await response.Content.ReadAsStringAsync(cancel), Formatting.Json);
                if (check is null || check.AccessMode is not ("open" or "login" or "setup")) throw new DashboardReachException(true, false);
                return check;
            }
            catch (JsonException) { throw new DashboardReachException(true, false); }
        }
    }

    /// <summary>POST /api/auth/devices/pair (contract section 5).</summary>
    public Task<AuthAnswer> Pair(string username, string password, string deviceName, string installId, string? appVersion, CancellationToken cancel = default)
    {
        object body = appVersion is null
            ? new { username, password, deviceName, platform = "windows", installId }
            : new { username, password, deviceName, platform = "windows", installId, appVersion };
        return Send(HttpMethod.Post, "api/auth/devices/pair", body, null, cancel);
    }

    /// <summary>POST /api/auth/setup (contract section 4): first run from the LAN needs the setup code.</summary>
    public Task<AuthAnswer> Setup(string username, string password, string setupCode, CancellationToken cancel = default) =>
        Send(HttpMethod.Post, "api/auth/setup", new { username, password, setupCode }, null, cancel);

    /// <summary>GET /api/auth/devices/me with the device key: the first use that proves a new key works.</summary>
    public Task<AuthAnswer> DeviceMe(string token, CancellationToken cancel = default) => Send(HttpMethod.Get, "api/auth/devices/me", null, token, cancel);

    private async Task<AuthAnswer> Send(HttpMethod method, string path, object? body, string? token, CancellationToken cancel)
    {
        byte[]? bytes = null;
        try
        {
            using var request = new HttpRequestMessage(method, path);
            if (body is not null)
            {
                bytes = JsonSerializer.SerializeToUtf8Bytes(body);
                request.Content = new ByteArrayContent(bytes);
                request.Content.Headers.ContentType = new MediaTypeHeaderValue("application/json") { CharSet = "utf-8" };
            }
            if (token is not null) request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", token);
            using var response = await http.SendAsync(request, HttpCompletionOption.ResponseContentRead, cancel);
            return await Read(response, cancel);
        }
        catch (TaskCanceledException) when (!cancel.IsCancellationRequested) { throw new DashboardReachException(false, true); }
        catch (HttpRequestException) { throw new DashboardReachException(false, false); }
        finally { if (bytes is not null) CryptographicOperations.ZeroMemory(bytes); }
    }

    internal static async Task<AuthAnswer> Read(HttpResponseMessage response, CancellationToken cancel = default)
    {
        JsonElement parsed = default;
        string? code = null;
        try
        {
            var text = await response.Content.ReadAsStringAsync(cancel);
            if (text.Length > 0 && text.Length <= 65536)
            {
                using var document = JsonDocument.Parse(text);
                parsed = document.RootElement.Clone();
                if (parsed.ValueKind == JsonValueKind.Object && parsed.TryGetProperty("code", out var value) && value.ValueKind == JsonValueKind.String) code = value.GetString();
            }
        }
        catch (JsonException) { }
        TimeSpan? retry = response.Headers.RetryAfter?.Delta;
        if (retry is null && parsed.ValueKind == JsonValueKind.Object && parsed.TryGetProperty("retryAfterSeconds", out var seconds) && seconds.ValueKind == JsonValueKind.Number && seconds.TryGetDouble(out var value2) && value2 >= 0)
            retry = TimeSpan.FromSeconds(Math.Min(value2, 24 * 3600));
        return new AuthAnswer(response.StatusCode, code, parsed, retry);
    }

    public void Dispose() => http.Dispose();
}

/// <summary>A 201 from pair: the device key and its record, held in memory until the key has worked once.</summary>
public sealed record PairedDevice(string DeviceId, string Token, string? PairedAt, string? RotateAfter)
{
    public static PairedDevice? From(AuthAnswer answer)
    {
        var token = answer.Text("token");
        var id = answer.Text("deviceId");
        if (answer.Status != HttpStatusCode.Created || !DeviceTokenFormat.IsToken(token) || id is null || !System.Text.RegularExpressions.Regex.IsMatch(id, "^dev_[0-9a-f]{16}$")) return null;
        return new PairedDevice(id, token!, answer.Text("pairedAt"), answer.Text("rotateAfter"));
    }
}
