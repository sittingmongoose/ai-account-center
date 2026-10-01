using System.Diagnostics;
using System.Text;
using System.Text.Json;

namespace CCS.QwenUsageBridge;

internal static class FixedCollector
{
    internal const string Python = @"C:\Program Files\Python313\python.exe";
    private const int MaxOutputChars = 65536;

    internal static async Task<UsageSample> CollectAsync()
    {
        var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        var directory = Path.Combine(home, ".ccs", "account-usage");
        var script = Path.Combine(directory, "plan_usage.py");
        if (!Path.IsPathFullyQualified(home) || !File.Exists(Python) || !File.Exists(script))
            throw new SafeFailure("collector_unavailable");
        var start = new ProcessStartInfo
        {
            FileName = Python, WorkingDirectory = directory,
            UseShellExecute = false, CreateNoWindow = true,
            RedirectStandardOutput = true, RedirectStandardError = true,
            StandardOutputEncoding = new UTF8Encoding(false, true),
            StandardErrorEncoding = Encoding.UTF8
        };
        // Ignore PYTHONPATH and user-site packages. The fixed script directory
        // remains available for the collector's bundled plan_common import.
        foreach (var argument in new[] { "-E", "-s", "-X", "utf8", script,
                                         "--provider", "qwen", "--platform", "windows" })
            start.ArgumentList.Add(argument);
        using var process = new Process { StartInfo = start };
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(60));
        try
        {
            if (!process.Start())
                throw new SafeFailure("collector_unavailable");
            var stdout = ReadBoundedAsync(process.StandardOutput, capture: true, deadline.Token);
            var stderr = ReadBoundedAsync(process.StandardError, capture: false, deadline.Token);
            // When either pipe exceeds the bound, cancel/kill without waiting
            // for the other pipe or process to finish filling a pipe buffer.
            _ = stdout.ContinueWith(_ => deadline.Cancel(), CancellationToken.None,
                TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
            _ = stderr.ContinueWith(_ => deadline.Cancel(), CancellationToken.None,
                TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
            var combined = Task.WhenAll(stdout, stderr);
            var exited = process.WaitForExitAsync(deadline.Token);
            var first = await Task.WhenAny(combined, exited);
            if (first == combined)
                await combined;
            else
                await exited;
            await combined;
            await exited;
            if (process.ExitCode != 0)
                throw new SafeFailure("collector_error");
            return SafeProjection.Parse(await stdout, DateTimeOffset.UtcNow);
        }
        catch (SafeFailure) { throw; }
        catch (OperationCanceledException) { throw new SafeFailure("timeout"); }
        catch { throw new SafeFailure("collector_error"); }
        finally
        {
            try
            {
                if (!process.HasExited)
                    process.Kill(entireProcessTree: true);
            }
            catch { }
        }
    }

    private static async Task<string> ReadBoundedAsync(StreamReader reader, bool capture, CancellationToken cancellation)
    {
        var result = capture ? new StringBuilder() : null;
        var buffer = new char[4096];
        var total = 0;
        while (true)
        {
            var count = await reader.ReadAsync(buffer.AsMemory(), cancellation);
            if (count == 0)
                break;
            total += count;
            if (total > MaxOutputChars)
                throw new SafeFailure("invalid_response");
            result?.Append(buffer, 0, count);
        }
        return result?.ToString() ?? string.Empty;
    }
}
