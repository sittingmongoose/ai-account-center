using System.Buffers.Binary;
using System.Security.Cryptography;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace CCS.QwenUsageBridge;

internal static class SelfTests
{
    private static readonly DateTimeOffset Now = DateTimeOffset.Parse("2026-10-01T01:00:00Z");
    private const string Sentinel = "fixture-credential-must-not-appear";
    private static int passed;

    internal static int Run()
    {
        try
        {
            RunProtocolTests();
            RunCookieTests();
            RunProjectionTests();
            RunCreditPackTests();
            if (OperatingSystem.IsWindows())
            {
                var fixture = Encoding.UTF8.GetBytes("ccs-self-test-fixture-not-a-credential");
                var encrypted = UserDpapi.Protect(fixture);
                var decrypted = UserDpapi.UnprotectFixture(encrypted);
                Check(decrypted.SequenceEqual(fixture) && !encrypted.SequenceEqual(fixture), "same-user DPAPI roundtrip");
                CryptographicOperations.ZeroMemory(fixture);
                CryptographicOperations.ZeroMemory(encrypted);
                CryptographicOperations.ZeroMemory(decrypted);
                var scopedFixture = ValidRequest(name: "new_console_session", domain: "home.qwencloud.com");
                var scopedPlaintext = SessionCapsule.Plaintext(scopedFixture, Now);
                var scopedCiphertext = UserDpapi.Protect(scopedPlaintext);
                var scopedDecrypted = UserDpapi.UnprotectFixture(scopedCiphertext);
                using (var scopedDocument = JsonDocument.Parse(scopedDecrypted))
                    Check(scopedDocument.RootElement.EnumerateObject().Count() == 2 &&
                          scopedDocument.RootElement.GetProperty("consoleCookie").GetString() == "new_console_session=" + Sentinel &&
                          scopedDocument.RootElement.GetProperty("gatewayCookie").GetString() == string.Empty,
                          "v2 encrypted scoped header JSON roundtrip");
                using (var envelope = JsonDocument.Parse(SessionCapsule.Envelope(scopedFixture, scopedCiphertext)))
                    Check(envelope.RootElement.GetProperty("version").GetInt32() == 2 &&
                          envelope.RootElement.GetProperty("region").GetString() == "intl" &&
                          envelope.RootElement.GetProperty("cookiesDPAPI").GetString() is { Length: > 0 } &&
                          !envelope.RootElement.TryGetProperty("cookieDPAPI", out _) &&
                          !envelope.RootElement.GetRawText().Contains(Sentinel),
                          "v2 capsule contains only encrypted scoped headers");
                CryptographicOperations.ZeroMemory(scopedPlaintext);
                CryptographicOperations.ZeroMemory(scopedCiphertext);
                CryptographicOperations.ZeroMemory(scopedDecrypted);
                var largePlaintext = SessionCapsule.Plaintext(LargeRequest(), Now);
                var largeCiphertext = UserDpapi.Protect(largePlaintext);
                var largeDecrypted = UserDpapi.UnprotectFixture(largeCiphertext);
                Check(largePlaintext.Length > 65536 && largeCiphertext.Length < 128 * 1024 &&
                      largeDecrypted.SequenceEqual(largePlaintext), "two maximum headers fit the DPAPI bound");
                CryptographicOperations.ZeroMemory(largePlaintext);
                CryptographicOperations.ZeroMemory(largeCiphertext);
                CryptographicOperations.ZeroMemory(largeDecrypted);
                using (var lease = CollectionLease.Acquire())
                {
                    var contended = Task.Run(() =>
                    {
                        if (!OperatingSystem.IsWindows())
                            throw new SafeFailure("unsupported_platform");
                        try { using var other = CollectionLease.Acquire(); return false; }
                        catch (SafeFailure failure) when (failure.Code == "busy") { return true; }
                    }).GetAwaiter().GetResult();
                    Check(contended, "parallel collection contention");
                }
                using (CollectionLease.Acquire())
                    Check(true, "collection lease released");
                var permissionFixture = Path.Combine(AppContext.BaseDirectory, $".permission-fixture-{Guid.NewGuid():N}.tmp");
                try
                {
                    PrivateFiles.WriteAsync(permissionFixture, Encoding.UTF8.GetBytes("ccs-file-permission-fixture"))
                        .GetAwaiter().GetResult();
                    var security = new FileInfo(permissionFixture).GetAccessControl();
                    using var identity = WindowsIdentity.GetCurrent();
                    var system = new SecurityIdentifier(WellKnownSidType.LocalSystemSid, null);
                    var rules = security.GetAccessRules(true, true, typeof(SecurityIdentifier)).Cast<FileSystemAccessRule>().ToArray();
                    var validRules = true;
                    foreach (var rule in rules)
                        validRules &= !rule.IsInherited && rule.AccessControlType == AccessControlType.Allow &&
                                      (rule.IdentityReference.Equals(identity.User) || rule.IdentityReference.Equals(system));
                    Check(security.AreAccessRulesProtected && rules.Length == 2 && validRules,
                          "private ACL applied before file contents");
                }
                finally
                {
                    try { File.Delete(permissionFixture); } catch { }
                }
            }
            Console.WriteLine($"PASS {passed} offline native-host checks; no network or saved credential used.");
            return 0;
        }
        catch
        {
            // Even tests do not print raw JSON/exception text or fixture tokens.
            Console.WriteLine($"FAIL offline native-host check after {passed} passes.");
            return 1;
        }
    }

    private static void RunProtocolTests()
    {
        Protocol.ValidateCaller([Program.AllowedOrigin]);
        Check(true, "pinned origin");
        Protocol.ValidateCaller([Program.AllowedOrigin, "--parent-window=1234"]);
        Check(true, "parent window syntax");
        Fails(() => Protocol.ValidateCaller(["chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/"]), "invalid_caller");
        Fails(() => Protocol.ValidateCaller([Program.AllowedOrigin, "--path=C:\\secret"]), "invalid_caller");
        Fails(() => Protocol.ValidateCaller([]), "invalid_caller");
        Check(Program.ExistingCollectionMode(["--collect-existing"]), "fixed existing-capsule CLI");
        Check(!Program.ExistingCollectionMode([Program.AllowedOrigin]), "browser origin keeps native mode");
        Fails(() => Program.ExistingCollectionMode(["--collect-existing", "--path=C:\\private"]), "invalid_request");
        Fails(() => Program.ExistingCollectionMode(["--collect-existing", "--collect-existing"]), "invalid_request");
        Fails(() => Program.ExistingCollectionMode(["--collect-existing=https://example.org"]), "invalid_request");
        Fails(() => Program.ExistingCollectionMode([Program.AllowedOrigin, "--collect-existing"]), "invalid_request");
        var request = ValidRequest();
        Check(request.Region == "intl" && request.Cookies.Count == 1, "request parse");
        Fails(() => Parse("{\"action\":\"collect\",\"region\":\"evil\",\"cookies\":[]}"), "invalid_request");
        Fails(() => Parse("{\"action\":\"login\",\"region\":\"intl\",\"cookies\":[]}"), "invalid_request");
        Fails(() => Parse("{\"action\":\"collect\",\"action\":\"collect\",\"region\":\"intl\",\"cookies\":[]}"), "invalid_request");
        Fails(() => Parse("{\"action\":\"collect\",\"region\":\"intl\",\"cookies\":[],\"url\":\"https://example.org\"}"), "invalid_request");
        Fails(() => Parse("{}"), "invalid_request");
        Fails(() => Protocol.ParseRequest(new byte[Protocol.MaxMessageBytes + 1]), "invalid_request");
        Fails(() => Protocol.ParseRequest(new byte[] { 0xff, 0xfe }), "invalid_request");
        Fails(() => ValidRequest(name: "ticket\r\nInjected"), "invalid_request");
        Fails(() => ValidRequest(value: "a;b=c"), "invalid_request");
        Fails(() => ValidRequest(value: "a\nb"), "invalid_request");
        Fails(() => ValidRequest(value: "a\u007fb"), "invalid_request");
        Fails(() => ValidRequest(domain: ".qwencloud.com.evil.org"), "invalid_request");
        Fails(() => ValidRequest(domain: ".evilqwencloud.com"), "invalid_request");
        Fails(() => ValidRequest(path: "relative"), "invalid_request");
        Fails(() => Parse(RequestJson(Enumerable.Repeat(Cookie(), 201).ToArray())), "invalid_request");
        Fails(() => Parse(RequestJson([new { name = "ticket", value = "x", domain = ".qwencloud.com", path = "/", secure = true, url = "x" }])), "invalid_request");
        Fails(() => Parse(RequestJson([new { name = "ticket", value = "x", domain = ".qwencloud.com", path = "/", secure = true, expirationDate = -1 }])), "invalid_request");

        var body = Encoding.UTF8.GetBytes(RequestJson([Cookie()]));
        var frame = new byte[4 + body.Length];
        BinaryPrimitives.WriteUInt32LittleEndian(frame, (uint)body.Length);
        body.CopyTo(frame, 4);
        using (var stream = new MemoryStream(frame))
        {
            var framed = Protocol.ReadRequestAsync(stream, CancellationToken.None).GetAwaiter().GetResult();
            Check(framed.Cookies.Count == 1, "length framed request");
        }
        using (var shortPrefix = new MemoryStream([1, 0]))
            Fails(() => Protocol.ReadRequestAsync(shortPrefix, CancellationToken.None).GetAwaiter().GetResult(), "invalid_request");
        using (var shortBody = new MemoryStream([10, 0, 0, 0, 123]))
            Fails(() => Protocol.ReadRequestAsync(shortBody, CancellationToken.None).GetAwaiter().GetResult(), "invalid_request");
        using (var largeFrame = new MemoryStream([1, 0, 4, 0]))
            Fails(() => Protocol.ReadRequestAsync(largeFrame, CancellationToken.None).GetAwaiter().GetResult(), "invalid_request");
        using var output = new MemoryStream();
        Protocol.WriteReplyAsync(output, new NativeReply(false, null, "needs_sign_in")).GetAwaiter().GetResult();
        var reply = output.ToArray();
        Check(BinaryPrimitives.ReadUInt32LittleEndian(reply) == reply.Length - 4, "reply length framing");
        Check(!Encoding.UTF8.GetString(reply[4..]).Contains(Sentinel), "reply sanitized");
    }

    private static void RunCookieTests()
    {
        var info = new Uri("https://home.qwencloud.com/tool/user/info.json");
        Check(Protocol.CookiesFor(ValidRequest(), info, Now).Count == 1, "domain cookie matches");
        Check(Protocol.CookiesFor(ValidRequest(domain: "qwencloud.com"), info, Now).Count == 0, "host-only cookie not broadened");
        Check(Protocol.CookiesFor(ValidRequest(domain: "home.qwencloud.com"), info, Now).Count == 1, "host-only exact matches");
        Check(Protocol.CookiesFor(ValidRequest(path: "/tool"), info, Now).Count == 1, "path boundary matches");
        Check(Protocol.CookiesFor(ValidRequest(path: "/too"), info, Now).Count == 0, "path boundary excludes prefix");
        Check(Protocol.CookiesFor(ValidRequest(path: "/tool/user/"), info, Now).Count == 1, "trailing path slash");
        Check(Protocol.CookiesFor(ValidRequest(expiration: Now.ToUnixTimeSeconds() - 1), info, Now).Count == 0, "expired cookie excluded");
        Check(Protocol.CookiesFor(ValidRequest(expiration: Now.ToUnixTimeSeconds() + 1), info, Now).Count == 1, "live cookie included");
        Check(Protocol.CookiesFor(ValidRequest(), new Uri("http://home.qwencloud.com/tool/user/info.json"), Now).Count == 0, "secure cookie excludes HTTP");
        var shared = SessionCapsule.CookieHeaders(ValidRequest(), Now);
        Check(shared.ConsoleCookie.StartsWith("login_qwencloud_ticket=") && shared.GatewayCookie == shared.ConsoleCookie,
              "shared domain applies independently to both targets");
        var hostOnly = SessionCapsule.CookieHeaders(ValidRequest(name: "new_console_session", domain: "home.qwencloud.com"), Now);
        Check(hostOnly.ConsoleCookie.StartsWith("new_console_session=") && hostOnly.GatewayCookie == string.Empty,
              "host-only auth reaches console only and gateway can be empty");
        var pathOnly = SessionCapsule.CookieHeaders(ValidRequest(path: "/tool"), Now);
        Check(pathOnly.ConsoleCookie.Length > 0 && pathOnly.GatewayCookie == string.Empty,
              "path-only console auth is retained without gateway forwarding");
        Check(SessionCapsule.CookieHeaders(ValidRequest(name: "unrecognized_cookie_name"), Now).ConsoleCookie.Length > 0,
              "provider verifies session without a cookie-name gate");
        Check(SessionCapsule.CookieHeaders(ValidRequest(value: ""), Now).ConsoleCookie.Length > 0,
              "empty cookie value is left for provider verification");
        Fails(() => SessionCapsule.CookieHeaders(ValidRequest(expiration: Now.ToUnixTimeSeconds() - 1), Now), "needs_sign_in");
        Fails(() => SessionCapsule.CookieHeaders(Parse(RequestJson([])), Now), "needs_sign_in");
        Fails(() => SessionCapsule.CookieHeaders(ValidRequest(domain: "cs-data.qwencloud.com"), Now), "needs_sign_in");
        var china = Parse(RequestJson([Cookie(name: "login_aliyunid_ticket", domain: ".aliyun.com")], "cn"));
        var chinaHeaders = SessionCapsule.CookieHeaders(china, Now);
        Check(chinaHeaders.ConsoleCookie.StartsWith("login_aliyunid_ticket=") && chinaHeaders.GatewayCookie == chinaHeaders.ConsoleCookie,
              "China shared domain applies independently to both targets");
        var chinaHostOnly = SessionCapsule.CookieHeaders(Parse(RequestJson([Cookie(name: "cn_auth", domain: "bailian.console.aliyun.com", path: "/cn-beijing")], "cn")), Now);
        Check(chinaHostOnly.ConsoleCookie.StartsWith("cn_auth=") && chinaHostOnly.GatewayCookie == string.Empty,
              "China host-only and path-only auth reaches console only");
        var mixed = Parse(RequestJson([Cookie(name: "shared"), Cookie(name: "console_private", domain: "home.qwencloud.com"),
            Cookie(name: "tool_private", path: "/tool"), Cookie(name: "gateway_private", domain: "cs-data.qwencloud.com"),
            Cookie(name: "data_private", path: "/data")]));
        var scoped = SessionCapsule.CookieHeaders(mixed, Now);
        Check(scoped.ConsoleCookie.Contains("console_private=") && scoped.ConsoleCookie.Contains("tool_private=") &&
              !scoped.ConsoleCookie.Contains("gateway_private=") && !scoped.ConsoleCookie.Contains("data_private="),
              "console preserves its own scoped cookies");
        Check(scoped.GatewayCookie.Contains("gateway_private=") && scoped.GatewayCookie.Contains("data_private=") &&
              !scoped.GatewayCookie.Contains("console_private=") && !scoped.GatewayCookie.Contains("tool_private="),
              "gateway preserves its own scoped cookies");
        var dottedIntl = SessionCapsule.CookieHeaders(Parse(RequestJson([Cookie(name: "console", domain: ".home.qwencloud.com"),
            Cookie(name: "gateway", domain: ".cs-data.qwencloud.com")])), Now);
        Check(dottedIntl.ConsoleCookie.StartsWith("console=") && !dottedIntl.ConsoleCookie.Contains("gateway=") &&
              dottedIntl.GatewayCookie.StartsWith("gateway=") && !dottedIntl.GatewayCookie.Contains("console="),
              "exact dotted intl host scopes remain separate");
        var dottedChina = SessionCapsule.CookieHeaders(Parse(RequestJson([Cookie(name: "console", domain: ".bailian.console.aliyun.com"),
            Cookie(name: "gateway", domain: ".bailian-cs.console.aliyun.com")], "cn")), Now);
        Check(dottedChina.ConsoleCookie.StartsWith("console=") && !dottedChina.ConsoleCookie.Contains("gateway=") &&
              dottedChina.GatewayCookie.StartsWith("gateway=") && !dottedChina.GatewayCookie.Contains("console="),
              "exact dotted China host scopes remain separate");
        Fails(() => ValidRequest(domain: ".home.qwencloud.com.evil.org"), "invalid_request");
        Fails(() => ValidRequest(domain: ".console.aliyun.com"), "invalid_request");
        var large = SessionCapsule.CookieHeaders(LargeRequest(), Now);
        Check(large.ConsoleCookie.Length == 32768 && large.GatewayCookie.Length == 32768,
              "individual maximum header lengths are bounded");
        var plaintext = SessionCapsule.Plaintext(LargeRequest(), Now);
        Check(plaintext.Length <= SessionCapsule.MaxPlaintextBytes, "combined scoped header JSON is bounded");
        CryptographicOperations.ZeroMemory(plaintext);
        Fails(() => Protocol.CookieHeader(Enumerable.Repeat(new BrowserCookie("ticket", new string('a', 16384), ".qwencloud.com", "/", true, null), 3).ToArray()), "invalid_request");
    }

    private static void RunProjectionTests()
    {
        var json = SampleJson();
        var sample = SafeProjection.Parse(json, Now);
        Check(sample.Windows.Count == 6 && sample.Windows[0].Limit == 1000, "all windows and real limits");
        Check(sample.Windows[0].Used == 250 && sample.Windows[0].Remaining == 750, "real usage counts");
        Check(sample.Windows[0].UsedPercent == 25 && sample.Windows[0].RemainingPercent == 75, "percent projection");
        Check(sample.Windows[3].ResetAt is null && sample.Windows[3].ExpiresAt == "2026-11-01T00:00:00Z", "expiry separate from reset");
        Check(sample.Windows[4].Kind == "balance" && sample.Windows[5].Unit == "packs", "extra credits and pack counts");
        var projected = JsonSerializer.Serialize(sample, Program.JsonOptions);
        Check(!projected.Contains(Sentinel), "credential labels and metadata excluded");
        Check(sample.Email is null && !sample.IsActive && sample.Capabilities.ClaudePlatforms!.Length == 0, "usage-only capabilities");
        Check(sample.Plan == "pro", "allowlisted plan");
        var unknownPlan = SafeProjection.Parse(json.Replace("\"plan\":\"pro\"", $"\"plan\":\"{Sentinel}\""), Now);
        Check(unknownPlan.Plan is null, "unknown plan stays null");
        Fails(() => SafeProjection.Parse(json.Replace("\"provider\":\"qwen\"", "\"provider\":\"cursor\""), Now), "invalid_response");
        Fails(() => SafeProjection.Parse(json.Replace("\"platform\":\"windows\"", "\"platform\":\"mac\""), Now), "invalid_response");
        Fails(() => SafeProjection.Parse(json.Replace("\"status\":\"ok\"", "\"status\":\"needs_sign_in\""), Now), "needs_sign_in");
        Fails(() => SafeProjection.Parse(json.Replace("\"key\":\"5h\"", $"\"key\":\"{Sentinel}\""), Now), "invalid_response");
        Fails(() => SafeProjection.Parse(json, Now.AddHours(1)), "invalid_response");
        Fails(() => SafeProjection.Parse("{}", Now), "invalid_response");
        Fails(() => SafeProjection.Parse(json.Replace("\"remaining\":750,", "\"remaining\":999,"), Now), "invalid_response");
        Fails(() => SafeProjection.Parse(json.Replace("\"usedPercent\":25", "\"usedPercent\":90"), Now), "invalid_response");
        Fails(() => SafeProjection.Parse(json.Replace("\"used\":250,", "\"used\":1500,"), Now), "invalid_response");
        Fails(() => SafeProjection.Parse(WithFirstNumbers(json, null, 1000, 750, 90), Now), "invalid_response");
        Check(SafeProjection.Parse(WithFirstNumbers(json, 1, 3, 2, 33.33), Now).Windows[0].UsedPercent == 33.33,
              "rounded percent is consistent");
        Check(SafeProjection.Parse(WithFirstNumbers(json, 1e307, 1e308, 9e307, 10), Now).Windows[0].UsedPercent == 10,
              "finite high counts avoid percent multiplication overflow");
        foreach (var percentage in new[] { 0.0125, 1.25, 125 })
        {
            var used = 10 * percentage;
            var remaining = percentage <= 100 ? (double?)(1000 - used) : null;
            var projectedWindow = SafeProjection.Parse(WithFirstNumbers(json, used, 1000, remaining, percentage), Now).Windows[0];
            Check(projectedWindow.UsedPercent == percentage && projectedWindow.Used == used && projectedWindow.Remaining == remaining,
                  "normalized percentage points and counts preserve units and overage");
            Check(projectedWindow.RemainingPercent == Math.Max(0, 100 - percentage),
                  "overage has no negative remaining percentage");
        }
        Fails(() => SafeProjection.Parse(WithFirstNumbers(json, 12.5, 1000, 987.5, 0.0125), Now), "invalid_response");
        Fails(() => SafeProjection.Parse(WithFirstNumbers(json, 1250, 1000, null, 1.25), Now), "invalid_response");
        Fails(() => SafeProjection.Parse(WithFirstNumbers(json, 12.5, 1000, 987.5, 125), Now), "invalid_response");
        var nullLimits = SafeProjection.Parse(json.Replace("\"limit\":1000,", "\"limit\":null,"), Now);
        Check(nullLimits.Windows[0].Limit is null && nullLimits.Windows[0].Unit is null, "unknown core limit remains null");
        var nullUsage = SafeProjection.Parse(json.Replace("\"usedPercent\":25", "\"usedPercent\":null"), Now);
        Check(nullUsage.Windows[0].UsedPercent is null && nullUsage.Windows[0].RemainingPercent is null, "unknown percentages remain null");
    }

    private static void RunCreditPackTests()
    {
        // Same semantic layout as the safe live helper DTO, with synthetic
        // values/keys and fixture timestamps; no real account or upstream ID.
        var fixture = CreditPackFixture();
        var sample = SafeProjection.Parse(fixture.ToJsonString(), Now);
        Check(sample.Windows.Count == 6 && sample.Plan == "pro" && sample.Windows[0].Key == "monthly",
              "monthly subscription three individual packs and listed count accepted");
        Check(sample.Windows.Skip(2).Take(3).Select(window => window.Label)
                  .SequenceEqual(new[] { "Additional credit pack 1", "Additional credit pack 2", "Additional credit pack 3" }),
              "pack labels are fixed indexed constants");
        Check(sample.Windows.Skip(2).Take(3).All(window => window.Kind == "balance" && window.Unit == "credits" &&
                  window.ResetAt is null && window.ExpiresAt == "2026-10-08T16:00:00Z"),
              "every pack retains expiry separate from reset");
        Check(sample.Windows[1].ExpiresAt == "2026-11-01T00:00:00Z" && sample.Windows[1].ResetAt is null &&
              sample.Windows[0].ExpiresAt is null && sample.Windows[0].ResetAt == "2026-11-01T00:00:00Z",
              "subscription expiry and monthly reset remain distinct");
        Check(sample.Windows[5].Key == "addon-listed-packs" && sample.Windows[5].Label == "Listed active credit packs" &&
              sample.Windows[5].Unit == "packs" && sample.Windows[5].Kind == "balance" &&
              sample.Windows[5].Remaining == 3 && sample.Windows[5].ResetAt is null && sample.Windows[5].ExpiresAt is null,
              "listed pack count has fixed balance semantics");
        Check(!JsonSerializer.Serialize(sample, Program.JsonOptions).Contains(Sentinel),
              "dynamic packs do not leak raw labels identifiers or metadata");
        foreach (var key in new[] { "addon-pack-00000000000", "addon-pack-0000000000000", "addon-pack-ABCDEF123456",
                                    "addon-pack-00000000000g", "addon-pack-000000000001\n", "x-addon-pack-000000000001", Sentinel })
        {
            var malicious = fixture.DeepClone();
            malicious["windows"]![2]!["key"] = key;
            Fails(() => SafeProjection.Parse(malicious.ToJsonString(), Now), "invalid_response");
        }
        var duplicate = fixture.DeepClone();
        duplicate["windows"]![3]!["key"] = "addon-pack-000000000001";
        Fails(() => SafeProjection.Parse(duplicate.ToJsonString(), Now), "invalid_response");
        var invalidExpiry = fixture.DeepClone();
        invalidExpiry["windows"]![2]!["expiresAt"] = "2026-10-08T16:00:00";
        Check(SafeProjection.Parse(invalidExpiry.ToJsonString(), Now).Windows[2].ExpiresAt is null,
              "timezone-free pack expiry remains unknown");
        var expiryOnly = fixture.DeepClone();
        foreach (var key in new[] { "used", "limit", "remaining", "usedPercent" })
            expiryOnly["windows"]![2]![key] = null;
        var expirySample = SafeProjection.Parse(expiryOnly.ToJsonString(), Now).Windows[2];
        Check(expirySample.ExpiresAt is not null && expirySample.Limit is null && expirySample.UsedPercent is null,
              "authoritative pack expiry survives unknown balances");
        var contradictory = fixture.DeepClone();
        contradictory["windows"]![3]!["remaining"] = 999;
        Fails(() => SafeProjection.Parse(contradictory.ToJsonString(), Now), "invalid_response");
        var maximum = SafeProjection.Parse(BoundedPackFixture(100, includeStatic: true).ToJsonString(), Now);
        Check(maximum.Windows.Count == 107 && maximum.Windows.Last().Label == "Additional credit pack 100",
              "all seven static windows and 100 packs fit the exact total bound");
        Fails(() => SafeProjection.Parse(BoundedPackFixture(101, includeStatic: true).ToJsonString(), Now), "invalid_response");
        Fails(() => SafeProjection.Parse(BoundedPackFixture(101, includeStatic: false).ToJsonString(), Now), "invalid_response");
        Fails(() => SafeProjection.Parse(fixture.ToJsonString(), Now.AddHours(1)), "invalid_response");
    }

    private static JsonNode CreditPackFixture()
    {
        var root = JsonNode.Parse(SampleJson())!;
        var original = root["windows"]!.AsArray();
        root["windows"] = new JsonArray(original[2]!.DeepClone(), original[3]!.DeepClone(),
            CreditPack(1, 0), CreditPack(2, 130), CreditPack(3, 200), ListedPacks(3));
        return root;
    }

    private static JsonNode BoundedPackFixture(int count, bool includeStatic)
    {
        var root = JsonNode.Parse(SampleJson())!;
        var windows = includeStatic ? root["windows"]!.AsArray() : new JsonArray();
        if (!includeStatic)
            root["windows"] = windows;
        else
            windows.Add(ListedPacks(count));
        for (var index = 1; index <= count; index++)
            windows.Add(CreditPack(index, 130));
        return root;
    }

    private static JsonNode CreditPack(int index, double used) => new JsonObject
    {
        ["key"] = "addon-pack-" + index.ToString("x12"), ["label"] = Sentinel,
        ["usedPercent"] = used / 2, ["used"] = used, ["limit"] = 200, ["remaining"] = 200 - used,
        ["resetAt"] = "2026-10-09T00:00:00Z", ["expiresAt"] = "2026-10-08T16:00:00Z",
        ["unit"] = Sentinel, ["kind"] = Sentinel, ["upstreamId"] = Sentinel
    };

    private static JsonNode ListedPacks(int count) => new JsonObject
    {
        ["key"] = "addon-listed-packs", ["label"] = Sentinel, ["remaining"] = count,
        ["resetAt"] = "2026-10-09T00:00:00Z", ["expiresAt"] = "2026-10-08T16:00:00Z",
        ["unit"] = Sentinel, ["kind"] = Sentinel
    };

    private static object Cookie(string name = "login_qwencloud_ticket", string value = Sentinel,
        string domain = ".qwencloud.com", string path = "/", double? expiration = null) => expiration is null
        ? new { name, value, domain, path, secure = true }
        : (object)new { name, value, domain, path, secure = true, expirationDate = expiration.Value };
    private static string RequestJson(object[] cookies, string region = "intl") =>
        JsonSerializer.Serialize(new { action = "collect", region, cookies });
    private static CollectRequest ValidRequest(string name = "login_qwencloud_ticket", string value = Sentinel,
        string domain = ".qwencloud.com", string path = "/", double? expiration = null) =>
        Parse(RequestJson([Cookie(name, value, domain, path, expiration)]));
    private static CollectRequest Parse(string json) => Protocol.ParseRequest(Encoding.UTF8.GetBytes(json));
    private static CollectRequest LargeRequest() => Parse(RequestJson([
        Cookie(name: "a", value: new string('&', 16381)), Cookie(name: "b", value: new string('&', 16381))]));

    private static string SampleJson() => JsonSerializer.Serialize(new
    {
        id = Sentinel, provider = "qwen", platform = "windows", status = "ok", plan = "pro",
        label = Sentinel, source = Sentinel, message = Sentinel, email = Sentinel,
        sampledAt = "2026-10-01T01:00:00Z", fetchedAt = "2026-10-01T01:00:00Z", token = Sentinel,
        windows = new object[]
        {
            new { key = "5h", label = Sentinel, usedPercent = (double?)25, used = (double?)250,
                limit = (double?)1000, remaining = (double?)750, resetAt = "2026-10-01T05:00:00Z", expiresAt = (string?)null },
            new { key = "weekly", label = Sentinel, usedPercent = (double?)50, used = (double?)5000,
                limit = (double?)10000, remaining = (double?)5000, resetAt = "2026-10-08T01:00:00Z", expiresAt = (string?)null },
            new { key = "monthly", label = Sentinel, usedPercent = (double?)75, used = (double?)75000,
                limit = (double?)100000, remaining = (double?)25000, resetAt = "2026-11-01T00:00:00Z", expiresAt = (string?)null },
            new { key = "subscription", label = Sentinel, usedPercent = (double?)null, used = (double?)null,
                limit = (double?)null, remaining = (double?)null, resetAt = "2026-10-01T05:00:00Z", expiresAt = "2026-11-01T00:00:00Z" },
            new { key = "addon-credits", label = Sentinel, usedPercent = (double?)20, used = (double?)200,
                limit = (double?)1000, remaining = (double?)800, resetAt = (string?)null, expiresAt = (string?)null },
            new { key = "addon-packs", label = Sentinel, usedPercent = (double?)null, used = (double?)null,
                limit = (double?)null, remaining = (double?)2, resetAt = (string?)null, expiresAt = (string?)null }
        }
    });

    private static string WithFirstNumbers(string json, double? used, double? limit, double? remaining, double? percent)
    {
        var root = JsonNode.Parse(json)!;
        var window = root["windows"]![0]!;
        window["used"] = JsonValue.Create(used);
        window["limit"] = JsonValue.Create(limit);
        window["remaining"] = JsonValue.Create(remaining);
        window["usedPercent"] = JsonValue.Create(percent);
        return root.ToJsonString();
    }

    private static void Check(bool condition, string name)
    {
        _ = name; // Test descriptions are constants; no payload diagnostics.
        if (!condition)
            throw new InvalidOperationException();
        passed++;
    }
    private static void Fails(Action action, string code)
    {
        try { action(); }
        catch (SafeFailure failure) when (failure.Code == code) { passed++; return; }
        throw new InvalidOperationException();
    }
}
