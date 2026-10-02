using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;

namespace CCSBar;

public sealed class CodexSwitchProcess
{
    public string Label { get; set; } = "";
    public int Pid { get; set; }
    public string Role { get; set; } = "";
}

public sealed class CodexSwitchConfirmation
{
    public string Token { get; set; } = "";
    public string ExpiresAt { get; set; } = "";
    public string TargetProfile { get; set; } = "";
    public List<CodexSwitchProcess> Processes { get; set; } = new();
    public string Warning { get; set; } = "";

    public bool IsValidFor(string profile) => TargetProfile == profile && Formatting.IsSafeProfile(profile)
        && Token is { Length: > 0 and <= 4096 } && !Token.Any(char.IsControl)
        && DateTimeOffset.TryParse(ExpiresAt, out _) && Processes is { Count: > 0 and <= 128 }
        && Processes.All(process => process is not null && process.Pid > 0 && process.Label is { Length: > 0 and <= 300 } && process.Role is { Length: <= 100 });
    public bool Expired => !DateTimeOffset.TryParse(ExpiresAt, out var expires) || expires <= DateTimeOffset.UtcNow;
}

public sealed class CodexConfirmationRequiredException : InvalidOperationException
{
    public CodexSwitchConfirmation Confirmation { get; }
    public CodexConfirmationRequiredException(CodexSwitchConfirmation confirmation) : base("Running Codex programs need your confirmation before switching.") => Confirmation = confirmation;
}

public static class CodexSwitchFlow
{
    // Cancellation never sends a confirmation token. Expired or changed proposals
    // require a fresh Activate gesture; there is no automatic confirmation retry.
    public static async Task<bool> Run(string profile, Func<string?, Task> activate,
        Func<CodexSwitchConfirmation, Task<bool>> approve)
    {
        try { await activate(null); return true; }
        catch (CodexConfirmationRequiredException error)
        {
            var proposal = error.Confirmation;
            if (!proposal.IsValidFor(profile)) throw new InvalidOperationException("The switch confirmation is invalid. Refresh and try again.");
            if (!await approve(proposal)) return false;
            if (proposal.Expired) throw new InvalidOperationException("The confirmation expired. Activate again to review the current programs.");
            await activate(proposal.Token);
            return true;
        }
    }
}

/// <summary>The server's Antigravity switch offer (antigravity-routes.ts activationResponse), validated before display.</summary>
public sealed class AntigravityConfirmation
{
    public string Token { get; set; } = "";
    public string ExpiresAt { get; set; } = "";
    public string ProfileId { get; set; } = "";
    public string HostId { get; set; } = "";
    public string? Email { get; set; }
    public string Warning { get; set; } = "";
    public List<CodexSwitchProcess> Processes { get; set; } = new();

    public static bool IsToken(string? token) => token is { Length: >= 16 and <= 256 } && token.All(c => char.IsAsciiLetterOrDigit(c) || c is '_' or '-');

    public bool IsValidFor(string profileId) => ProfileId == profileId && Formatting.IsSafeId(profileId) && HostId == "ubuntu" && IsToken(Token)
        && DateTimeOffset.TryParse(ExpiresAt, out _) && Processes is { Count: <= 32 }
        && Processes.All(process => process is not null && process.Pid > 0 && process.Label is { Length: > 0 and <= 300 } && process.Role is { Length: <= 100 });
    public bool Expired => !DateTimeOffset.TryParse(ExpiresAt, out var expires) || expires <= DateTimeOffset.UtcNow;
}

public sealed class AntigravityConfirmationRequiredException : InvalidOperationException
{
    public AntigravityConfirmation Confirmation { get; }
    public AntigravityConfirmationRequiredException(AntigravityConfirmation confirmation) : base("Running Antigravity programs need your confirmation before switching.") => Confirmation = confirmation;
}

/// <summary>Same rules as Codex: Cancel never sends the token; an expired or changed offer needs a fresh Activate.</summary>
public static class AntigravitySwitchFlow
{
    public static async Task<bool> Run(string profileId, Func<string?, Task> activate, Func<AntigravityConfirmation, Task<bool>> approve)
    {
        try { await activate(null); return true; }
        catch (AntigravityConfirmationRequiredException error)
        {
            var proposal = error.Confirmation;
            if (!proposal.IsValidFor(profileId)) throw new InvalidOperationException("The switch confirmation is invalid. Refresh and try again.");
            if (!await approve(proposal)) return false;
            if (proposal.Expired) throw new InvalidOperationException("The confirmation expired. Activate again to review the current programs.");
            await activate(proposal.Token);
            return true;
        }
    }
}
