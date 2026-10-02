using System;
using System.Collections.Generic;
using System.Linq;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Media.Imaging;
using System.Windows.Shapes;

namespace CCSBar;

/// <summary>Small builders shared by the panel, Settings and the fixture renderer.</summary>
public static class Ui
{
    public static TextBlock Text(string text, double size, string ink = "Ink", FontWeight? weight = null, bool wrap = false, bool trim = false) => new()
    {
        Text = text, FontSize = size, Foreground = Theme.Brush(ink), FontWeight = weight ?? FontWeights.Normal,
        TextWrapping = wrap ? TextWrapping.Wrap : TextWrapping.NoWrap, TextTrimming = trim ? TextTrimming.CharacterEllipsis : TextTrimming.None,
    };

    public static ToolTip Tip(string text) => new() { Content = new TextBlock { Text = text, TextWrapping = TextWrapping.Wrap } };

    private static readonly Dictionary<string, BitmapImage> marks = new();
    private static readonly Dictionary<string, double> markScale = new()
    {
        ["claude"] = 0.93, ["codex"] = 0.95, ["antigravity"] = 0.97, ["cursor"] = 0.89, ["muse"] = 1, ["kimi-code"] = 0.97, ["qwen"] = 0.93, ["zai"] = 0.92, ["opencode-go"] = 0.84,
    };
    private static readonly HashSet<string> lightVariants = new() { "codex", "cursor", "qwen", "zai", "opencode-go" };
    public static readonly HashSet<string> KnownMarks = new(markScale.Keys);

    private static BitmapImage? MarkImage(string file)
    {
        if (marks.TryGetValue(file, out var image)) return image;
        try
        {
            image = new BitmapImage();
            image.BeginInit();
            image.UriSource = new Uri($"pack://application:,,,/CCSBar;component/Resources/Providers/{file}.png");
            image.CacheOption = BitmapCacheOption.OnLoad;
            image.EndInit(); image.Freeze();
            marks[file] = image;
            return image;
        }
        catch { return null; }
    }

    /// <summary>Official provider mark, bare, at one optical size; light-surface artwork in Light. Unknown providers
    /// get a neutral globe instead of a borrowed mark. Marks are never recoloured.</summary>
    public static FrameworkElement Mark(string provider, double box)
    {
        var host = new Grid { Width = box, Height = box, VerticalAlignment = VerticalAlignment.Center, HorizontalAlignment = HorizontalAlignment.Center };
        if (!KnownMarks.Contains(provider))
        {
            host.Children.Add(Icons.Icon("globe", box * 0.86, Theme.Brush("Ink3")));
            host.ToolTip = Tip(provider);
            return host;
        }
        var size = Math.Round(box * markScale[provider] * 10) / 10;
        Image Make(string file) { var image = new Image { Source = MarkImage(file), Width = size, Height = size, Stretch = Stretch.Uniform }; RenderOptions.SetBitmapScalingMode(image, BitmapScalingMode.HighQuality); return image; }
        if (lightVariants.Contains(provider))
        {
            var light = Make(provider + "-light"); var dark = Make(provider);
            void Sync() { light.Visibility = Theme.IsDark ? Visibility.Collapsed : Visibility.Visible; dark.Visibility = Theme.IsDark ? Visibility.Visible : Visibility.Collapsed; }
            Sync(); Theme.Changed += Sync;
            host.Unloaded += (_, _) => Theme.Changed -= Sync;
            host.Loaded += (_, _) => { Theme.Changed -= Sync; Theme.Changed += Sync; Sync(); };
            host.Children.Add(light); host.Children.Add(dark);
        }
        else host.Children.Add(Make(provider));
        return host;
    }

    public static void Detach(FrameworkElement element)
    {
        switch (element.Parent)
        {
            case Panel panel: panel.Children.Remove(element); break;
            case Decorator decorator: decorator.Child = null; break;
            case ContentControl content: content.Content = null; break;
        }
    }

    public static Button Button(string label, string style = "AtlasButton", FrameworkElement? icon = null, bool trailingIcon = false) => new()
    {
        Style = (Style)System.Windows.Application.Current.FindResource(style),
        Content = icon is null ? label : LabelWithIcon(label, icon, trailingIcon),
    };

    public static StackPanel LabelWithIcon(string label, FrameworkElement icon, bool trailing = false)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal };
        var text = new TextBlock { Text = label, VerticalAlignment = VerticalAlignment.Center };
        icon.VerticalAlignment = VerticalAlignment.Center;
        if (trailing) { row.Children.Add(text); icon.Margin = new Thickness(6, 0, 0, 0); row.Children.Add(icon); }
        else { row.Children.Add(icon); text.Margin = new Thickness(6, 0, 0, 0); row.Children.Add(text); }
        return row;
    }

    public static Button IconButton(string icon, string tip, double size = 28, string style = "IconButton", double glyph = 15)
    {
        var button = new Button { Style = (Style)System.Windows.Application.Current.FindResource(style), Width = size, Height = size, ToolTip = Tip(tip) };
        button.Content = Icons.Icon(icon, glyph, Theme.Brush("Ink2"));
        System.Windows.Automation.AutomationProperties.SetName(button, tip);
        return button;
    }

    /// <summary>Filled check-circle: accent disc, accent-ink check drawn by a dash offset.</summary>
    public static Grid CheckCircle(double size, bool draw)
    {
        var grid = new Grid { Width = size, Height = size, RenderTransformOrigin = new Point(0.5, 0.5), RenderTransform = new ScaleTransform(1, 1) };
        var canvas = new Canvas { Width = 20, Height = 20 };
        canvas.Children.Add(new Ellipse { Width = 16.5, Height = 16.5, Fill = Theme.Brush("Accent"), Margin = new Thickness(1.75) });
        var check = new Path { Data = Icons.Geometry("M6.4 10.3 l2.5 2.5 4.8-5"), Stroke = Theme.Brush("AccentInk"), StrokeThickness = 1.9, StrokeStartLineCap = PenLineCap.Round, StrokeEndLineCap = PenLineCap.Round, StrokeLineJoin = PenLineJoin.Round, StrokeDashArray = new DoubleCollection { 6, 6 }, StrokeDashOffset = 0 };
        canvas.Children.Add(check);
        grid.Children.Add(new Viewbox { Child = canvas });
        if (draw && Motion.Enabled)
        {
            grid.Opacity = 0;
            Motion.To(grid, UIElement.OpacityProperty, 1, 300);
            var scale = (ScaleTransform)grid.RenderTransform;
            Motion.To(scale, ScaleTransform.ScaleXProperty, 1, 300, from: 0.6);
            Motion.To(scale, ScaleTransform.ScaleYProperty, 1, 300, from: 0.6);
            check.StrokeDashOffset = 6;
            Motion.To(check, Shape.StrokeDashOffsetProperty, 0, 380, delay: 150, from: 6);
        }
        return grid;
    }

    /// <summary>The section column grid: mark | identity | meters | action slot | tail, with 14 px gaps.</summary>
    public static Grid SectionGrid(int meters, double acts, double identity = 186, double mark = 22)
    {
        var grid = new Grid();
        void Add(GridLength length) => grid.ColumnDefinitions.Add(new ColumnDefinition { Width = length });
        var gap = new GridLength(14);
        Add(new GridLength(mark)); Add(gap); Add(new GridLength(identity));
        for (int i = 0; i < meters; i++) { Add(gap); Add(new GridLength(1, GridUnitType.Star)); }
        Add(gap); Add(new GridLength(acts)); Add(gap); Add(new GridLength(14));
        return grid;
    }
    public static int MeterColumn(int index) => 4 + 2 * index;
    public static int ActsColumn(int meters) => 4 + 2 * meters;
    public static int TailColumn(int meters) => 6 + 2 * meters;
}

public enum MeterKind { Compact, Labeled, Detail }

/// <summary>What a meter shows. Value is % used; null is "unavailable", never zero.</summary>
public sealed record MeterSpec(double? Value, string Tooltip, string? Label = null, string? Reset = null, bool ResetSoon = false, string? Amount = null,
    double? Notch = null, double NotchOpacity = 1, string NaText = "Unavailable", string? Caption = null);

/// <summary>
/// The dashboard meter scaled to the tray: tabular SemiCondensed value with "%" in the same run, reset with a clock,
/// a 6 px track with quarter ticks, three severity gradients that cross-fade, the auto-switch notch and an overage end
/// cap in an 8 px gutter. The value and the fill animate together with an ease-out (never past the reading).
/// </summary>
public sealed class Meter : Grid
{
    public static readonly DependencyProperty ShownProperty = DependencyProperty.Register(nameof(Shown), typeof(double), typeof(Meter), new PropertyMetadata(0d, (d, _) => ((Meter)d).Paint()));
    public double Shown { get => (double)GetValue(ShownProperty); set => SetValue(ShownProperty, value); }

    public string Key { get; }
    public MeterKind Kind { get; }
    public double? Target { get; private set; }
    public bool HasShownTarget { get; private set; }
    private int decimals;
    private readonly TextBlock value = new() { FontFamily = Theme.Numerals, FontWeight = FontWeights.SemiBold, VerticalAlignment = VerticalAlignment.Bottom };
    private readonly TextBlock label = Ui.Text("", 12, "Ink2", FontWeights.Medium, trim: true);
    private readonly DockPanel reset = new() { LastChildFill = true, VerticalAlignment = VerticalAlignment.Center };
    private readonly TextBlock resetText = Ui.Text("", 11.5, "Ink3");
    private readonly TextBlock amount = Ui.Text("", 11.5, "Ink3", trim: true);
    private readonly TextBlock longReset = Ui.Text("", 11.5, "Ink2", wrap: true);
    private readonly TextBlock caption = Ui.Text("", 11, "Ink3", FontWeights.Medium);
    private readonly Grid track = new() { Height = 6 };
    private readonly Border trackPlate = new() { CornerRadius = new CornerRadius(3) };
    private readonly Rectangle naOutline = new() { RadiusX = 3, RadiusY = 3, StrokeThickness = 1, StrokeDashArray = new DoubleCollection { 3, 2 }, Visibility = Visibility.Collapsed };
    private readonly Grid ticks = new();
    private readonly Grid fill = new() { HorizontalAlignment = HorizontalAlignment.Left, Width = 0 };
    private readonly Border calm = new() { CornerRadius = new CornerRadius(3) }, warn = new() { CornerRadius = new CornerRadius(3), Opacity = 0 }, crit = new() { CornerRadius = new CornerRadius(3), Opacity = 0 };
    private readonly Border notch = new() { Width = 2, Height = 12, CornerRadius = new CornerRadius(1), HorizontalAlignment = HorizontalAlignment.Left, VerticalAlignment = VerticalAlignment.Center, Visibility = Visibility.Collapsed };
    private readonly Border over = new() { CornerRadius = new CornerRadius(0, 3, 3, 0), HorizontalAlignment = HorizontalAlignment.Left, Visibility = Visibility.Collapsed };
    private string severity = "calm";
    private MeterSpec spec = new(null, "");

    public Meter(string key, MeterKind kind)
    {
        Key = key; Kind = kind;
        VerticalAlignment = VerticalAlignment.Center;
        Margin = new Thickness(0, 0, 8, 0); // the 8 px overage gutter: every track keeps one length
        RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        trackPlate.Background = Theme.Brush("Track");
        naOutline.Stroke = Theme.Brush("RuleStrong");
        for (int i = 0; i < 4; i++) ticks.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        for (int i = 1; i < 4; i++) { var tick = new Border { Width = 1, Margin = new Thickness(0, 1, 0, 1), HorizontalAlignment = HorizontalAlignment.Left, Background = Theme.Brush("TrackTick") }; Grid.SetColumn(tick, i); ticks.Children.Add(tick); }
        calm.Background = Theme.Gradient("calm"); warn.Background = Theme.Gradient("warn"); crit.Background = Theme.Gradient("crit");
        fill.Children.Add(calm); fill.Children.Add(warn); fill.Children.Add(crit);
        notch.Background = Theme.Brush("Ink");
        over.Background = Theme.Brush("Over");
        track.Children.Add(trackPlate); track.Children.Add(naOutline); track.Children.Add(ticks); track.Children.Add(fill); track.Children.Add(over); track.Children.Add(notch);
        track.ClipToBounds = false;
        track.SizeChanged += (_, _) => Paint();
        var clock = Icons.Icon("clock", 12, Theme.Brush("Ink3"));
        clock.Margin = new Thickness(0, 1, 4, 0);
        reset.Children.Add(clock); reset.Children.Add(resetText);
        var top = new Grid { Height = kind == MeterKind.Compact ? 17 : 16 };
        if (kind == MeterKind.Compact)
        {
            // The value keeps its width; the reset takes what is left, right-aligned, and trims before it can touch the value.
            top.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            top.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            value.FontSize = 15; value.HorizontalAlignment = HorizontalAlignment.Left;
            top.Children.Add(value);
            resetText.TextTrimming = TextTrimming.CharacterEllipsis;
            Grid.SetColumn(reset, 1); reset.HorizontalAlignment = HorizontalAlignment.Right; reset.Margin = new Thickness(6, 0, 0, 0); top.Children.Add(reset);
        }
        else
        {
            top.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            top.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            value.FontSize = 14; label.VerticalAlignment = VerticalAlignment.Bottom;
            top.Children.Add(label);
            Grid.SetColumn(value, 1); top.Children.Add(value);
        }
        Children.Add(top);
        track.Margin = new Thickness(0, kind == MeterKind.Compact ? 6 : 5, 0, 0);
        Grid.SetRow(track, 1); Children.Add(track);
        if (kind == MeterKind.Labeled)
        {
            var foot = new Grid { Margin = new Thickness(0, 4, 0, 0), MinHeight = 14 };
            foot.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            foot.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            foot.Children.Add(reset);
            amount.HorizontalAlignment = HorizontalAlignment.Right; amount.Margin = new Thickness(8, 0, 0, 0);
            Grid.SetColumn(amount, 1); foot.Children.Add(amount);
            Grid.SetRow(foot, 2); Children.Add(foot);
        }
        else if (kind == MeterKind.Detail)
        {
            longReset.Margin = new Thickness(0, 5, 0, 0); Grid.SetRow(longReset, 2); Children.Add(longReset);
            amount.Margin = new Thickness(0, 2, 0, 0); Grid.SetRow(amount, 3); Children.Add(amount);
        }
        caption.Margin = new Thickness(0, 3, 0, 0); caption.Visibility = Visibility.Collapsed;
        if (kind == MeterKind.Compact) { Grid.SetRow(caption, 2); Children.Add(caption); }
    }

    /// <summary>Applies a reading. Animated changes ease out from what is on screen to the new value.</summary>
    public void Update(MeterSpec next, bool animate, double milliseconds = Motion.Draw)
    {
        spec = next;
        ToolTip = string.IsNullOrEmpty(next.Tooltip) ? null : Ui.Tip(next.Tooltip);
        label.Text = next.Label ?? "";
        resetText.Text = next.Reset ?? "";
        reset.Visibility = string.IsNullOrEmpty(next.Reset) ? Visibility.Collapsed : Visibility.Visible;
        resetText.Foreground = Theme.Brush(next.ResetSoon ? "Ink2" : "Ink3");
        resetText.FontWeight = next.ResetSoon ? FontWeights.Medium : FontWeights.Normal;
        amount.Text = next.Amount ?? ""; amount.Visibility = string.IsNullOrEmpty(next.Amount) ? Visibility.Collapsed : Visibility.Visible;
        longReset.Text = next.Reset ?? "";
        if (Kind == MeterKind.Detail) { reset.Visibility = Visibility.Collapsed; longReset.Visibility = string.IsNullOrEmpty(next.Reset) ? Visibility.Collapsed : Visibility.Visible; }
        caption.Text = next.Caption ?? ""; caption.Visibility = string.IsNullOrEmpty(next.Caption) ? Visibility.Collapsed : Visibility.Visible;
        notch.Visibility = next.Notch is double ? Visibility.Visible : Visibility.Collapsed;
        notch.Opacity = next.NotchOpacity;
        var nextSeverity = Theme.Severity(next.Value);
        if (next.Value is not double target)
        {
            BeginAnimation(ShownProperty, null);
            Target = null; Shown = 0;
            value.Text = next.NaText; value.FontFamily = Theme.Sans; value.FontWeight = FontWeights.Medium; value.FontSize = 12; value.Foreground = Theme.Brush("Ink3");
            trackPlate.Background = Brushes.Transparent; naOutline.Visibility = Visibility.Visible; ticks.Visibility = Visibility.Collapsed; fill.Visibility = Visibility.Collapsed;
            severity = "na"; Paint();
            return;
        }
        value.FontFamily = Theme.Numerals; value.FontWeight = FontWeights.SemiBold; value.FontSize = Kind == MeterKind.Compact ? 15 : 14;
        trackPlate.Background = Theme.Brush("Track"); naOutline.Visibility = Visibility.Collapsed; ticks.Visibility = Visibility.Visible; fill.Visibility = Visibility.Visible;
        decimals = Formatting.Decimals(target);
        if (nextSeverity != severity)
        {
            var fade = severity == "na" || !animate ? 0 : 420;
            Motion.To(calm, OpacityProperty, nextSeverity == "calm" ? 1 : 0, fade, Motion.InOut);
            Motion.To(warn, OpacityProperty, nextSeverity == "warn" ? 1 : 0, fade, Motion.InOut);
            Motion.To(crit, OpacityProperty, nextSeverity is "crit" or "over" ? 1 : 0, fade, Motion.InOut);
            severity = nextSeverity;
        }
        value.Foreground = Theme.Brush(Theme.SeverityText(nextSeverity));
        Target = target;
        if (animate && Motion.Enabled && Math.Abs(Shown - target) > 0.0001)
            Motion.To(this, ShownProperty, target, milliseconds, Motion.Out, from: Shown);
        else { BeginAnimation(ShownProperty, null); Shown = target; }
        HasShownTarget = true;
        Paint();
    }

    /// <summary>Hover feedback from the row: the track brightens.</summary>
    public void SetHover(bool hover) { if (severity != "na") trackPlate.Background = Theme.Brush(hover ? "TrackHover" : "Track"); }

    public double TrackWidth => track.ActualWidth;
    public double FillWidth => fill.Width;

    private void Paint()
    {
        var width = track.ActualWidth;
        if (severity == "na") { value.Text = spec.NaText; return; }
        var shown = Shown;
        value.Text = Formatting.PercentWith(shown, decimals);
        if (width <= 0) return;
        fill.Width = width * Math.Clamp(shown, 0, 100) / 100;
        if (spec.Notch is double at) notch.Margin = new Thickness(width * Math.Clamp(at, 0, 100) / 100 - 1, 0, 0, 0);
        if (Target is double target && target > 100 && shown > 100)
        {
            over.Visibility = Visibility.Visible;
            over.Width = Math.Min(8, 3 + (shown - 100) / 4);
            over.Margin = new Thickness(width, 0, 0, 0);
            calm.CornerRadius = warn.CornerRadius = crit.CornerRadius = new CornerRadius(3, 0, 0, 3);
        }
        else
        {
            over.Visibility = Visibility.Collapsed;
            calm.CornerRadius = warn.CornerRadius = crit.CornerRadius = new CornerRadius(3);
        }
    }
}

/// <summary>Windows 11 toggle: outlined when off, filled accent when on; the knob springs (a control, not a value).</summary>
public sealed class ToggleSwitch : Grid
{
    private readonly Border trackOff = new() { Width = 38, Height = 20, CornerRadius = new CornerRadius(10), BorderThickness = new Thickness(1) };
    private readonly Border trackOn = new() { Width = 38, Height = 20, CornerRadius = new CornerRadius(10), Opacity = 0 };
    private readonly Ellipse knobOff = new() { Width = 12, Height = 12 }, knobOn = new() { Width = 12, Height = 12, Opacity = 0 };
    private readonly Grid knob = new() { Width = 12, Height = 12, HorizontalAlignment = HorizontalAlignment.Left, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(4, 0, 0, 0), RenderTransform = new TranslateTransform() };
    private readonly TextBlock text;
    private bool on, enabled = true;
    public event Action<bool>? Toggled;

    public ToggleSwitch(bool isOn, string label, string? tooltip = null)
    {
        Cursor = Cursors.Hand; Background = Brushes.Transparent; VerticalAlignment = VerticalAlignment.Center;
        ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(38) });
        ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        trackOff.BorderBrush = Theme.Brush("Ink3"); trackOn.Background = Theme.Brush("Accent");
        knobOff.Fill = Theme.Brush("Ink2"); knobOn.Fill = Theme.Brush("AccentInk");
        knob.Children.Add(knobOff); knob.Children.Add(knobOn);
        Children.Add(trackOff); Children.Add(trackOn); Children.Add(knob);
        text = Ui.Text(label, 12.5, "Ink"); text.VerticalAlignment = VerticalAlignment.Center; text.Margin = new Thickness(label.Length > 0 ? 8 : 0, 0, 0, 0);
        Grid.SetColumn(text, 1); Children.Add(text);
        if (tooltip is not null) ToolTip = Ui.Tip(tooltip);
        SetOn(isOn, false);
        MouseLeftButtonUp += (_, e) => { if (!enabled) return; e.Handled = true; Toggled?.Invoke(!on); };
        MouseLeftButtonDown += (_, e) => { if (enabled) e.Handled = true; };
    }

    public bool IsOn => on;

    public void SetOn(bool value, bool animate = true)
    {
        on = value;
        var shift = (TranslateTransform)knob.RenderTransform;
        Motion.To(shift, TranslateTransform.XProperty, value ? 18 : 0, animate ? 280 : 0, Motion.Spring);
        Motion.To(trackOn, OpacityProperty, value ? 1 : 0, animate ? 200 : 0, Motion.InOut);
        Motion.To(knobOn, OpacityProperty, value ? 1 : 0, animate ? 200 : 0, Motion.InOut);
        Motion.To(knobOff, OpacityProperty, value ? 0 : 1, animate ? 200 : 0, Motion.InOut);
    }

    public void SetEnabled(bool value)
    {
        enabled = value;
        Cursor = value ? Cursors.Hand : Cursors.Arrow;
        trackOff.BorderBrush = Theme.Brush(value ? "Ink3" : "Ink4");
        Opacity = value ? 1 : 0.55;
    }
}

/// <summary>Segmented control with a sliding thumb (a control: the thumb may spring).</summary>
public sealed class Segmented : Border
{
    private readonly Grid grid = new();
    private readonly Border thumb = new() { CornerRadius = new CornerRadius(4), BorderThickness = new Thickness(1), HorizontalAlignment = HorizontalAlignment.Left, RenderTransform = new TranslateTransform() };
    private readonly List<(string Value, Button Button, TextBlock Label, FrameworkElement? Icon)> items = new();
    private string selected;
    public event Action<string>? Changed;

    public Segmented(IEnumerable<(string Value, string Label, string? Icon)> options, string value)
    {
        selected = value;
        Background = Theme.Brush("Card3"); BorderBrush = Theme.Brush("Rule2"); BorderThickness = new Thickness(1); CornerRadius = new CornerRadius(5); Padding = new Thickness(3);
        thumb.Background = Theme.Brush("Card"); thumb.BorderBrush = Theme.Brush("Rule");
        var row = new StackPanel { Orientation = Orientation.Horizontal };
        grid.Children.Add(thumb); grid.Children.Add(row);
        foreach (var (optionValue, optionLabel, iconName) in options)
        {
            var labelText = new TextBlock { Text = optionLabel, FontSize = 12.5, FontWeight = FontWeights.Medium, VerticalAlignment = VerticalAlignment.Center };
            var content = new StackPanel { Orientation = Orientation.Horizontal };
            FrameworkElement? icon = null;
            if (iconName is not null) { icon = Icons.Icon(iconName, 14, Theme.Brush("Ink3")); icon.Margin = new Thickness(0, 0, 6, 0); content.Children.Add(icon); }
            content.Children.Add(labelText);
            var button = new Button { Content = content, Height = 26, Padding = new Thickness(12, 0, 12, 0), Style = (Style)System.Windows.Application.Current.FindResource("GhostButton"), Background = Brushes.Transparent };
            button.Click += (_, _) => Select(optionValue, true);
            row.Children.Add(button);
            items.Add((optionValue, button, labelText, icon));
        }
        Child = grid;
        Loaded += (_, _) => Place(false);
        SizeChanged += (_, _) => Place(false);
        Paint();
    }

    public string Value => selected;

    public void Select(string value, bool raise)
    {
        if (value == selected) return;
        selected = value; Paint(); Place(true);
        if (raise) Changed?.Invoke(value);
    }

    private void Paint()
    {
        foreach (var (value, _, labelText, icon) in items)
        {
            var ink = Theme.Brush(value == selected ? "Ink" : "Ink3");
            labelText.Foreground = ink;
            if (icon is Viewbox { Child: Canvas { Children.Count: > 0 } canvas } && canvas.Children[0] is Path path) path.Stroke = ink;
        }
    }

    private void Place(bool animate)
    {
        var target = items.FirstOrDefault(item => item.Value == selected).Button;
        if (target is null || target.ActualWidth <= 0) return;
        var x = target.TranslatePoint(new Point(0, 0), grid).X;
        Motion.To((TranslateTransform)thumb.RenderTransform, TranslateTransform.XProperty, x, animate ? 380 : 0, Motion.Spring);
        Motion.To(thumb, WidthProperty, target.ActualWidth, animate ? 380 : 0, Motion.Out);
        thumb.Height = target.ActualHeight;
    }
}

/// <summary>A themed dropdown menu (threshold choices, Qwen packs) in a non-activating popup.</summary>
public static class Menus
{
    public static Popup Create(UIElement target, FrameworkElement content, PlacementMode placement = PlacementMode.Bottom, double minWidth = 150)
    {
        var border = new Border { Background = Theme.Brush("Card"), BorderBrush = Theme.Brush("Rule"), BorderThickness = new Thickness(1), CornerRadius = new CornerRadius(8), Padding = new Thickness(5), MinWidth = minWidth, Child = content, Margin = new Thickness(0, 4, 0, 4), RenderTransform = new TranslateTransform() };
        var popup = new Popup { PlacementTarget = target, Placement = placement, AllowsTransparency = true, StaysOpen = false, Child = border, PopupAnimation = PopupAnimation.None };
        popup.Opened += (_, _) =>
        {
            border.Opacity = 0; Motion.To(border, UIElement.OpacityProperty, 1, 160);
            Motion.To((TranslateTransform)border.RenderTransform, TranslateTransform.YProperty, 0, 220, from: placement == PlacementMode.Top ? 4 : -4);
        };
        return popup;
    }

    public static Button Item(string text, bool selected, Action click, string? icon = null)
    {
        var row = new Grid { Height = 30 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        if (icon is not null) { var glyph = Icons.Icon(icon, 15, Theme.Brush("Ink3")); glyph.Margin = new Thickness(0, 0, 9, 0); row.Children.Add(glyph); }
        var label = Ui.Text(text, 12.5, "Ink", selected ? FontWeights.SemiBold : FontWeights.Normal); label.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(label, 1); row.Children.Add(label);
        var check = Icons.Icon("check", 14, Theme.Brush("AccentText")); check.Margin = new Thickness(14, 0, 0, 0); check.Opacity = selected ? 1 : 0;
        Grid.SetColumn(check, 2); row.Children.Add(check);
        var button = new Button { Content = row, Style = (Style)System.Windows.Application.Current.FindResource("GhostButton"), Height = 30, Padding = new Thickness(10, 0, 10, 0), HorizontalContentAlignment = HorizontalAlignment.Stretch };
        button.Click += (_, _) => click();
        return button;
    }
}
