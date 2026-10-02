using System;
using System.Collections.Generic;
using System.IO;
using System.Text.Json;
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Animation;
using Microsoft.Win32;

namespace CCSBar;

public enum ThemeMode { Light, Dark, Auto }

/// <summary>Non-secret per-user preferences. Connection credentials stay in the DPAPI store.</summary>
public sealed class Preferences
{
    public string Theme { get; set; } = "auto";
    public bool Hotkey { get; set; } = true;
    /// <summary>When this tray last tried to trade a stored password for a device key (contract section 8: at most once
    /// per launch and every 24 hours). Not a secret.</summary>
    public string? LastPairAttempt { get; set; }

    public static string DefaultPath => Path.Combine(SecureStore.StateDirectory, "preferences.json");
    /// <summary>Where <see cref="Save"/> writes when no path is given: null is the tray's own file. The pairing checks give
    /// their windows a file in their isolated folder.</summary>
    [System.Text.Json.Serialization.JsonIgnore] internal string? StorePath { get; init; }
    /// <summary>A check's, a render's or the E2E driver's copy without a file of its own (its window does not load the
    /// real connection): Save writes nothing, so running the checks never rewrites the real preferences.</summary>
    [System.Text.Json.Serialization.JsonIgnore] internal bool Detached { get; set; }

    public ThemeMode Mode => Theme switch { "light" => ThemeMode.Light, "dark" => ThemeMode.Dark, _ => ThemeMode.Auto };

    public static Preferences Load(string? path = null)
    {
        try
        {
            var file = path ?? DefaultPath;
            if (!File.Exists(file)) return new Preferences();
            var text = File.ReadAllText(file);
            if (text.Length > 4096) return new Preferences();
            var loaded = JsonSerializer.Deserialize<Preferences>(text, Formatting.Json) ?? new Preferences();
            if (loaded.Theme is not ("light" or "dark" or "auto")) loaded.Theme = "auto";
            return loaded;
        }
        catch { return new Preferences(); }
    }

    public void Save(string? path = null)
    {
        try
        {
            var file = path ?? StorePath ?? (Detached ? null : DefaultPath);
            if (file is null) return;
            Directory.CreateDirectory(Path.GetDirectoryName(file)!);
            var temporary = file + ".tmp-" + Guid.NewGuid().ToString("N");
            File.WriteAllText(temporary, JsonSerializer.Serialize(this, Formatting.Json));
            File.Move(temporary, file, true);
        }
        catch { /* Preferences are a convenience; a failed write never blocks the tray. */ }
    }
}

/// <summary>
/// Daylight Atlas tokens (trays/tray.css) for Light and Dark. Every brush is one shared, unfrozen instance, so a theme
/// change animates each colour in place (300 ms cross-fade) and every element that uses it follows without a rebuild.
/// </summary>
public static class Theme
{
    public static readonly Dictionary<string, (string Light, string Dark)> Tokens = new()
    {
        ["Panel"] = ("#EEF2F5", "#0F161C"), ["Card"] = ("#FBFCFD", "#151E26"), ["Card2"] = ("#F4F7F9", "#19232C"), ["Card3"] = ("#EBEFF3", "#1E2933"),
        ["Ink"] = ("#15202B", "#E6ECF2"), ["Ink2"] = ("#445363", "#B0BCC8"), ["Ink3"] = ("#63717F", "#8794A2"), ["Ink4"] = ("#95A2AE", "#5B6876"),
        ["Rule"] = ("#D8DFE5", "#26333F"), ["Rule2"] = ("#E5EAEE", "#1E2933"), ["RuleStrong"] = ("#B6C1CB", "#3A4958"),
        ["Track"] = ("#E2E7EC", "#24303B"), ["TrackHover"] = ("#D3DAE1", "#2E3B48"), ["TrackTick"] = ("#F8FAFB", "#151E26"),
        ["RowHover"] = ("#F1F5F8", "#1A242E"), ["RowPress"] = ("#E9EEF3", "#202B36"),
        ["Accent"] = ("#2552CC", "#86ABFF"), ["AccentInk"] = ("#FFFFFF", "#0B1424"), ["AccentText"] = ("#2149B8", "#9BBAFF"), ["AccentSoft"] = ("#E7EDFB", "#1A2742"),
        ["Calm"] = ("#23907F", "#46C7B2"), ["CalmA"] = ("#8ED3C6", "#1E7468"), ["Warn"] = ("#C98410", "#F3B743"), ["WarnA"] = ("#F2CD78", "#946311"),
        ["Crit"] = ("#CF4127", "#FF6D50"), ["CritA"] = ("#F3A28A", "#A1331F"), ["Over"] = ("#8C1D48", "#FF5C93"),
        ["CalmText"] = ("#15202B", "#E6ECF2"), ["WarnText"] = ("#9A6100", "#F5C35D"), ["CritText"] = ("#BC3219", "#FF8469"), ["OverText"] = ("#8C1D48", "#FF7AA6"),
        ["GoodText"] = ("#1C7D6E", "#5AD3BF"), ["TipBg"] = ("#15202B", "#E6ECF2"), ["TipFg"] = ("#F2F5F8", "#101820"),
        // color-mix(accent 9%, card), accent at 38% (platter); accent 45% into rule (accent-line buttons); accent 5% (active hover)
        ["Platter"] = ("#E8EDF9", "#1F2B3A"), ["PlatterLine"] = ("#612552CC", "#6186ABFF"), ["AccentLine"] = ("#87A0DA", "#516995"), ["ActiveHover"] = ("#0D2552CC", "#0D86ABFF"),
        ["WarnSoft"] = ("#F7F4EC", "#242928"), ["WarnLine"] = ("#D3C4A5", "#635B40"), ["CritSoft"] = ("#F8F1F0", "#232328"), ["CritLine"] = ("#D6B8B6", "#5C4243"),
        ["Shadow"] = ("#2615202B", "#66000000"),
        // Sign-in screen (trays/tray.css, the sign-in block): contours, elevation figures, scale bar, graticule,
        // the focus halo (accent 24%), the ink on the calm success fill, and the hover and match mixes.
        ["SiCt"] = ("#CDD6DD", "#2B3946"), ["SiCtIdx"] = ("#B6C1CB", "#3A4958"), ["SiElev"] = ("#95A2AE", "#5B6876"),
        ["SiCtOk"] = ("#94A9D7", "#4A6085"), ["SiCtIdxOk"] = ("#6684CC", "#647FB4"),
        ["SiSbPaper"] = ("#FBFCFD", "#151E26"), ["SiSbInk"] = ("#63717F", "#8794A2"),
        ["PgMinor"] = ("#0915202B", "#06E6ECF2"), ["PgMajor"] = ("#1115202B", "#0BE6ECF2"),
        ["SiOnCalm"] = ("#FFFFFF", "#0B1A17"), ["Halo"] = ("#3D2552CC", "#3D86ABFF"), ["HaloBad"] = ("#38CF4127", "#38FF6D50"),
        ["AccentHover"] = ("#234CB9", "#92B3FD"), ["OkMatch"] = ("#4F9F96", "#42A197"),
    };

    private static readonly Dictionary<string, SolidColorBrush> brushes = new();
    private static readonly Dictionary<string, LinearGradientBrush> gradients = new();
    public static bool IsDark { get; private set; }
    public static ThemeMode Mode { get; private set; } = ThemeMode.Auto;
    public static event Action? Changed;
    /// <summary>Handlers on Changed (the render checks assert it stays bounded across refreshes).</summary>
    internal static int SubscriberCount => Changed?.GetInvocationList().Length ?? 0;

    public static SolidColorBrush Brush(string key)
    {
        if (brushes.TryGetValue(key, out var brush)) return brush;
        brush = new SolidColorBrush(Color(key, IsDark));
        brushes[key] = brush;
        return brush;
    }

    /// <summary>One gradient per severity (calm, warn, crit), left (soft) to right (strong), as in the concept.</summary>
    public static LinearGradientBrush Gradient(string severity)
    {
        if (gradients.TryGetValue(severity, out var brush)) return brush;
        var (soft, strong) = SeverityStops(severity);
        brush = new LinearGradientBrush { StartPoint = new Point(0, 0.5), EndPoint = new Point(1, 0.5) };
        brush.GradientStops.Add(new GradientStop(Color(soft, IsDark), 0));
        brush.GradientStops.Add(new GradientStop(Color(strong, IsDark), 1));
        gradients[severity] = brush;
        return brush;
    }

    private static (string Soft, string Strong) SeverityStops(string severity) => severity switch
    {
        "warn" => ("WarnA", "Warn"), "crit" => ("CritA", "Crit"), _ => ("CalmA", "Calm")
    };

    public static Color Color(string key, bool dark)
    {
        var (light, darkHex) = Tokens[key];
        return (Color)ColorConverter.ConvertFromString(dark ? darkHex : light);
    }

    /// <summary>Severity of a used percentage: calm, warn from 80, crit from 95, over past 100; null is unavailable.</summary>
    public static string Severity(double? used) => used is not double v || !double.IsFinite(v) ? "na" : v > 100 ? "over" : v >= 95 ? "crit" : v >= 80 ? "warn" : "calm";

    public static string SeverityText(string severity) => severity switch { "warn" => "WarnText", "crit" => "CritText", "over" => "OverText", _ => "CalmText" };

    public static void Apply(ThemeMode mode, bool animate)
    {
        Mode = mode;
        var dark = mode == ThemeMode.Dark || mode == ThemeMode.Auto && !SystemTheme.AppsUseLightTheme();
        var changed = dark != IsDark;
        IsDark = dark;
        foreach (var (key, brush) in brushes) Retint(brush, Color(key, dark), animate && changed);
        foreach (var (severity, brush) in gradients)
        {
            var (soft, strong) = SeverityStops(severity);
            RetintStop(brush.GradientStops[0], Color(soft, dark), animate && changed);
            RetintStop(brush.GradientStops[1], Color(strong, dark), animate && changed);
        }
        ThemeFlags.Instance.Raise();
        Changed?.Invoke();
    }

    private static void Retint(SolidColorBrush brush, Color target, bool animate)
    {
        if (!animate || !Motion.Enabled) { brush.BeginAnimation(SolidColorBrush.ColorProperty, null); brush.Color = target; return; }
        brush.BeginAnimation(SolidColorBrush.ColorProperty, new ColorAnimation(target, Motion.Duration(300)) { EasingFunction = Motion.InOut });
    }

    private static void RetintStop(GradientStop stop, Color target, bool animate)
    {
        if (!animate || !Motion.Enabled) { stop.BeginAnimation(GradientStop.ColorProperty, null); stop.Color = target; return; }
        stop.BeginAnimation(GradientStop.ColorProperty, new ColorAnimation(target, Motion.Duration(300)) { EasingFunction = Motion.InOut });
    }

    // Instrument Sans (static, tnum frozen into every face) and its SemiCondensed SemiBold numeral face.
    public static readonly FontFamily Sans = new(new Uri("pack://application:,,,/CCSBar;component/"), "./Resources/Fonts/#Instrument Sans");
    public static readonly FontFamily Numerals = new(new Uri("pack://application:,,,/CCSBar;component/"), "./Resources/Fonts/#Instrument Sans SemiCondensed");
    // Martian Mono (static Regular cut at the dashboard's 87.5 width): paths, the setup code and the motif's figures.
    public static readonly FontFamily Mono = new(new Uri("pack://application:,,,/CCSBar;component/"), "./Resources/Fonts/#Martian Mono");
}

/// <summary>Theme state for bindings. WPF listens to it through a weak event manager, so short-lived elements (rows
/// rebuilt on every sample) can follow Light and Dark without a static handler that would keep them alive.</summary>
public sealed class ThemeFlags : System.ComponentModel.INotifyPropertyChanged
{
    public static readonly ThemeFlags Instance = new();
    public Visibility LightOnly => Theme.IsDark ? Visibility.Collapsed : Visibility.Visible;
    public Visibility DarkOnly => Theme.IsDark ? Visibility.Visible : Visibility.Collapsed;
    public event System.ComponentModel.PropertyChangedEventHandler? PropertyChanged;
    internal void Raise() => PropertyChanged?.Invoke(this, new System.ComponentModel.PropertyChangedEventArgs(string.Empty));
}

/// <summary>Windows light/dark settings: the app mode drives Auto, the system (taskbar) mode picks the tray icon.</summary>
public static class SystemTheme
{
    private const string Personalize = @"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

    public static bool AppsUseLightTheme() => ReadFlag("AppsUseLightTheme", true);
    public static bool SystemUsesLightTheme() => ReadFlag("SystemUsesLightTheme", false);

    private static bool ReadFlag(string name, bool fallback)
    {
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(Personalize);
            return key?.GetValue(name) is int value ? value != 0 : fallback;
        }
        catch { return fallback; }
    }
}
