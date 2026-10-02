using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Documents;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Shapes;

namespace CCSBar;

/// <summary>
/// The sign-in screen's own icon set (trays concept, Windows view): Segoe Fluent style outlines on a 20 grid, 1.2 px
/// strokes, drawn as vector paths (never glyph fonts). Filled parts use the stroke colour; the check and the marks inside
/// the filled warning and error shapes use the "ink" brush (white, or the ink on the calm success fill).
/// </summary>
public static class SignInIcons
{
    private sealed record Part(string Data, bool Fill = false, bool Ink = false, double? Stroke = null);

    private static string Circle(double cx, double cy, double r) => string.Create(CultureInfo.InvariantCulture,
        $"M{cx - r},{cy} A{r},{r} 0 1 0 {cx + r},{cy} A{r},{r} 0 1 0 {cx - r},{cy} Z");
    private static string Ellipse(double cx, double cy, double rx, double ry) => string.Create(CultureInfo.InvariantCulture,
        $"M{cx - rx},{cy} A{rx},{ry} 0 1 0 {cx + rx},{cy} A{rx},{ry} 0 1 0 {cx - rx},{cy} Z");
    private static string Rect(double x, double y, double w, double h, double r) => string.Create(CultureInfo.InvariantCulture,
        $"M{x + r},{y} H{x + w - r} A{r},{r} 0 0 1 {x + w},{y + r} V{y + h - r} A{r},{r} 0 0 1 {x + w - r},{y + h} H{x + r} A{r},{r} 0 0 1 {x},{y + h - r} V{y + r} A{r},{r} 0 0 1 {x + r},{y} Z");

    private static readonly Dictionary<string, Part[]> Set = new()
    {
        ["lock"] = new[] { new Part(Rect(4.5, 8.5, 11, 8.5, 1.5)), new Part("M7 8.5V6a3 3 0 0 1 6 0v2.5"), new Part(Circle(10, 12.75, 0.95), Fill: true) },
        ["key"] = new[] { new Part("M12.5 3a4.5 4.5 0 1 1-1.6 8.7L9 13.6H7.5v1.5H6v1.5H3.4V14l5.1-5.1A4.5 4.5 0 0 1 12.5 3Z"), new Part(Circle(13.6, 6.4, 0.95), Fill: true) },
        ["check"] = new[] { new Part(Circle(10, 10, 8), Fill: true), new Part("M6.6 10.2 l2.3 2.3 4.6-4.8", Ink: true, Stroke: 1.4) },
        ["warn"] = new[] { new Part("M8.7 3.4a1.5 1.5 0 0 1 2.6 0l6.1 10.8a1.5 1.5 0 0 1-1.3 2.3H3.9a1.5 1.5 0 0 1-1.3-2.3Z"), new Part("M10 7.6v4"), new Part(Circle(10, 13.9, 0.85), Fill: true) },
        ["err"] = new[] { new Part(Circle(10, 10, 7.5)), new Part("M10 6.2v4.6"), new Part(Circle(10, 13.5, 0.85), Fill: true) },
        ["info"] = new[] { new Part(Circle(10, 10, 7.5)), new Part("M10 9v4.8"), new Part(Circle(10, 6.5, 0.85), Fill: true) },
        ["eye"] = new[] { new Part("M2.5 11C3.6 7.4 6.5 5 10 5s6.4 2.4 7.5 6"), new Part(Circle(10, 11.5, 3)) },
        ["eyeOff"] = new[] { new Part("M3 3l14 14"), new Part("M5.4 6.7A8.4 8.4 0 0 0 2.5 11M8 5.3A7.5 7.5 0 0 1 10 5c3.5 0 6.4 2.4 7.5 6"), new Part("M7.9 9.4a3 3 0 0 0 4.2 4.2") },
        ["timer"] = new[] { new Part(Circle(10, 11, 6.5)), new Part("M10 11V7.5M8 2.5h4M15.1 5.4l1.1-1.1") },
        ["globe"] = new[] { new Part(Circle(10, 10, 7.5)), new Part(Ellipse(10, 10, 3.1, 7.5)), new Part("M2.5 10h15") },
        ["retry"] = new[] { new Part("M16.5 10a6.5 6.5 0 1 1-1.9-4.6"), new Part("M15 2.5V6h-3.5") },
        ["terminal"] = new[] { new Part(Rect(2.5, 3.5, 15, 13, 1.5)), new Part("M6 8l2 2-2 2M10 12.5h4") },
        ["shield"] = new[] { new Part("M10 2.5 16 4.7v4.8c0 3.9-2.5 6.5-6 8-3.5-1.5-6-4.1-6-8V4.7Z"), new Part("M7.6 10l1.7 1.7 3.2-3.4") },
        ["signOut"] = new[] { new Part("M8 3.5H5A1.5 1.5 0 0 0 3.5 5v10A1.5 1.5 0 0 0 5 16.5h3"), new Part("M8 10h9M14 7l3 3-3 3") },
        ["capsLock"] = new[] { new Part("M10 3.5 4 9.8h3.2V13h5.6V9.8H16Z"), new Part("M7.2 16.5h5.6") },
        ["pending"] = new[] { new Part(Circle(10, 10, 7.5)) },
        ["house"] = new[] { new Part("M3 9.4 10 3.5l7 5.9"), new Part("M5 7.8v8.7h3.7V12h2.6v4.5H15V7.8") },
    };

    public static IEnumerable<string> Names => Set.Keys;

    /// <summary>An icon at <paramref name="size"/> px. <paramref name="ink"/> colours the marks drawn on a filled shape.</summary>
    public static FrameworkElement Icon(string name, double size, Brush brush, Brush? ink = null, double stroke = 1.2)
    {
        var canvas = new Canvas { Width = 20, Height = 20 };
        foreach (var part in Set.TryGetValue(name, out var parts) ? parts : Set["info"])
        {
            var path = new Path { Data = Icons.Geometry(part.Data), StrokeStartLineCap = PenLineCap.Round, StrokeEndLineCap = PenLineCap.Round, StrokeLineJoin = PenLineJoin.Round };
            if (part.Fill) path.Fill = brush;
            else { path.Stroke = part.Ink ? ink ?? Brushes.White : brush; path.StrokeThickness = part.Stroke ?? stroke; }
            canvas.Children.Add(path);
        }
        return new Viewbox { Width = size, Height = size, Child = canvas, SnapsToDevicePixels = false };
    }

    /// <summary>The Windows ProgressRing: a faint full ring and a quarter arc that turns only while asked to.</summary>
    public static Grid Ring(double size, Brush brush, out RotateTransform turn)
    {
        var canvas = new Canvas { Width = 20, Height = 20 };
        canvas.Children.Add(new Path { Data = Icons.Geometry(Circle(10, 10, 7.2)), Stroke = brush, StrokeThickness = 1.6, Opacity = 0.22 });
        var arc = new Path { Data = Icons.Geometry("M17.2 10A7.2 7.2 0 0 0 10 2.8"), Stroke = brush, StrokeThickness = 1.6, StrokeStartLineCap = PenLineCap.Round, StrokeEndLineCap = PenLineCap.Round };
        canvas.Children.Add(arc);
        turn = new RotateTransform(0, 10, 10);
        canvas.RenderTransform = turn;
        return new Grid { Width = size, Height = size, Children = { new Viewbox { Child = canvas } } };
    }

    public static void Spin(RotateTransform turn, bool on)
    {
        if (on && Motion.Enabled) turn.BeginAnimation(RotateTransform.AngleProperty, new DoubleAnimation(0, 360, TimeSpan.FromMilliseconds(900)) { RepeatBehavior = RepeatBehavior.Forever });
        else { turn.BeginAnimation(RotateTransform.AngleProperty, null); turn.Angle = 0; }
    }
}

/// <summary>A region that opens and closes with an animated height and a content fade (260 ms), as the dashboard's
/// sign-in page does. A closed region's content is collapsed, so its fields leave the tab order.</summary>
internal sealed class Collapsible : Border
{
    private readonly FrameworkElement inner;
    private readonly TranslateTransform shift = new();
    private readonly double measureWidth;
    public bool IsOpen { get; private set; }
    public FrameworkElement Content => inner;

    public Collapsible(FrameworkElement content, double width)
    {
        measureWidth = width;
        inner = content;
        // The 6 px side margins keep a focused field's halo inside the clip, as the concept's .si-coll does.
        Margin = new Thickness(-6, 0, -6, 0);
        inner.Margin = new Thickness(6, 0, 6, 0);
        inner.RenderTransform = shift;
        Child = inner;
        ClipToBounds = true;
        Height = 0;
        inner.Opacity = 0;
        inner.Visibility = Visibility.Collapsed;
    }

    public void Set(bool open, bool instant)
    {
        if (open == IsOpen && !instant) return;
        IsOpen = open;
        BeginAnimation(HeightProperty, null);
        if (instant || !Motion.Enabled)
        {
            inner.Visibility = open ? Visibility.Visible : Visibility.Collapsed;
            Height = open ? double.NaN : 0;
            ClipToBounds = !open;
            inner.BeginAnimation(OpacityProperty, null); inner.Opacity = open ? 1 : 0;
            shift.BeginAnimation(TranslateTransform.YProperty, null); shift.Y = 0;
            return;
        }
        var from = ActualHeight;
        ClipToBounds = true;
        double to = 0;
        if (open)
        {
            inner.Visibility = Visibility.Visible;
            inner.Measure(new Size(measureWidth + 12, double.PositiveInfinity));
            to = inner.DesiredSize.Height;
        }
        Motion.To(this, HeightProperty, to, Motion.Med, Motion.InOut, from: from, completed: (_, _) =>
        {
            if (IsOpen != open) return;
            BeginAnimation(HeightProperty, null);
            Height = open ? double.NaN : 0;
            ClipToBounds = !open;
            if (!open) inner.Visibility = Visibility.Collapsed;
        });
        Motion.To(inner, OpacityProperty, open ? 1 : 0, Motion.Med, Motion.InOut, delay: open ? 60 : 0);
        Motion.To(shift, TranslateTransform.YProperty, open ? 0 : -4, Motion.Med, Motion.Out, delay: open ? 60 : 0, from: open ? -4 : null);
    }
}

/// <summary>A sign-in field: label (with an aside on the right), a 36 px box with the focus halo, an optional eye
/// toggle that swaps the password box for a text box, and a placeholder.</summary>
internal sealed class SiField : StackPanel
{
    public string Key { get; }
    public readonly TextBlock Label, Aside;
    private readonly Border box, halo;
    private readonly ScaleTransform haloScale = new(0.985, 0.985);
    public readonly TextBox? Text;
    public readonly PasswordBox? Secret;
    private readonly TextBox? shown;
    private readonly Button? eye;
    private readonly FrameworkElement? eyeShow, eyeHide;
    private readonly TextBlock placeholder;
    private bool bad, off, focused, matched;
    public event Action? Edited;
    public event Action<bool>? FocusChanged;

    public SiField(string key, string label, bool password = false, string? placeholderText = null, bool mono = false)
    {
        Key = key;
        Margin = new Thickness(0, 0, 0, 12);
        var head = new DockPanel { LastChildFill = true, Margin = new Thickness(0, 0, 0, 6) };
        Aside = Ui.Text("", 11.5, "Ink3"); Aside.VerticalAlignment = VerticalAlignment.Bottom;
        DockPanel.SetDock(Aside, Dock.Right); head.Children.Add(Aside);
        Label = Ui.Text(label, 12, "Ink2", FontWeights.SemiBold); head.Children.Add(Label);
        Children.Add(head);
        var host = new Grid { Height = 36 };
        halo = new Border { Margin = new Thickness(-4), CornerRadius = new CornerRadius(7), BorderThickness = new Thickness(3), BorderBrush = Theme.Brush("Halo"), Opacity = 0, IsHitTestVisible = false, RenderTransformOrigin = new Point(0.5, 0.5), RenderTransform = haloScale };
        host.Children.Add(halo);
        var inside = new Grid();
        box = new Border { CornerRadius = new CornerRadius(4), Background = Theme.Brush("Card"), BorderBrush = Theme.Brush("RuleStrong"), BorderThickness = new Thickness(1), Child = inside, SnapsToDevicePixels = true };
        host.Children.Add(box);
        placeholder = Ui.Text(placeholderText ?? "", 13.5, "Ink4"); placeholder.VerticalAlignment = VerticalAlignment.Center; placeholder.Margin = new Thickness(12, 0, 0, 0); placeholder.IsHitTestVisible = false;
        if (mono) { placeholder.FontFamily = Theme.Mono; placeholder.FontSize = 12.5; }
        if (password)
        {
            Secret = new PasswordBox { Style = (Style)Application.Current.FindResource("BarePasswordBox"), Padding = new Thickness(11, 0, 40, 0) };
            shown = new TextBox { Style = (Style)Application.Current.FindResource("BareTextBox"), Padding = new Thickness(11, 0, 40, 0), Visibility = Visibility.Collapsed };
            Secret.PasswordChanged += (_, _) => { UpdatePlaceholder(); Edited?.Invoke(); };
            shown.TextChanged += (_, _) => { UpdatePlaceholder(); Edited?.Invoke(); };
            inside.Children.Add(Secret); inside.Children.Add(shown);
            var icons = new Grid();
            eyeShow = SignInIcons.Icon("eye", 16, Theme.Brush("Ink3")); eyeHide = SignInIcons.Icon("eyeOff", 16, Theme.Brush("AccentText"));
            foreach (var icon in new[] { eyeShow, eyeHide }) { icon.RenderTransformOrigin = new Point(0.5, 0.5); icon.RenderTransform = new TransformGroup { Children = { new ScaleTransform(), new RotateTransform() } }; icons.Children.Add(icon); }
            eyeHide.Opacity = 0; SetIconPose(eyeHide, 0.7, -12);
            eye = new Button { Style = (Style)Application.Current.FindResource("BareIconButton"), Width = 28, Height = 28, Content = icons, HorizontalAlignment = HorizontalAlignment.Right, Margin = new Thickness(0, 0, 3, 0), Focusable = true, Uid = "si-eye-" + key };
            System.Windows.Automation.AutomationProperties.SetName(eye, "Show password");
            eye.Click += (_, _) => ToggleShown();
            inside.Children.Add(eye);
        }
        else
        {
            Text = new TextBox { Style = (Style)Application.Current.FindResource("BareTextBox") };
            if (mono) { Text.FontFamily = Theme.Mono; Text.FontSize = 12.5; Text.CharacterCasing = CharacterCasing.Upper; }
            Text.TextChanged += (_, _) => { UpdatePlaceholder(); Edited?.Invoke(); };
            inside.Children.Insert(0, Text);
        }
        inside.Children.Insert(0, placeholder);
        foreach (var input in Inputs())
        {
            input.GotKeyboardFocus += (_, _) => { focused = true; Paint(); FocusChanged?.Invoke(true); };
            input.LostKeyboardFocus += (_, _) => { focused = Inputs().Any(control => control.IsKeyboardFocusWithin); Paint(); FocusChanged?.Invoke(false); };
        }
        host.MouseEnter += (_, _) => Paint(); host.MouseLeave += (_, _) => Paint();
        Children.Add(host);
        Paint();
    }

    private IEnumerable<Control> Inputs() => new Control?[] { Text, Secret, shown }.Where(control => control is not null).Select(control => control!);

    public Control Input => shown is { Visibility: Visibility.Visible } ? shown : (Control?)Secret ?? Text!;

    public string Value
    {
        get => Text?.Text ?? (shown is { Visibility: Visibility.Visible } ? shown.Text : Secret!.Password);
        set
        {
            if (Text is not null) { Text.Text = value; return; }
            Secret!.Password = value; shown!.Text = value;
        }
    }

    public bool IsShown => shown is { Visibility: Visibility.Visible };

    /// <summary>Show or hide the password: a text box takes the password box's place with the same text, and the two
    /// icons cross-fade and turn.</summary>
    public void ToggleShown(bool? show = null)
    {
        if (Secret is null || shown is null || eye is null) return;
        var on = show ?? !IsShown;
        if (on == IsShown) return;
        var refocus = Input.IsKeyboardFocusWithin;
        if (on) { shown.Text = Secret.Password; shown.Visibility = Visibility.Visible; Secret.Visibility = Visibility.Collapsed; }
        else { Secret.Password = shown.Text; shown.Clear(); shown.Visibility = Visibility.Collapsed; Secret.Visibility = Visibility.Visible; }
        System.Windows.Automation.AutomationProperties.SetName(eye, on ? "Hide password" : "Show password");
        Motion.To(eyeShow!, OpacityProperty, on ? 0 : 1, Motion.Fast, Motion.InOut); Motion.To(eyeHide!, OpacityProperty, on ? 1 : 0, Motion.Fast, Motion.InOut);
        SetIconPose(eyeShow!, on ? 0.7 : 1, on ? 12 : 0, animate: true); SetIconPose(eyeHide!, on ? 1 : 0.7, on ? 0 : -12, animate: true);
        if (refocus) FocusInput(false);
        UpdatePlaceholder();
    }

    private static void SetIconPose(FrameworkElement icon, double scale, double angle, bool animate = false)
    {
        var group = (TransformGroup)icon.RenderTransform;
        var s = (ScaleTransform)group.Children[0]; var r = (RotateTransform)group.Children[1];
        if (!animate) { s.ScaleX = s.ScaleY = scale; r.Angle = angle; return; }
        Motion.To(s, ScaleTransform.ScaleXProperty, scale, Motion.Med); Motion.To(s, ScaleTransform.ScaleYProperty, scale, Motion.Med);
        Motion.To(r, RotateTransform.AngleProperty, angle, Motion.Med);
    }

    /// <summary>Clears the secret from both boxes (the hand-off forgets the password).</summary>
    public void Clear()
    {
        if (Text is not null) { Text.Clear(); return; }
        Secret!.Clear(); shown!.Clear();
    }

    public void FocusInput(bool selectAll)
    {
        var input = Input;
        input.Focus(); Keyboard.Focus(input);
        if (!selectAll) return;
        if (input is TextBox text) text.SelectAll(); else if (input is PasswordBox secret) secret.SelectAll();
    }

    public bool Bad { get => bad; set { bad = value; Paint(); } }
    public bool Matched { get => matched; set { matched = value; Paint(); } }

    public bool Off
    {
        get => off;
        set
        {
            off = value;
            foreach (var input in Inputs()) input.IsEnabled = !value;
            if (eye is not null) eye.IsEnabled = !value;
            Paint();
        }
    }

    private void UpdatePlaceholder() => placeholder.Visibility = Value.Length == 0 ? Visibility.Visible : Visibility.Collapsed;

    private void Paint()
    {
        box.Background = Theme.Brush(off ? "Card2" : "Card");
        box.Opacity = off ? 0.72 : 1;
        box.BorderBrush = Theme.Brush(bad ? "Crit" : focused ? "Accent" : matched ? "OkMatch" : IsMouseOver && !off ? "Ink4" : "RuleStrong");
        // Windows: the focused box gains an inset bottom line in the accent (or the error colour).
        box.BorderThickness = new Thickness(1, 1, 1, focused ? 2 : 1);
        halo.BorderBrush = Theme.Brush(bad ? "HaloBad" : "Halo");
        Motion.To(halo, OpacityProperty, focused ? 1 : 0, Motion.Med, Motion.Out);
        Motion.To(haloScale, ScaleTransform.ScaleXProperty, focused ? 1 : 0.985, Motion.Med, Motion.Out);
        Motion.To(haloScale, ScaleTransform.ScaleYProperty, focused ? 1 : 0.985, Motion.Med, Motion.Out);
        UpdatePlaceholder();
    }
}

/// <summary>The primary button's three layers (label; progress ring and label; check and label) that cross-fade with a
/// 10 px rise, and its plate: accent, the calm green on success, or rested while disabled.</summary>
internal sealed class SiPrimary
{
    public readonly Button Button;
    private readonly TextBlock label, busyLabel, doneLabel;
    private readonly FrameworkElement[] layers;
    private readonly TranslateTransform[] lifts;
    private readonly RotateTransform ring;
    private readonly SolidColorBrush plate = new(), ink = new();
    private string mode = "normal";
    private bool rested;

    public SiPrimary()
    {
        label = new TextBlock { VerticalAlignment = VerticalAlignment.Center };
        busyLabel = new TextBlock { VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(8, 0, 0, 0) };
        doneLabel = new TextBlock { VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(8, 0, 0, 0) };
        var busy = new StackPanel { Orientation = Orientation.Horizontal };
        busy.Children.Add(SignInIcons.Ring(16, ink, out ring)); busy.Children.Add(busyLabel);
        var done = new StackPanel { Orientation = Orientation.Horizontal };
        done.Children.Add(SignInIcons.Icon("check", 17, ink, Theme.Brush("Calm"))); done.Children.Add(doneLabel);
        layers = new FrameworkElement[] { label, busy, done };
        lifts = layers.Select(_ => new TranslateTransform()).ToArray();
        var grid = new Grid();
        for (int i = 0; i < layers.Length; i++)
        {
            layers[i].HorizontalAlignment = HorizontalAlignment.Center; layers[i].VerticalAlignment = VerticalAlignment.Center;
            layers[i].RenderTransform = lifts[i]; layers[i].Opacity = i == 0 ? 1 : 0; lifts[i].Y = i == 0 ? 0 : 10;
            grid.Children.Add(layers[i]);
        }
        Button = new Button { Style = (Style)Application.Current.FindResource("SignInPrimaryButton"), Content = grid, Background = plate, Foreground = ink, Uid = "si-primary", IsDefault = true };
        Recolor(false);
        Theme.Changed += () => Recolor(false);
    }

    public string Label => label.Text;
    public string Mode => mode;

    public void SetLabels(string text, string busy, string done)
    {
        label.Text = text; busyLabel.Text = busy; doneLabel.Text = done;
        System.Windows.Automation.AutomationProperties.SetName(Button, mode == "busy" ? busy : mode == "done" ? done : text);
    }

    /// <summary>normal, busy or done; the layers cross-fade (260 ms) and the plate turns calm on done.</summary>
    public void SetMode(string next, bool instant)
    {
        mode = next;
        var index = next == "busy" ? 1 : next == "done" ? 2 : 0;
        for (int i = 0; i < layers.Length; i++)
        {
            var on = i == index;
            // The label leaves upward; the progress and done layers wait 10 px below.
            var y = on ? 0 : i == 0 ? -10 : 10;
            if (instant || !Motion.Enabled) { layers[i].BeginAnimation(UIElement.OpacityProperty, null); layers[i].Opacity = on ? 1 : 0; lifts[i].BeginAnimation(TranslateTransform.YProperty, null); lifts[i].Y = y; }
            else { Motion.To(layers[i], UIElement.OpacityProperty, on ? 1 : 0, Motion.Med, Motion.InOut); Motion.To(lifts[i], TranslateTransform.YProperty, y, Motion.Med, Motion.Out); }
        }
        SignInIcons.Spin(ring, next == "busy");
        Button.Cursor = next == "normal" && !rested ? Cursors.Hand : next == "busy" ? Cursors.Wait : Cursors.Arrow;
        Recolor(!instant);
    }

    /// <summary>Disabled (rate limited): the plate rests on the card tone with quiet ink.</summary>
    public void SetRested(bool value)
    {
        rested = value;
        Button.IsEnabled = !value;
        Button.Cursor = value ? Cursors.Arrow : mode == "busy" ? Cursors.Wait : Cursors.Hand;
        Recolor(true);
    }

    private void Recolor(bool animate)
    {
        var dark = Theme.IsDark;
        var plateKey = mode == "done" ? "Calm" : rested && mode == "normal" ? "Card3" : "Accent";
        var inkKey = mode == "done" ? "SiOnCalm" : rested && mode == "normal" ? "Ink3" : "AccentInk";
        if (animate) { Motion.Color(plate, Theme.Color(plateKey, dark), Motion.Med); Motion.Color(ink, Theme.Color(inkKey, dark), Motion.Med); }
        else { plate.BeginAnimation(SolidColorBrush.ColorProperty, null); plate.Color = Theme.Color(plateKey, dark); ink.BeginAnimation(SolidColorBrush.ColorProperty, null); ink.Color = Theme.Color(inkKey, dark); }
    }
}

/// <summary>The atlas motif behind the card (trays concept): nine contour rings of one summit made with the dashboard's
/// ring function at 0.62 scale, the Apex mark at the summit, 25/50/75 elevation figures and a four-segment scale bar.
/// Entrance: inner rings first, the summit, then the bar fills. Busy: a sweep runs along the bar. Success: the bar fills
/// with the Apex gradient and the contours take the accent.</summary>
internal sealed class AtlasMotif : Canvas
{
    private const double W = 420, H = 470, SummitX = 226, SummitY = 196, BaseX = 210, BaseY = 236, R0 = 178;
    private const int Rings = 9;
    private readonly List<(FrameworkElement Host, ScaleTransform Scale, int Index)> rings = new();
    private readonly List<(FrameworkElement Host, int Index)> labels = new();
    private readonly List<(Path Path, bool Index)> contours = new();
    private readonly FrameworkElement summit;
    private readonly ScaleTransform summitScale = new(1, 1);
    private readonly ScaleTransform barFill = new(1, 1), barDone = new(0, 1);
    private readonly TranslateTransform sweepShift = new(-56, 0);
    private readonly Rectangle sweep;
    private readonly ScaleTransform whole = new(1, 1, W / 2, H / 2);
    private readonly Canvas ringLayer = new(), labelLayer = new();
    private bool success;

    public AtlasMotif()
    {
        Width = W; Height = H; IsHitTestVisible = false; ClipToBounds = false;
        RenderTransform = whole;
        Children.Add(ringLayer); Children.Add(labelLayer);
        for (int i = 0; i < Rings; i++)
        {
            var points = RingPoints(i);
            var path = new Path { Data = Smooth(points), Stroke = Theme.Brush(i % 2 == 0 ? "SiCtIdx" : "SiCt"), StrokeThickness = i % 2 == 0 ? 1.3 : 1, SnapsToDevicePixels = false };
            var host = new Canvas { Width = W, Height = H };
            host.Children.Add(path);
            var scale = new ScaleTransform(1, 1, SummitX, SummitY);
            host.RenderTransform = scale;
            ringLayer.Children.Add(host);
            rings.Add((host, scale, i));
            contours.Add((path, i % 2 == 0));
            if (i is 2 or 4 or 6)
            {
                var p = points[6]; var o = RingPoints(i - 1)[6];
                var text = new TextBlock { Text = i == 2 ? "25" : i == 4 ? "50" : "75", FontFamily = Theme.Mono, FontSize = 10.5, Foreground = Theme.Brush("SiElev") };
                text.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
                var x = (p.X + o.X) / 2; var y = (p.Y + o.Y) / 2 + 3.6;
                Canvas.SetLeft(text, x - text.DesiredSize.Width / 2); Canvas.SetTop(text, y - 10.5);
                var labelHost = new Canvas { Width = W, Height = H, RenderTransform = new ScaleTransform(1, 1, SummitX, SummitY) };
                labelHost.Children.Add(text);
                labelLayer.Children.Add(labelHost);
                labels.Add((labelHost, i));
            }
        }
        // The Apex mark at the summit (30 px on the motif's 24 grid).
        summit = Icons.Logo(30, Theme.Brush("Ink"));
        summit.RenderTransformOrigin = new Point(0.5, 0.5); summit.RenderTransform = summitScale;
        Canvas.SetLeft(summit, SummitX - 15); Canvas.SetTop(summit, SummitY - 16);
        Children.Add(summit);
        // The survey scale bar: 168 x 6 with four segments (ink, paper, ink, paper) and 0..100 under it.
        const double sw = 168, sx = BaseX - sw / 2, sy = 378, sh = 6, seg = sw / 4;
        var bar = new Canvas { Width = sw, Height = sh, ClipToBounds = true };
        Canvas.SetLeft(bar, sx); Canvas.SetTop(bar, sy);
        bar.Children.Add(new Rectangle { Width = sw, Height = sh, Fill = Theme.Brush("SiSbPaper"), Stroke = Theme.Brush("SiCtIdx"), StrokeThickness = 1 });
        var fill = new Canvas { Width = sw, Height = sh, RenderTransform = barFill };
        for (int k = 0; k < 4; k++)
        {
            var alt = k % 2 == 0;
            var rect = new Rectangle { Width = seg, Height = sh, Fill = Theme.Brush(alt ? "SiSbInk" : "SiSbPaper"), Stroke = Theme.Brush(alt ? "SiSbInk" : "SiCtIdx"), StrokeThickness = 1 };
            Canvas.SetLeft(rect, k * seg); fill.Children.Add(rect);
        }
        bar.Children.Add(fill);
        var apex = new LinearGradientBrush((Color)ColorConverter.ConvertFromString("#2F6BFF"), (Color)ColorConverter.ConvertFromString("#1FC6EE"), 0); apex.Freeze();
        bar.Children.Add(new Rectangle { Width = sw, Height = sh, Fill = apex, RenderTransform = barDone });
        var sweepBrush = new LinearGradientBrush { StartPoint = new Point(0, 0.5), EndPoint = new Point(1, 0.5) };
        sweepBrush.GradientStops.Add(new GradientStop(Color.FromArgb(0, 0x2F, 0x6B, 0xFF), 0));
        sweepBrush.GradientStops.Add(new GradientStop(Color.FromArgb(0xD9, 0x2F, 0x6B, 0xFF), 0.6));
        sweepBrush.GradientStops.Add(new GradientStop((Color)ColorConverter.ConvertFromString("#1FC6EE"), 1));
        sweepBrush.Freeze();
        sweep = new Rectangle { Width = 56, Height = sh, Fill = sweepBrush, RenderTransform = sweepShift, Opacity = 0 };
        bar.Children.Add(sweep);
        Children.Add(bar);
        for (int k = 0; k <= 4; k++)
        {
            var text = new TextBlock { Text = (k * 25).ToString(CultureInfo.InvariantCulture), FontFamily = Theme.Mono, FontSize = 10.5, Foreground = Theme.Brush("SiElev") };
            text.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
            Canvas.SetLeft(text, sx + k * seg - text.DesiredSize.Width / 2); Canvas.SetTop(text, sy + 20 - 10.5);
            Children.Add(text);
        }
    }

    /// <summary>The dashboard's ring function at panel scale: a rounded triangle around the base with a slow wobble,
    /// shrinking toward the summit (12 points per ring).</summary>
    internal static Point[] RingPoints(int i)
    {
        var s = 1 - i * 0.1;
        var v = new[] { -90.0, 30, 150 }.Select(a => new Point(BaseX + Math.Cos(a * Math.PI / 180) * R0 * 1.12, BaseY + Math.Sin(a * Math.PI / 180) * R0 * 0.88)).ToArray();
        var points = new List<Point>();
        for (int e = 0; e < 3; e++)
        {
            var a = v[e]; var b = v[(e + 1) % 3];
            var ts = new[] { 0.14, 0.38, 0.62, 0.86 };
            for (int k = 0; k < 4; k++)
            {
                var t = ts[k];
                var x = a.X + (b.X - a.X) * t; var y = a.Y + (b.Y - a.Y) * t; var kk = e * 4 + k;
                var w = 0.05 * Math.Sin(kk * 1.7 + i * 0.3) + 0.024 * Math.Sin(kk * 3.3 - i * 0.5) - 0.012 * Math.Cos(kk * 0.9 + i * 0.9);
                var m = s * (1 + w * (0.55 + 0.45 * s));
                points.Add(new Point(SummitX + (x - SummitX) * m, SummitY + (y - SummitY) * m));
            }
        }
        return points.ToArray();
    }

    /// <summary>Closed Catmull-Rom through the points, as cubic Beziers (the concept's siSmooth).</summary>
    private static Geometry Smooth(Point[] p)
    {
        var n = p.Length;
        var figure = new PathFigure { StartPoint = p[0], IsClosed = true, IsFilled = false };
        for (int i = 0; i < n; i++)
        {
            Point p0 = p[(i - 1 + n) % n], p1 = p[i], p2 = p[(i + 1) % n], p3 = p[(i + 2) % n];
            figure.Segments.Add(new BezierSegment(new Point(p1.X + (p2.X - p0.X) / 6, p1.Y + (p2.Y - p0.Y) / 6), new Point(p2.X - (p3.X - p1.X) / 6, p2.Y - (p3.Y - p1.Y) / 6), p2, true));
        }
        var geometry = new PathGeometry(new[] { figure });
        geometry.Freeze();
        return geometry;
    }

    /// <summary>The entrance: each ring fades and grows from 0.94 (900 ms, ease-out), inner rings first; the figures
    /// follow; the summit scales in (a decoration, so it may spring); the bar fills from the left.</summary>
    public void Enter(bool animate)
    {
        foreach (var (host, scale, i) in rings)
        {
            var delay = (Rings - 1 - i) * 50 + 140;
            if (!animate || !Motion.Enabled) { host.BeginAnimation(OpacityProperty, null); host.Opacity = 1; scale.ScaleX = scale.ScaleY = 1; continue; }
            host.Opacity = 0; scale.ScaleX = scale.ScaleY = 0.94;
            Motion.To(host, OpacityProperty, 1, Motion.Slow, Motion.InOut, delay: delay, from: 0);
            Motion.To(scale, ScaleTransform.ScaleXProperty, 1, Motion.Draw, Motion.Out, delay: delay, from: 0.94);
            Motion.To(scale, ScaleTransform.ScaleYProperty, 1, Motion.Draw, Motion.Out, delay: delay, from: 0.94);
        }
        foreach (var (host, i) in labels)
        {
            if (!animate || !Motion.Enabled) { host.BeginAnimation(OpacityProperty, null); host.Opacity = 1; continue; }
            host.Opacity = 0;
            Motion.To(host, OpacityProperty, 1, Motion.Slow, Motion.InOut, delay: (Rings - 1 - i) * 50 + 380, from: 0);
        }
        if (!animate || !Motion.Enabled) { summit.Opacity = 1; summitScale.ScaleX = summitScale.ScaleY = success ? 1.08 : 1; barFill.ScaleX = 1; return; }
        summit.Opacity = 0; summitScale.ScaleX = summitScale.ScaleY = 0.6;
        Motion.To(summit, OpacityProperty, 1, Motion.Med, Motion.InOut, delay: 600, from: 0);
        Motion.To(summitScale, ScaleTransform.ScaleXProperty, 1, Motion.Slow, Motion.Spring, delay: 600, from: 0.6);
        Motion.To(summitScale, ScaleTransform.ScaleYProperty, 1, Motion.Slow, Motion.Spring, delay: 600, from: 0.6);
        barFill.ScaleX = 0;
        Motion.To(barFill, ScaleTransform.ScaleXProperty, 1, Motion.Draw, Motion.Out, delay: 720, from: 0);
    }

    /// <summary>While busy, a short gradient sweeps along the bar (1100 ms, repeating); it stops when idle.</summary>
    public void SetBusy(bool busy)
    {
        Motion.To(sweep, OpacityProperty, busy ? 1 : 0, Motion.Med);
        if (busy && Motion.Enabled)
            sweepShift.BeginAnimation(TranslateTransform.XProperty, new DoubleAnimation(-56, 168, TimeSpan.FromMilliseconds(1100)) { RepeatBehavior = RepeatBehavior.Forever, EasingFunction = Motion.InOut });
        else { sweepShift.BeginAnimation(TranslateTransform.XProperty, null); sweepShift.X = -56; }
    }

    /// <summary>Success: the bar fills with the Apex gradient and the contours take the accent; the summit lifts.</summary>
    public void SetSuccess(bool on, bool animate)
    {
        success = on;
        Motion.To(barDone, ScaleTransform.ScaleXProperty, on ? 1 : 0, animate ? Motion.Slow : 0, Motion.Out);
        Motion.To(summitScale, ScaleTransform.ScaleXProperty, on ? 1.08 : 1, animate ? Motion.Slow : 0, Motion.Spring);
        Motion.To(summitScale, ScaleTransform.ScaleYProperty, on ? 1.08 : 1, animate ? Motion.Slow : 0, Motion.Spring);
        foreach (var (path, index) in contours) path.Stroke = Theme.Brush(index ? (on ? "SiCtIdxOk" : "SiCtIdx") : (on ? "SiCtOk" : "SiCt"));
    }

    /// <summary>Rate limited: the contours and figures recede to half.</summary>
    public void SetDim(bool dim)
    {
        Motion.To(ringLayer, OpacityProperty, dim ? 0.5 : 1, Motion.Slow, Motion.InOut);
        Motion.To(labelLayer, OpacityProperty, dim ? 0.5 : 1, Motion.Slow, Motion.InOut);
    }

    /// <summary>The hand-off: the motif swells slightly as the screen fades.</summary>
    public void Leave()
    {
        Motion.To(whole, ScaleTransform.ScaleXProperty, 1.02, 340, Motion.Out);
        Motion.To(whole, ScaleTransform.ScaleYProperty, 1.02, 340, Motion.Out);
    }

    public void ResetLeave() { whole.BeginAnimation(ScaleTransform.ScaleXProperty, null); whole.BeginAnimation(ScaleTransform.ScaleYProperty, null); whole.ScaleX = whole.ScaleY = 1; }

    internal int RingCount => rings.Count;
}
