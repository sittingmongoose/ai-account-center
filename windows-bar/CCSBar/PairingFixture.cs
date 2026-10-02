using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;

namespace CCSBar;

/// <summary>
/// A loopback stand-in for the dashboard's sign-in and device routes (CLIENT API SHEET 4.1 to 4.4), used only by the
/// offline checks. It answers /api/auth/check, setup, pair, devices/me, rotate and Disconnect, the legacy cookie login,
/// the dashboard read and the Claude profile list, and records every request (method, path, status, and whether it
/// carried a bearer key or a cookie). Every behaviour the sign-in screen depends on can be scripted. Nothing here ever
/// reaches a real dashboard or account.
/// </summary>
internal sealed class PairingFixture : IDisposable
{
    private readonly HttpListener listener = new();
    private readonly object gate = new();
    private readonly List<string> requests = new();
    private readonly Dictionary<string, (string Id, string State)> tokens = new(StringComparer.Ordinal);
    private int deviceCounter;

    public string Origin { get; }
    public string Username { get; set; } = "fixture";
    public string Password { get; set; } = "fixture-new";
    /// <summary>The owner switch: true, false, or null for a dashboard that predates pairing (no field at all).</summary>
    public bool? TrustSwitch { get; set; } = true;
    public bool ConnectionTrusted { get; set; } = true;
    public string Peer { get; set; } = "192.168.1.31";
    public bool SecureTransport { get; set; }
    public string AccessMode { get; set; } = "login";
    public bool PairRouteExists { get; set; } = true;
    public int TriesLeft { get; set; } = 5;
    public int? RateLimitSeconds { get; set; }
    public string SetupCode { get; set; } = "ABCD-2345";
    public string RotateAfter { get; set; } = "2026-11-01T00:00:00.000Z";
    /// <summary>What devices/me answers for a good key: 200, or a scripted status ("401:invalid_token", "abort").</summary>
    public string DeviceMeMode { get; set; } = "200";
    /// <summary>What the dashboard read answers for a good key: 200, or "401:device_revoked", "503:auth_store_unavailable".</summary>
    public string DashboardMode { get; set; } = "200";
    public string? RevokedReason { get; set; }
    public string? RevokedBy { get; set; }
    public string RotateMode { get; set; } = "200";
    public string DisconnectMode { get; set; } = "204";
    /// <summary>How long pair waits before it answers, after it has already replaced the install's record (the real
    /// dashboard revokes a Re-pair's old key before the answer reaches the tray).</summary>
    public int PairDelayMs { get; set; }
    /// <summary>Pair replaces the record and issues the key, then drops the connection instead of answering.</summary>
    public bool PairDropsAnswer { get; set; }
    /// <summary>How long rotate waits before it answers, after it has issued the new key.</summary>
    public int RotateDelayMs { get; set; }
    /// <summary>The next N devices/me requests (any key) are dropped, as a flaky network would.</summary>
    public int DeviceMeDropNext { get; set; }
    public string? LastInstallId { get; private set; }
    public string? LastDeviceName { get; private set; }
    public string? LastBearer { get; private set; }
    public int Unexpected { get; private set; }

    public PairingFixture()
    {
        var socket = new TcpListener(IPAddress.Loopback, 0); socket.Start();
        var port = ((IPEndPoint)socket.LocalEndpoint).Port; socket.Stop();
        Origin = $"http://127.0.0.1:{port}";
        listener.Prefixes.Add(Origin + "/"); listener.Start();
        _ = Task.Run(Loop);
    }

    public List<string> Requests { get { lock (gate) return requests.ToList(); } }
    public int Count(string prefix) { lock (gate) return requests.Count(line => line.StartsWith(prefix, StringComparison.Ordinal)); }
    public void ClearLog() { lock (gate) requests.Clear(); }

    /// <summary>Revokes every key the fixture issued (the dashboard's Revoke, or Sign out all devices).</summary>
    public void RevokeAll(string state = "revoked") { lock (gate) foreach (var key in tokens.Keys.ToList()) tokens[key] = (tokens[key].Id, state); }
    public int ActiveKeys { get { lock (gate) return tokens.Values.Count(value => value.State == "active"); } }

    private static string NewToken() => "aacd_" + Convert.ToBase64String(RandomNumberGenerator.GetBytes(32)).TrimEnd('=').Replace('+', '-').Replace('/', '_');

    private async Task Loop()
    {
        while (true)
        {
            HttpListenerContext context;
            try { context = await listener.GetContextAsync(); } catch { return; }
            _ = Task.Run(() => Handle(context));
        }
    }

    private async Task Handle(HttpListenerContext context)
    {
        var request = context.Request; var path = request.Url!.AbsolutePath; var method = request.HttpMethod;
        string body = "";
        if (request.HasEntityBody) { using var reader = new StreamReader(request.InputStream); body = await reader.ReadToEndAsync(); }
        var bearer = request.Headers["Authorization"] is { } header && header.StartsWith("Bearer ", StringComparison.Ordinal) ? header[7..] : null;
        var cookie = request.Cookies["fixture-session"]?.Value == "yes";
        int status = 200; object? payload = null; var abort = false; int? retry = null; var delay = 0;
        lock (gate)
        {
            if (bearer is not null) LastBearer = bearer;
            (string Id, string State)? device = bearer is not null && tokens.TryGetValue(bearer, out var found) ? found : null;
            (int, object)? DeviceRejection()
            {
                if (device is null) return (401, new { error = "The device token is not valid.", code = "invalid_token" });
                if (device.Value.State == "revoked") return (401, RevokedReason is null && RevokedBy is null ? new { error = "This device was signed out from the dashboard.", code = "device_revoked" } : (object)new { error = "This device was signed out from the dashboard.", code = "device_revoked", revokedReason = RevokedReason, revokedBy = RevokedBy, revokedAt = "2026-10-02T12:00:00.000Z" });
                if (device.Value.State == "expired") return (401, new { error = "This device token expired after 90 days without use.", code = "device_expired" });
                if (device.Value.State == "rotated-away") return (401, new { error = "The device token is not valid.", code = "invalid_token" });
                return null;
            }
            (int, object)? Scripted(string mode)
            {
                if (mode == "abort") { abort = true; return (0, new { }); }
                var parts = mode.Split(':');
                var code = int.Parse(parts[0], System.Globalization.CultureInfo.InvariantCulture);
                if (code is 200 or 204) return null;
                return (code, new { error = "Fixture refusal.", code = parts.Length > 1 ? parts[1] : null });
            }
            if (method == "GET" && path == "/api/auth/check")
            {
                payload = TrustSwitch is null
                    ? new Dictionary<string, object?> { ["authRequired"] = true, ["authEnabled"] = true, ["authConfigured"] = AccessMode == "login", ["accessMode"] = AccessMode, ["authenticated"] = false }
                    : new Dictionary<string, object?>
                    {
                        ["authRequired"] = true, ["authEnabled"] = true, ["authConfigured"] = AccessMode == "login", ["isLocalAccess"] = false, ["accessMode"] = AccessMode,
                        ["authenticated"] = false, ["username"] = null, ["signedOutReason"] = null, ["setupCodeRequired"] = AccessMode == "setup",
                        ["secureTransport"] = SecureTransport, ["secureOrigin"] = null, ["trustedLocalNetwork"] = TrustSwitch,
                        ["connection"] = new { peer = Peer, trusted = TrustSwitch == true && ConnectionTrusted },
                    };
            }
            else if (method == "POST" && path == "/api/auth/devices/pair")
            {
                using var json = JsonDocument.Parse(body.Length > 0 ? body : "{}");
                var root = json.RootElement;
                var canPair = SecureTransport || TrustSwitch == true && ConnectionTrusted;
                if (!PairRouteExists) { status = 404; payload = new { error = "Not found" }; }
                else if (!canPair) { status = 403; payload = new { error = "Use the secure dashboard address.", code = "secure_transport_required", secureOrigin = (string?)null }; }
                else if (RateLimitSeconds is int seconds) { status = 429; retry = seconds; payload = new { error = "Too many attempts.", code = "rate_limited", retryAfterSeconds = seconds }; }
                else if (root.GetProperty("username").GetString() != Username || root.GetProperty("password").GetString() != Password || AccessMode != "login")
                {
                    TriesLeft = Math.Max(0, TriesLeft - 1);
                    status = 401; payload = new { error = "Invalid credentials", code = "invalid_credentials", triesLeft = TriesLeft };
                }
                else
                {
                    LastInstallId = root.TryGetProperty("installId", out var install) ? install.GetString() : null;
                    LastDeviceName = root.GetProperty("deviceName").GetString();
                    // Pairing again with the same installId revokes that install's old key.
                    foreach (var key in tokens.Keys.ToList()) if (tokens[key].Id == "dev_" + Hex(LastInstallId)) tokens[key] = (tokens[key].Id, "revoked");
                    var token = NewToken(); var id = "dev_" + Hex(LastInstallId ?? (++deviceCounter).ToString(System.Globalization.CultureInfo.InvariantCulture));
                    tokens[token] = (id, "active");
                    status = 201; payload = new { deviceId = id, token, name = LastDeviceName, platform = "windows", pairedAt = "2026-10-02T12:00:00.000Z", rotateAfter = RotateAfter };
                    delay = PairDelayMs; abort = PairDropsAnswer;
                }
            }
            else if (method == "POST" && path == "/api/auth/setup")
            {
                using var json = JsonDocument.Parse(body.Length > 0 ? body : "{}");
                var root = json.RootElement;
                if (AccessMode != "setup") { status = 409; payload = new { error = "Already set up.", code = "already_configured" }; }
                else if (root.GetProperty("setupCode").GetString() != SetupCode) { TriesLeft = Math.Max(0, TriesLeft - 1); status = 403; payload = new { error = "The setup code is not valid.", code = "setup_code_invalid", triesLeft = TriesLeft }; }
                else
                {
                    Username = root.GetProperty("username").GetString()!; Password = root.GetProperty("password").GetString()!; AccessMode = "login";
                    status = 201; payload = new { ok = true, username = Username, session = new { expiresAt = "2026-10-03T12:00:00.000Z" } };
                }
            }
            else if (path == "/api/auth/devices/me" && method == "GET")
            {
                if (DeviceMeDropNext > 0) { DeviceMeDropNext--; abort = true; }
                var rejected = DeviceRejection() ?? Scripted(DeviceMeMode);
                if (rejected is { } no) { status = no.Item1; payload = no.Item2; }
                else payload = new { id = device!.Value.Id, name = LastDeviceName, platform = "windows", pairedAt = "2026-10-02T12:00:00.000Z", rotateAfter = RotateAfter, idleExpiresAt = "2026-12-31T12:00:00.000Z" };
            }
            else if (path == "/api/auth/devices/me/rotate" && method == "POST")
            {
                var rejected = (SecureTransport || TrustSwitch == true && ConnectionTrusted ? null : ((int, object)?)(403, new { error = "Use the secure dashboard address.", code = "secure_transport_required" })) ?? DeviceRejection() ?? Scripted(RotateMode);
                if (rejected is { } no) { status = no.Item1; payload = no.Item2; }
                else
                {
                    var next = NewToken(); tokens[next] = (device!.Value.Id, "active");
                    // The previous key stays valid until the new one is first used (contract section 7).
                    tokens[bearer!] = (device.Value.Id, "previous:" + next);
                    payload = new { token = next, rotateAfter = "2026-12-01T00:00:00.000Z" };
                    delay = RotateDelayMs;
                }
            }
            else if (path == "/api/auth/devices/me" && method == "DELETE")
            {
                var rejected = DeviceRejection() ?? Scripted(DisconnectMode);
                if (rejected is { } no) { status = no.Item1; payload = no.Item2; }
                else { tokens[bearer!] = (device!.Value.Id, "revoked"); status = 204; }
            }
            else if (method == "POST" && path == "/api/auth/login")
            {
                using var json = JsonDocument.Parse(body.Length > 0 ? body : "{}");
                if (json.RootElement.GetProperty("username").GetString() == Username && json.RootElement.GetProperty("password").GetString() == Password)
                { context.Response.SetCookie(new Cookie("fixture-session", "yes", "/") { HttpOnly = true }); payload = new { success = true, username = Username }; }
                else { status = 401; payload = new { error = "Invalid credentials", code = "invalid_credentials", triesLeft = 4 }; }
            }
            else if (method == "GET" && path is "/api/accounts/dashboard" or "/api/accounts/settings" or "/api/claude/desktop-profiles")
            {
                if (bearer is not null)
                {
                    // A previous key of a rotation still works until the new key is used once.
                    if (device is { State: var state } && state.StartsWith("previous:", StringComparison.Ordinal)) device = (device.Value.Id, "active");
                    var rejected = DeviceRejection() ?? (path == "/api/accounts/dashboard" ? Scripted(DashboardMode) : null);
                    if (rejected is { } no) { status = no.Item1; payload = no.Item2; }
                    if (device is { State: "active" } && tokens.FirstOrDefault(pair => pair.Value.State == "previous:" + bearer) is { Key: { } previous }) tokens[previous] = (tokens[previous].Id, "rotated-away");
                }
                else if (!cookie) { status = 401; payload = new { error = "Authentication required", code = "auth_required" }; }
                if (status == 200) payload = path switch
                {
                    "/api/accounts/settings" => new { refreshIntervalSeconds = 60 },
                    "/api/claude/desktop-profiles" => new { profiles = Array.Empty<object>() },
                    _ => (object)new { schemaVersion = 1, updatedAt = "2026-10-02T12:00:00.000Z", accounts = new[] { new { id = "codex:fixture", provider = "codex", providerLabel = "Codex", label = "fixture", email = "fixture@example.com", platform = "ubuntu", status = "ok", isActive = true, windows = new[] { new { key = "seven_day", label = "Weekly", usedPercent = 40, windowMinutes = 10080, kind = "rate_limit" } }, capabilities = new { codexProfile = "fixture" } } }, codexAutoSwitch = new { enabled = false, thresholdPercent = 5 } },
                };
            }
            else if (method == "POST" && path.StartsWith("/api/claude/desktop-profiles/", StringComparison.Ordinal) && path.EndsWith("/open", StringComparison.Ordinal) && bearer is not null)
            {
                // The dashboard checks a device key before any tray route runs.
                if (DeviceRejection() is { } no) { status = no.Item1; payload = no.Item2; }
                else payload = new { opened = true };
            }
            else { Unexpected++; status = 404; payload = new { error = "Not found" }; }
            requests.Add($"{method} {path} {status}{(bearer is not null ? " bearer" : "")}{(cookie ? " cookie" : "")}");
        }
        try
        {
            if (delay > 0) await Task.Delay(delay);
            if (abort) { context.Response.Abort(); return; }
            context.Response.StatusCode = status;
            if (retry is int seconds) context.Response.AddHeader("Retry-After", seconds.ToString(System.Globalization.CultureInfo.InvariantCulture));
            if (payload is null) { context.Response.Close(); return; }
            var bytes = JsonSerializer.SerializeToUtf8Bytes(payload, Formatting.Json);
            context.Response.ContentType = "application/json"; context.Response.ContentLength64 = bytes.Length;
            await context.Response.OutputStream.WriteAsync(bytes); context.Response.Close();
        }
        catch { try { context.Response.Abort(); } catch { } }
    }

    private static string Hex(string? seed)
    {
        var hash = SHA256.HashData(Encoding.UTF8.GetBytes(seed ?? "none"));
        return Convert.ToHexString(hash)[..16].ToLowerInvariant();
    }

    /// <summary>A key the fixture issued is still accepted (the checks prove an old key stops working).</summary>
    public bool Accepts(string token) { lock (gate) return tokens.TryGetValue(token, out var value) && (value.State == "active" || value.State.StartsWith("previous:", StringComparison.Ordinal)); }

    public void Dispose() { try { listener.Stop(); listener.Close(); } catch { } }
}
