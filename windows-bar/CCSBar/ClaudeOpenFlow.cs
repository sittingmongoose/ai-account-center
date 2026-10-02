using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;

namespace CCSBar;

/// <summary>What the Open POST answered: today's 200, or the opt-in 202 whose operation the tray then read-polls.</summary>
public readonly record struct ClaudeOpenStart(bool Accepted, string OperationId);

/// <summary>One Open's calm inline progress, as the account row's secondary line shows it.</summary>
public sealed record ClaudeOpenProgress(string Platform, string Text, bool Finished, bool Opened)
{
    public bool Running => !Finished;
}

/// <summary>How often the profile list is read while an Open runs: 1 s for the first two minutes, then every 5 s,
/// with a hard stop at three minutes (CONTRACT-serving-misc 4.4). The checks shorten every value.</summary>
public sealed record ClaudeOpenPolling
{
    public TimeSpan FastInterval { get; init; } = TimeSpan.FromSeconds(1);
    public TimeSpan SlowInterval { get; init; } = TimeSpan.FromSeconds(5);
    public TimeSpan SlowAfter { get; init; } = TimeSpan.FromSeconds(120);
    public TimeSpan Timeout { get; init; } = TimeSpan.FromSeconds(180);
}

/// <summary>One account's Open at a time. A second start for the same Claude profile is refused while the first still
/// runs, so the POST is never repeated and the row's actions stay disabled during the poll.</summary>
public sealed class ClaudeOpenCoordinator
{
    private readonly HashSet<string> running = new(StringComparer.Ordinal);

    public bool TryStart(string profile) { lock (running) return running.Add(profile); }
    public void Finish(string profile) { lock (running) running.Remove(profile); }
    public bool IsRunning(string profile) { lock (running) return running.Contains(profile); }
}

/// <summary>
/// Claude "Open on Mac" and "Open on Windows": the POST, then (on 202) a read-poll of the profile list. The POST is
/// sent once and is never replayed, and a poll is never resumed after a restart: this flow lives in memory only.
/// Windows opens go through here too, so the local ccs-claude:// URI is never started and a guarded history copy can
/// never be bypassed.
/// </summary>
public static class ClaudeOpenFlow
{
    /// <summary>The 409 history_unconfirmed refusal, and the fallback for a terminal operation with no usable
    /// server message.</summary>
    public const string HistoryUnconfirmed = "History copy could not be confirmed. Claude was not opened.";
    /// <summary>Said when the three-minute poll ends without a terminal state.</summary>
    public const string StillWorking = "Still working on the dashboard. Check again shortly.";
    /// <summary>The row's text while the POST itself is in flight.</summary>
    public const string Starting = "Opening";
    /// <summary>Windows never opens the URI itself, so an unreachable dashboard says so instead.</summary>
    public const string Unreachable = "Can't reach the dashboard. Try again.";

    /// <summary>The row's calm secondary text for one reported operation, or null for a state this tray does not know
    /// (the poll then keeps the text it already shows and keeps running).</summary>
    public static string? TextFor(ClaudeOpenOperation operation) => operation.State switch
    {
        "checking" => "Copying history",
        "copying" => operation.ConfirmedCount is int confirmed && operation.TotalCount is int total
            ? $"Copying history {confirmed} of {total}"
            : "Copying history",
        "opening" => "Opening",
        "opened" => "Opened",
        "failed" or "blocked_uncertain" => PublicMessage(operation.Message),
        _ => null
    };

    /// <summary>The server's own fixed sentence, or the client's fallback. The contract promises a fixed sentence, so a
    /// long or multiline value is not shown: server strings never reach the screen unbounded.</summary>
    public static string PublicMessage(string? message) =>
        !string.IsNullOrEmpty(message) && message!.Length <= 300 && message.IndexOf('\n') < 0 && message.IndexOf('\r') < 0
            ? message
            : HistoryUnconfirmed;

    /// <summary>Runs one Open. <paramref name="progress"/> reports the row's text at every change, including the end.
    /// Returns null when another Open for the same account is still running (nothing at all is sent), otherwise the
    /// final progress. Errors from the POST are thrown unchanged, so the caller's existing error handling applies.</summary>
    public static async Task<ClaudeOpenProgress?> Run(DashboardClient client, string profile, string platform,
        ClaudeOpenCoordinator coordinator, Func<ClaudeOpenProgress, Task> progress,
        ClaudeOpenPolling? polling = null, Func<DateTimeOffset>? now = null, Func<TimeSpan, Task>? sleep = null)
    {
        var plan = polling ?? new ClaudeOpenPolling();
        var clock = now ?? (() => DateTimeOffset.UtcNow);
        var wait = sleep ?? (async delay => await Task.Delay(delay));
        if (!coordinator.TryStart(profile)) return null;
        try
        {
            await progress(new ClaudeOpenProgress(platform, Starting, false, false));
            // A POST that fails is thrown unchanged: the caller rests the row and shows the error it already shows.
            var start = await client.OpenClaude(profile, platform);
            if (!start.Accepted)
            {
                var done = new ClaudeOpenProgress(platform, "Opened", true, true);
                await progress(done);
                return done;
            }
            var began = clock();
            string? shown = null;
            while (true)
            {
                var elapsed = clock() - began;
                if (elapsed >= plan.Timeout)
                {
                    var gaveUp = new ClaudeOpenProgress(platform, StillWorking, true, false);
                    await progress(gaveUp);
                    return gaveUp;
                }
                await wait(elapsed < plan.SlowAfter ? plan.FastInterval : plan.SlowInterval);
                IReadOnlyList<ClaudeDesktopProfile> profiles;
                // A read that fails never ends the Open: the server keeps the operation and the POST is never
                // replayed, so the poll simply tries again until the deadline.
                try { profiles = await client.ClaudeDesktopProfiles(); }
                catch { continue; }
                var operation = profiles.Where(candidate => candidate.Id == profile).Select(candidate => candidate.OpenOperation)
                    .FirstOrDefault(candidate => candidate is not null && candidate.Platform == platform
                        && (start.OperationId.Length == 0 || candidate.Id == start.OperationId));
                if (operation is null) continue;
                if (operation.IsTerminal)
                {
                    var ended = new ClaudeOpenProgress(platform, TextFor(operation) ?? HistoryUnconfirmed, true, operation.IsOpened);
                    await progress(ended);
                    return ended;
                }
                var text = TextFor(operation);
                if (text is not null && text != shown)
                {
                    shown = text;
                    await progress(new ClaudeOpenProgress(platform, text, false, false));
                }
            }
        }
        finally { coordinator.Finish(profile); }
    }
}
