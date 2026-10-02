using System;
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Animation;

namespace CCSBar;

/// <summary>
/// Shared motion tokens (ROUND2 "Motion"): values ease out and never pass their reading; springs (BackEase) are only
/// used on controls such as the toggle knob, the segmented thumb and the gear turn.
/// </summary>
public static class Motion
{
    /// <summary>False for fixture renders and when Windows turns client-area animations off.</summary>
    public static bool Enabled { get; set; } = SystemParameters.ClientAreaAnimation;

    public static readonly IEasingFunction Out = Freeze(new CubicEase { EasingMode = EasingMode.EaseOut });
    public static readonly IEasingFunction In = Freeze(new CubicEase { EasingMode = EasingMode.EaseIn });
    public static readonly IEasingFunction InOut = Freeze(new CubicEase { EasingMode = EasingMode.EaseInOut });
    /// <summary>Controls only (toggle knob, segmented thumb, gear): a small overshoot. Never on a value.</summary>
    public static readonly IEasingFunction Spring = Freeze(new BackEase { EasingMode = EasingMode.EaseOut, Amplitude = 0.45 });

    public const int Fast = 140, Med = 260, Slow = 520, Draw = 900;

    private static IEasingFunction Freeze(EasingFunctionBase easing) { easing.Freeze(); return easing; }

    public static Duration Duration(double milliseconds) => new(TimeSpan.FromMilliseconds(Enabled ? milliseconds : 0));
    public static TimeSpan Delay(double milliseconds) => TimeSpan.FromMilliseconds(Enabled ? milliseconds : 0);

    public static void To(IAnimatable target, DependencyProperty property, double to, double milliseconds, IEasingFunction? easing = null, double delay = 0, double? from = null, EventHandler? completed = null)
    {
        if (!Enabled)
        {
            target.BeginAnimation(property, null);
            if (target is DependencyObject dependency) dependency.SetValue(property, to);
            completed?.Invoke(target, EventArgs.Empty);
            return;
        }
        var animation = new DoubleAnimation(to, Duration(milliseconds)) { EasingFunction = easing ?? Out, BeginTime = Delay(delay) };
        if (from.HasValue) animation.From = from.Value;
        if (completed is not null) animation.Completed += completed;
        target.BeginAnimation(property, animation);
    }

    public static void Color(SolidColorBrush brush, Color to, double milliseconds = Fast)
    {
        if (brush.IsFrozen) return;
        if (!Enabled) { brush.BeginAnimation(SolidColorBrush.ColorProperty, null); brush.Color = to; return; }
        brush.BeginAnimation(SolidColorBrush.ColorProperty, new ColorAnimation(to, Duration(milliseconds)) { EasingFunction = InOut });
    }

    /// <summary>Samples an easing curve; value animations must stay within [0, 1] (no overshoot).</summary>
    public static bool NeverOvershoots(IEasingFunction easing)
    {
        double previous = 0;
        for (int i = 0; i <= 200; i++)
        {
            var value = easing.Ease(i / 200.0);
            if (value < -1e-9 || value > 1 + 1e-9 || value + 1e-9 < previous) return false;
            previous = value;
        }
        return true;
    }
}
