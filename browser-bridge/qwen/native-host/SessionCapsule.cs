using System.Runtime.InteropServices;
using System.Runtime.Versioning;
using System.Security.AccessControl;
using System.Security.Cryptography;
using System.Security.Principal;
using System.Text.Encodings.Web;
using System.Text.Json;

namespace CCS.QwenUsageBridge;

internal sealed record ScopedCookies(string ConsoleCookie, string GatewayCookie);

internal static class SessionCapsule
{
    internal const int MaxPlaintextBytes = 70000;
    private static readonly JsonSerializerOptions PrivateJsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        // This JSON is immediately DPAPI-encrypted, never embedded in HTML or
        // returned to the browser. Avoid expanding valid ASCII cookie octets.
        Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping
    };

    internal static ScopedCookies CookieHeaders(CollectRequest request, DateTimeOffset now)
    {
        var targets = request.Region switch
        {
            "intl" => new[] { new Uri("https://home.qwencloud.com/tool/user/info.json"),
                              new Uri("https://cs-data.qwencloud.com/data/api.json") },
            "cn" => new[] { new Uri("https://bailian.console.aliyun.com/cn-beijing?tab=plan"),
                            new Uri("https://bailian-cs.console.aliyun.com/data/api.json") },
            _ => throw new SafeFailure("invalid_request")
        };
        // Each request keeps its own browser cookie domain/path boundary.
        // Console authentication may be host-only; the gateway can authorize
        // the subsequent read with SEC_TOKEN and no gateway cookie at all.
        var console = Protocol.CookieHeader(Protocol.CookiesFor(request, targets[0], now));
        var gateway = Protocol.CookieHeader(Protocol.CookiesFor(request, targets[1], now));
        if (string.IsNullOrEmpty(console))
            throw new SafeFailure("needs_sign_in");
        // Cookie names differ between deployments; only the provider decides
        // whether the existing console session is authenticated.
        return new ScopedCookies(console, gateway);
    }

    internal static byte[] Plaintext(CollectRequest request, DateTimeOffset now)
    {
        var bytes = JsonSerializer.SerializeToUtf8Bytes(CookieHeaders(request, now), PrivateJsonOptions);
        if (bytes.Length > MaxPlaintextBytes)
        {
            CryptographicOperations.ZeroMemory(bytes);
            throw new SafeFailure("invalid_request");
        }
        return bytes;
    }

    internal static byte[] Envelope(CollectRequest request, byte[] protectedBytes) =>
        JsonSerializer.SerializeToUtf8Bytes(new
        {
            version = 2,
            region = request.Region,
            cookiesDPAPI = Convert.ToBase64String(protectedBytes)
        });

    [SupportedOSPlatform("windows")]
    internal static async Task WriteAsync(CollectRequest request)
    {
        var plaintext = Plaintext(request, DateTimeOffset.UtcNow);
        byte[]? protectedBytes = null;
        string? temporary = null;
        try
        {
            protectedBytes = UserDpapi.Protect(plaintext);
            var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
            if (string.IsNullOrWhiteSpace(home) || !Path.IsPathFullyQualified(home))
                throw new SafeFailure("cache_write_error");
            var directory = Path.Combine(home, ".ccs", "account-usage");
            Directory.CreateDirectory(directory);
            var target = Path.Combine(directory, "qwen-console-session.json");
            temporary = Path.Combine(directory, $".qwen-console-session-{Guid.NewGuid():N}.tmp");
            var capsule = Envelope(request, protectedBytes);
            await PrivateFiles.WriteAsync(temporary, capsule);
            File.Move(temporary, target, overwrite: true);
            temporary = null;
        }
        catch (SafeFailure) { throw; }
        catch { throw new SafeFailure("cache_write_error"); }
        finally
        {
            CryptographicOperations.ZeroMemory(plaintext);
            if (protectedBytes is not null)
                CryptographicOperations.ZeroMemory(protectedBytes);
            if (temporary is not null)
            {
                try { File.Delete(temporary); } catch { }
            }
        }
    }
}

internal static class PrivateFiles
{
    [SupportedOSPlatform("windows")]
    internal static async Task WriteAsync(string destination, byte[] bytes)
    {
        using var identity = WindowsIdentity.GetCurrent();
        var sid = identity.User ?? throw new SafeFailure("cache_write_error");
        var security = new FileSecurity();
        security.SetAccessRuleProtection(isProtected: true, preserveInheritance: false);
        security.SetOwner(sid);
        security.AddAccessRule(new FileSystemAccessRule(sid, FileSystemRights.FullControl, AccessControlType.Allow));
        security.AddAccessRule(new FileSystemAccessRule(new SecurityIdentifier(WellKnownSidType.LocalSystemSid, null),
                                                       FileSystemRights.FullControl, AccessControlType.Allow));
        // Create with the protected ACL atomically, and request a handle with
        // the access-control rights required by Windows. A plain FileAccess.Write
        // handle lacks WRITE_DAC even for its owner.
        await using var file = new FileInfo(destination).Create(FileMode.CreateNew, FileSystemRights.FullControl,
            FileShare.None, 8192, FileOptions.Asynchronous, security);
        await file.WriteAsync(bytes);
        await file.FlushAsync();
        file.Flush(flushToDisk: true);
    }
}

internal static class UserDpapi
{
    [StructLayout(LayoutKind.Sequential)]
    private struct Blob
    {
        public int Size;
        public IntPtr Data;
    }

    [DllImport("crypt32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CryptProtectData(ref Blob input, string? description, IntPtr entropy,
        IntPtr reserved, IntPtr prompt, uint flags, out Blob output);

    [DllImport("crypt32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CryptUnprotectData(ref Blob input, IntPtr description, IntPtr entropy,
        IntPtr reserved, IntPtr prompt, uint flags, out Blob output);

    [DllImport("kernel32.dll")]
    private static extern IntPtr LocalFree(IntPtr memory);

    [SupportedOSPlatform("windows")]
    internal static byte[] Protect(byte[] plaintext) => Transform(plaintext, protect: true);

    [SupportedOSPlatform("windows")]
    internal static byte[] UnprotectFixture(byte[] ciphertext) => Transform(ciphertext, protect: false);

    [SupportedOSPlatform("windows")]
    private static byte[] Transform(byte[] bytes, bool protect)
    {
        var input = new Blob { Size = bytes.Length, Data = Marshal.AllocHGlobal(bytes.Length) };
        var output = new Blob();
        try
        {
            Marshal.Copy(bytes, 0, input.Data, bytes.Length);
            // CRYPTPROTECT_UI_FORBIDDEN=1; no machine-wide flag, no entropy, no
            // prompts, same Windows user only. No credentials are exported.
            var ok = protect
                ? CryptProtectData(ref input, null, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, 1, out output)
                : CryptUnprotectData(ref input, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, 1, out output);
            if (!ok || output.Size is <= 0 or > (128 * 1024) || output.Data == IntPtr.Zero)
                throw new SafeFailure("credential_protection_error");
            var result = new byte[output.Size];
            Marshal.Copy(output.Data, result, 0, result.Length);
            return result;
        }
        finally
        {
            ZeroUnmanaged(input.Data, input.Size);
            Marshal.FreeHGlobal(input.Data);
            if (output.Data != IntPtr.Zero)
            {
                ZeroUnmanaged(output.Data, output.Size);
                LocalFree(output.Data);
            }
        }
    }

    private static void ZeroUnmanaged(IntPtr memory, int size)
    {
        if (memory != IntPtr.Zero && size > 0)
            for (var offset = 0; offset < size; offset++)
                Marshal.WriteByte(memory, offset, 0);
    }
}
