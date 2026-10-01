using System.Text.Json;
using System.Runtime.Versioning;

namespace CCS.QwenUsageBridge;

internal static class Program
{
    internal const string AllowedOrigin = "chrome-extension://clobbdmblhillanldmmjnlpbaafbnklj/";
    internal static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        MaxDepth = 20
    };

    private static async Task<int> Main(string[] args)
    {
        if (args.Length == 1 && args[0] == "--self-test")
            return SelfTests.Run();

        NativeReply reply;
        var collectExisting = false;
        try
        {
            collectExisting = ExistingCollectionMode(args);
            CollectRequest? request = null;
            if (!collectExisting)
            {
                Protocol.ValidateCaller(args);
                using var inputTimeout = new CancellationTokenSource(TimeSpan.FromSeconds(15));
                request = await Protocol.ReadRequestAsync(Console.OpenStandardInput(), inputTimeout.Token);
            }
            if (!OperatingSystem.IsWindows())
                throw new SafeFailure("unsupported_platform");

            // A mutex is thread-owned. Keep this complete transaction on one
            // thread while its async I/O finishes; native hosts have no UI.
            using var lease = CollectionLease.Acquire();
            // The fixed diagnostic CLI reads the existing same-user capsule;
            // only the browser collection path can replace its encrypted jar.
            if (request is not null)
                SessionCapsule.WriteAsync(request).GetAwaiter().GetResult();
            var sample = FixedCollector.CollectAsync().GetAwaiter().GetResult();
            SampleCache.WriteAsync(sample).GetAwaiter().GetResult();
            reply = new NativeReply(true, sample, null);
        }
        catch (SafeFailure failure)
        {
            reply = new NativeReply(false, null, failure.Code);
        }
        catch (OperationCanceledException)
        {
            reply = new NativeReply(false, null, "timeout");
        }
        catch
        {
            // Upstream exceptions can contain tokens, URLs or response bodies.
            // Never write exception messages, stack traces or upstream payloads.
            reply = new NativeReply(false, null, "unavailable");
        }

        try
        {
            if (collectExisting)
                await Console.Out.WriteLineAsync(JsonSerializer.Serialize(reply, JsonOptions));
            else
                await Protocol.WriteReplyAsync(Console.OpenStandardOutput(), reply);
            return reply.Ok ? 0 : 1;
        }
        catch
        {
            // A closed browser pipe is not a reason to log the response.
            return 1;
        }
    }

    internal static bool ExistingCollectionMode(string[] args)
    {
        if (args.Length == 1 && args[0] == "--collect-existing")
            return true;
        if (args.Any(argument => argument.StartsWith("--collect-existing", StringComparison.Ordinal)))
            throw new SafeFailure("invalid_request");
        return false;
    }
}

internal sealed class SafeFailure(string code) : Exception
{
    internal string Code { get; } = code;
}

internal sealed record NativeReply(bool Ok, UsageSample? Sample, string? Error);

internal static class SampleCache
{
    [SupportedOSPlatform("windows")]
    internal static async Task WriteAsync(UsageSample sample)
    {
        var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        if (string.IsNullOrWhiteSpace(home) || !Path.IsPathFullyQualified(home))
            throw new SafeFailure("cache_write_error");
        var directory = Path.Combine(home, ".ccs", "account-usage");
        var destination = Path.Combine(directory, "qwen-browser-usage.json");
        string? temporary = null;
        try
        {
            Directory.CreateDirectory(directory);
            temporary = Path.Combine(directory, $".qwen-browser-usage-{Guid.NewGuid():N}.tmp");
            // Only this explicit normalized DTO is persisted, never cookies or
            // SEC_TOKEN, headers, upstream JSON, account identifiers or errors.
            var bytes = JsonSerializer.SerializeToUtf8Bytes(sample, Program.JsonOptions);
            await PrivateFiles.WriteAsync(temporary, bytes);
            File.Move(temporary, destination, overwrite: true);
            temporary = null;
        }
        catch
        {
            throw new SafeFailure("cache_write_error");
        }
        finally
        {
            if (temporary is not null)
            {
                try { File.Delete(temporary); } catch { }
            }
        }
    }
}
