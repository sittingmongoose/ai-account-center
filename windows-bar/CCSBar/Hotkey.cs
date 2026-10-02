using System;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Windows.Interop;

namespace CCSBar;

/// <summary>
/// Optional global shortcut (Ctrl+Alt+A) that opens or hides the panel while the tray runs. Registered on a
/// message-only window, so it works with the panel hidden. When another app already owns the combination,
/// registration fails and Settings says so; nothing is overridden.
/// </summary>
public sealed class Hotkey : IDisposable
{
    public const string Display = "Ctrl+Alt+A";
    private const int Id = 0xAAC1, WmHotkey = 0x0312;
    private const uint ModAlt = 0x1, ModControl = 0x2, ModNoRepeat = 0x4000, KeyA = 0x41;
    private HwndSource? source;
    public bool Registered { get; private set; }
    public event Action? Pressed;

    public bool Register()
    {
        if (Registered) return true;
        source ??= new HwndSource(new HwndSourceParameters("AI Account Center hotkey") { ParentWindow = new IntPtr(-3), WindowStyle = 0 });
        source.AddHook(Hook);
        Registered = RegisterHotKey(source.Handle, Id, ModControl | ModAlt | ModNoRepeat, KeyA);
        if (!Registered) source.RemoveHook(Hook);
        return Registered;
    }

    public void Unregister()
    {
        if (source is null || !Registered) return;
        UnregisterHotKey(source.Handle, Id);
        source.RemoveHook(Hook);
        Registered = false;
    }

    private IntPtr Hook(IntPtr hwnd, int message, IntPtr wParam, IntPtr lParam, ref bool handled)
    {
        if (message == WmHotkey && wParam.ToInt32() == Id) { handled = true; Pressed?.Invoke(); }
        return IntPtr.Zero;
    }

    public void Dispose() { Unregister(); source?.Dispose(); }

    [DllImport("user32.dll", SetLastError = true)] private static extern bool RegisterHotKey(IntPtr hwnd, int id, uint modifiers, uint key);
    [DllImport("user32.dll", SetLastError = true)] private static extern bool UnregisterHotKey(IntPtr hwnd, int id);
}

/// <summary>
/// "Start with Windows" reads and toggles the installer's per-user logon task ("AI Account Center"). The task is
/// created by Install.ps1; without it the setting explains how to install instead of inventing a second mechanism.
/// </summary>
public static class StartupTask
{
    public const string Name = "AI Account Center";

    /// <summary>True or false when the task exists; null when it does not or cannot be read.</summary>
    public static bool? Enabled()
    {
        var output = Run("/Query", "/TN", Name, "/XML");
        if (output is null) return null;
        // Only <Settings><Enabled> is the task's state (triggers carry their own); it is omitted when true.
        var settings = output.IndexOf("<Settings>", StringComparison.Ordinal);
        var end = output.IndexOf("</Settings>", StringComparison.Ordinal);
        if (settings < 0 || end < settings) return true;
        var start = output.IndexOf("<Enabled>", settings, end - settings, StringComparison.Ordinal);
        if (start < 0) return true;
        return !output.AsSpan(start + 9).TrimStart().StartsWith("false", StringComparison.OrdinalIgnoreCase);
    }

    public static bool Set(bool enabled) => Run("/Change", "/TN", Name, enabled ? "/ENABLE" : "/DISABLE") is not null;

    private static string? Run(params string[] arguments)
    {
        try
        {
            var info = new ProcessStartInfo(Path.Combine(Environment.SystemDirectory, "schtasks.exe")) { UseShellExecute = false, CreateNoWindow = true, RedirectStandardOutput = true, RedirectStandardError = true };
            foreach (var argument in arguments) info.ArgumentList.Add(argument);
            using var process = Process.Start(info)!;
            var output = process.StandardOutput.ReadToEnd();
            process.StandardError.ReadToEnd();
            if (!process.WaitForExit(10_000)) { try { process.Kill(); } catch { } return null; }
            return process.ExitCode == 0 ? output : null;
        }
        catch { return null; }
    }
}
