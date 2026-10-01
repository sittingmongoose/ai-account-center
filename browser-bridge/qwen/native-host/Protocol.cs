using System.Buffers.Binary;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;

namespace CCS.QwenUsageBridge;

internal sealed record BrowserCookie(string Name, string Value, string Domain, string Path,
                                     bool Secure, double? ExpirationDate);
internal sealed record CollectRequest(string Region, IReadOnlyList<BrowserCookie> Cookies);

internal static class Protocol
{
    internal const int MaxMessageBytes = 256 * 1024;
    private static readonly HashSet<string> AllowedDomains = new(StringComparer.Ordinal)
    {
        "qwencloud.com", ".qwencloud.com", "home.qwencloud.com", "cs-data.qwencloud.com",
        ".home.qwencloud.com", ".cs-data.qwencloud.com",
        "aliyun.com", ".aliyun.com", "bailian.console.aliyun.com", "bailian-cs.console.aliyun.com",
        ".bailian.console.aliyun.com", ".bailian-cs.console.aliyun.com"
    };
    private static readonly HashSet<string> RequestFields = new(StringComparer.Ordinal)
        { "action", "region", "cookies" };
    private static readonly HashSet<string> CookieFields = new(StringComparer.Ordinal)
        { "name", "value", "domain", "path", "secure", "expirationDate" };

    internal static void ValidateCaller(string[] args)
    {
        if (args.Length is < 1 or > 2 || args[0] != Program.AllowedOrigin ||
            (args.Length == 2 && !Regex.IsMatch(args[1], @"\A--parent-window=[0-9]{1,20}\z",
                                              RegexOptions.CultureInvariant)))
            throw new SafeFailure("invalid_caller");
    }

    internal static async Task<CollectRequest> ReadRequestAsync(Stream stream, CancellationToken cancellation)
    {
        var prefix = new byte[4];
        await ReadExactlyAsync(stream, prefix, cancellation);
        var size = BinaryPrimitives.ReadUInt32LittleEndian(prefix);
        if (size == 0 || size > MaxMessageBytes)
            throw new SafeFailure("invalid_request");
        var bytes = new byte[(int)size];
        try
        {
            await ReadExactlyAsync(stream, bytes, cancellation);
            return ParseRequest(bytes);
        }
        finally
        {
            Array.Clear(bytes);
        }
    }

    private static async Task ReadExactlyAsync(Stream stream, Memory<byte> buffer, CancellationToken cancellation)
    {
        var read = 0;
        while (read < buffer.Length)
        {
            var count = await stream.ReadAsync(buffer[read..], cancellation);
            if (count == 0)
                throw new SafeFailure("invalid_request");
            read += count;
        }
    }

    internal static CollectRequest ParseRequest(ReadOnlyMemory<byte> bytes)
    {
        if (bytes.Length == 0 || bytes.Length > MaxMessageBytes)
            throw new SafeFailure("invalid_request");
        try
        {
            using var document = JsonDocument.Parse(bytes, new JsonDocumentOptions { MaxDepth = 8 });
            var root = document.RootElement;
            CheckFields(root, RequestFields);
            if (RequiredString(root, "action", 16) != "collect")
                throw new SafeFailure("invalid_request");
            var region = RequiredString(root, "region", 4);
            if (region is not ("intl" or "cn"))
                throw new SafeFailure("invalid_request");
            if (!root.TryGetProperty("cookies", out var list) || list.ValueKind != JsonValueKind.Array ||
                list.GetArrayLength() > 200)
                throw new SafeFailure("invalid_request");

            var cookies = new List<BrowserCookie>(list.GetArrayLength());
            foreach (var item in list.EnumerateArray())
            {
                CheckFields(item, CookieFields);
                var name = RequiredString(item, "name", 256);
                var value = RequiredString(item, "value", 16384, allowEmpty: true);
                var domain = RequiredString(item, "domain", 128);
                var path = RequiredString(item, "path", 2048);
                if (!name.All(IsTokenCharacter) || !ValidCookieValue(value) || !AllowedDomains.Contains(domain) ||
                    path[0] != '/' || path.Any(c => c < 0x20 || c == 0x7f) ||
                    !item.TryGetProperty("secure", out var secure) ||
                    secure.ValueKind is not (JsonValueKind.True or JsonValueKind.False))
                    throw new SafeFailure("invalid_request");
                double? expiration = null;
                if (item.TryGetProperty("expirationDate", out var expires))
                {
                    if (expires.ValueKind != JsonValueKind.Number || !expires.TryGetDouble(out var seconds) ||
                        !double.IsFinite(seconds) || seconds < 0)
                        throw new SafeFailure("invalid_request");
                    expiration = seconds;
                }
                cookies.Add(new BrowserCookie(name, value, domain, path, secure.GetBoolean(), expiration));
            }
            return new CollectRequest(region, cookies);
        }
        catch (SafeFailure) { throw; }
        catch { throw new SafeFailure("invalid_request"); }
    }

    private static void CheckFields(JsonElement element, HashSet<string> allowed)
    {
        if (element.ValueKind != JsonValueKind.Object)
            throw new SafeFailure("invalid_request");
        var seen = new HashSet<string>(StringComparer.Ordinal);
        foreach (var property in element.EnumerateObject())
            if (!allowed.Contains(property.Name) || !seen.Add(property.Name))
                throw new SafeFailure("invalid_request");
    }

    private static string RequiredString(JsonElement element, string key, int max, bool allowEmpty = false)
    {
        if (!element.TryGetProperty(key, out var property) || property.ValueKind != JsonValueKind.String)
            throw new SafeFailure("invalid_request");
        var value = property.GetString()!;
        if (value.Length > max || (!allowEmpty && value.Length == 0))
            throw new SafeFailure("invalid_request");
        return value;
    }

    private static bool IsTokenCharacter(char c) =>
        c is >= '0' and <= '9' or >= 'A' and <= 'Z' or >= 'a' and <= 'z' ||
        "!#$%&'*+-.^_`|~".Contains(c);

    private static bool ValidCookieValue(string value)
    {
        // RFC 6265 cookie-octet, optionally quoted. Reject delimiter/header
        // injection even when a browser accepts a more permissive cookie.
        var start = value.StartsWith('"') && value.EndsWith('"') && value.Length >= 2 ? 1 : 0;
        var end = start == 1 ? value.Length - 1 : value.Length;
        for (var index = start; index < end; index++)
        {
            var c = value[index];
            if (!(c == 0x21 || c is >= (char)0x23 and <= (char)0x2b ||
                  c is >= (char)0x2d and <= (char)0x3a || c is >= (char)0x3c and <= (char)0x5b ||
                  c is >= (char)0x5d and <= (char)0x7e))
                return false;
        }
        return true;
    }

    internal static IReadOnlyList<BrowserCookie> CookiesFor(CollectRequest request, Uri target, DateTimeOffset now)
    {
        var unix = now.ToUnixTimeMilliseconds() / 1000.0;
        return request.Cookies.Where(cookie =>
        {
            var domainMatches = cookie.Domain.StartsWith('.')
                ? target.Host == cookie.Domain[1..] || target.Host.EndsWith(cookie.Domain, StringComparison.Ordinal)
                : target.Host == cookie.Domain;
            var pathMatches = target.AbsolutePath == cookie.Path ||
                (target.AbsolutePath.StartsWith(cookie.Path, StringComparison.Ordinal) &&
                 (cookie.Path.EndsWith('/') || target.AbsolutePath.Length > cookie.Path.Length &&
                  target.AbsolutePath[cookie.Path.Length] == '/'));
            return domainMatches && pathMatches && (!cookie.Secure || target.Scheme == "https") &&
                   (cookie.ExpirationDate is null || cookie.ExpirationDate > unix);
        }).OrderByDescending(cookie => cookie.Path.Length).ToArray();
    }

    internal static string CookieHeader(IReadOnlyList<BrowserCookie> cookies)
    {
        var header = string.Join("; ", cookies.Select(cookie => $"{cookie.Name}={cookie.Value}"));
        if (header.Length > 32768)
            throw new SafeFailure("invalid_request");
        return header;
    }

    internal static async Task WriteReplyAsync(Stream stream, NativeReply reply)
    {
        var body = JsonSerializer.SerializeToUtf8Bytes(reply, Program.JsonOptions);
        var prefix = new byte[4];
        BinaryPrimitives.WriteUInt32LittleEndian(prefix, checked((uint)body.Length));
        await stream.WriteAsync(prefix);
        await stream.WriteAsync(body);
        await stream.FlushAsync();
    }
}
