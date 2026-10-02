using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Documents;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Media.Effects;
using System.Windows.Shapes;
using System.Windows.Threading;

namespace CCSBar;

/// <summary>The fifteen states of the trays concept's sign-in screen (TSIGN-C, revision for local HTTP), numbered 1 to
/// 11 by the brief.</summary>
public enum SignInState
{
    FirstRun, Password, SetupCode, Pairing, NotLocal, PairingOff, WrongPassword, RateLimited, Unreachable, WrongAddress,
    Securing, SignedOut, SignedOutAll, Expired, Success,
}

public enum SignInFlow { Pair, Setup, Secure }

/// <summary>Everything a state's words depend on (the concept's S.si record). Addresses are origins as typed by the
/// user or saved by the tray; nothing here is a secret.</summary>
public sealed class SignInModel
{
    public SignInState State { get; set; } = SignInState.FirstRun;
    /// <summary>Re-pair or Change from Settings on a paired tray: the current device key keeps working until Pair is
    /// pressed, and Cancel before then changes nothing.</summary>
    public bool Repair { get; set; }
    /// <summary>Pair from Settings on a version 1 tray (a saved password, no key): the saved password keeps working
    /// until pairing finishes, and Cancel changes nothing.</summary>
    public bool Upgrade { get; set; }
    /// <summary>A working connection is waiting behind the screen (Re-pair or Upgrade), so Cancel can return to it.</summary>
    public bool HasCurrent => Repair || Upgrade;
    /// <summary>Sign in with the password the old way and keep it with DPAPI (version 1): a dashboard without pairing,
    /// or the password option while pairing is turned off.</summary>
    public bool Legacy { get; set; }
    /// <summary>Pairing is turned off and this tray holds no device key: the password sign-in is offered instead.</summary>
    public bool PasswordFallback { get; set; }
    /// <summary>A Re-pair's pair request got no answer and the current key did not answer either, so the dashboard may
    /// have replaced it.</summary>
    public bool KeyUncertain { get; set; }
    /// <summary>The dashboard address that answered as a dashboard.</summary>
    public string Verified { get; set; } = "";
    /// <summary>The address just refused: not local, unreachable or not a dashboard.</summary>
    public string Tried { get; set; } = "";
    /// <summary>The local address this tray last used (state 4's "Use this"), when it has one.</summary>
    public string? LocalAddress { get; set; }
    /// <summary>This computer as the dashboard saw it, when the dashboard refused it (state 4, the dashboard's view).</summary>
    public string? Peer { get; set; }
    public int TriesLeft { get; set; } = 5;
    public DateTimeOffset LimitUntil { get; set; }
    public TimeSpan LimitTotal { get; set; } = TimeSpan.FromMinutes(15);
    public SignInFlow Flow { get; set; } = SignInFlow.Pair;
    public int Step { get; set; }
    public DateTimeOffset? DisconnectedAt { get; set; }
    public DateTimeOffset? SignedOutAt { get; set; }
    public string? RevokedBy { get; set; }
    /// <summary>When usage last updated, for the signed-out note.</summary>
    public DateTimeOffset? LastSample { get; set; }
    /// <summary>Unreachable: no answer within the time limit (true) or refused outright (false).</summary>
    public bool TimedOut { get; set; } = true;
    public string Username { get; set; } = "";
    public string DeviceName { get; set; } = Environment.MachineName;
    /// <summary>A message under the fields that is not the state's own (a refusal with no screen of its own).</summary>
    public (string Bold, string? Sub, bool Info)? Note { get; set; }
    /// <summary>The field to mark as wrong for <see cref="Note"/>.</summary>
    public string? NoteField { get; set; }
}

/// <summary>
/// The Windows tray's sign-in screen, built as the trays concept draws it at the panel's real size: a 372 px Atlas paper
/// card on the left over a graticule, the atlas motif behind it on the right, and every state of the brief. One skeleton
/// serves every state, as the dashboard keeps one form: a state change swaps text, opens and closes regions (animated
/// height and fade) and moves the primary between its three layers, so nothing re-mounts and an error never moves
/// anything below it. The flows (what each button does) live in MainWindow.SignIn.cs.
/// </summary>
public sealed class SignInView : Grid
{
    public event Action? Submitted, AltClicked, ChangeClicked, UseLocalClicked, UsePasswordClicked, CountdownFinished;
    /// <summary>After every state change (the E2E driver records the sequence and renders each state).</summary>
    public event Action<SignInState>? StateApplied;
    public SignInModel Model { get; } = new();

    private const double CardWidth = 372, ContentWidth = CardWidth - 2 - 52;
    private readonly Border card;
    private readonly TranslateTransform cardShift = new();
    private readonly ScaleTransform cardScale = new(1, 1);
    private readonly AtlasMotif motif;
    private readonly TextBlock title;
    private readonly ContentControl lede;
    private readonly Collapsible bannerRegion, addrRegion, addrHintRegion, guideRegion, credsRegion, setupRegion, deviceRegion, stepsRegion, actsRegion;
    private readonly ContentControl bannerHost = new() { Focusable = false }, guideHost = new() { Focusable = false };
    private readonly StackPanel stepsHost = new() { Margin = new Thickness(0, 2, 0, 6) };
    private readonly TextBlock deviceText;
    internal readonly SiField Addr, User, Pass, Confirm, Code;
    private readonly TranslateTransform shakeShift = new();
    private readonly Grid msg;
    private readonly ContentControl msgIcon = new() { Focusable = false };
    private readonly TextBlock msgText;
    private readonly TranslateTransform msgShift = new();
    private readonly SiPrimary primary = new();
    private readonly Button alt;
    private readonly Grid acts;
    private readonly Border foot;
    private readonly ContentControl footIcon = new() { Focusable = false };
    private readonly TextBlock footText;
    private readonly FrameworkElement head, bannerWrap, form;
    private readonly Grid strengthTrack;
    private readonly Border strengthFill;
    private readonly TextBlock strengthWord, strengthHint;
    private readonly DispatcherTimer countdown = new() { Interval = TimeSpan.FromSeconds(1) };
    private TextBlock? countNumber;
    private ScaleTransform? countDrain;
    private string? stepsKey;
    private readonly List<StepRow> stepRows = new();
    private string? msgShown;
    private bool capsShown;

    public SignInView()
    {
        Background = Theme.Brush("Panel");
        ClipToBounds = true;
        Uid = "signin";
        // The Atlas graticule: 24 px minor and 96 px major lines, tiled from the screen's top left.
        Children.Add(new Rectangle { Fill = Graticule(), IsHitTestVisible = false });
        motif = new AtlasMotif { HorizontalAlignment = HorizontalAlignment.Right, VerticalAlignment = VerticalAlignment.Center };
        var motifHost = new Grid { HorizontalAlignment = HorizontalAlignment.Right, VerticalAlignment = VerticalAlignment.Center, Width = 420, Height = 470, IsHitTestVisible = false };
        motifHost.Children.Add(motif);
        Panel.SetZIndex(motifHost, 1);
        Children.Add(motifHost);

        var stack = new StackPanel();
        // Head: the title (23 px, the SemiCondensed face) and the lede.
        title = new TextBlock { FontFamily = Theme.Numerals, FontSize = 23, FontWeight = FontWeights.SemiBold, Foreground = Theme.Brush("Ink"), TextWrapping = TextWrapping.Wrap, LineHeight = 26.5, LineStackingStrategy = LineStackingStrategy.BlockLineHeight, Uid = "si-title" };
        lede = new ContentControl { Margin = new Thickness(0, 6, 0, 0), Focusable = false, Uid = "si-lede" };
        var headStack = new StackPanel { Margin = new Thickness(0, 0, 0, 16) };
        headStack.Children.Add(title); headStack.Children.Add(lede);
        head = headStack;
        stack.Children.Add(head);
        bannerRegion = new Collapsible(bannerHost, ContentWidth);
        bannerWrap = new Border { Child = bannerRegion };
        stack.Children.Add(bannerWrap);

        // The form: fields and regions inside the shake, the message slot, the buttons.
        var formStack = new StackPanel();
        var shake = new StackPanel { RenderTransform = shakeShift };
        Addr = new SiField("addr", "Dashboard address", placeholderText: "http://");
        addrRegion = new Collapsible(Addr, ContentWidth);
        var addrHint = Hint();
        addrHint.Inlines.Add(new Run("The dashboard shows it under ")); addrHint.Inlines.Add(new Run("Settings › Dashboard sign-in") { }); addrHint.Inlines.Add(new Run("."));
        addrHintRegion = new Collapsible(addrHint, ContentWidth);
        guideRegion = new Collapsible(guideHost, ContentWidth);
        User = new SiField("user", "Username");
        Pass = new SiField("pass", "Password", password: true);
        var creds = new StackPanel(); creds.Children.Add(User); creds.Children.Add(Pass);
        credsRegion = new Collapsible(creds, ContentWidth);
        // Setup: the strength meter under the password, then confirm and the one-time code.
        var setup = new StackPanel();
        strengthTrack = new Grid { Height = 5, ClipToBounds = true };
        strengthTrack.Children.Add(new Border { Background = Theme.Brush("Track"), CornerRadius = new CornerRadius(3) });
        strengthFill = new Border { CornerRadius = new CornerRadius(3), HorizontalAlignment = HorizontalAlignment.Left, Width = 0, Background = Theme.Gradient("crit") };
        strengthTrack.Children.Add(strengthFill);
        foreach (var at in new[] { 0.25, 0.5, 0.75 })
            strengthTrack.Children.Add(new Border { Width = 1, Margin = new Thickness(ContentWidth * at, 1, 0, 1), HorizontalAlignment = HorizontalAlignment.Left, Background = Theme.Brush("TrackTick") });
        var strengthRow = new DockPanel { Margin = new Thickness(0, 5, 0, 0), LastChildFill = true };
        strengthWord = Ui.Text("", 12, "Ink3", FontWeights.SemiBold); DockPanel.SetDock(strengthWord, Dock.Right); strengthRow.Children.Add(strengthWord);
        strengthHint = Ui.Text("At least 8 characters. Longer is stronger.", 11.5, "Ink3", trim: true); strengthRow.Children.Add(strengthHint);
        var strength = new StackPanel { Margin = new Thickness(0, -4, 0, 12), Uid = "si-strength" };
        strength.Children.Add(strengthTrack); strength.Children.Add(strengthRow);
        setup.Children.Add(strength);
        Confirm = new SiField("confirm", "Confirm password", password: true);
        setup.Children.Add(Confirm);
        Code = new SiField("code", "Setup code", placeholderText: "XXXX-XXXX", mono: true);
        Code.Aside.Text = "printed in the server's terminal";
        setup.Children.Add(Code);
        var codeHint = Hint();
        codeHint.Inlines.Add(new Run("Also saved on the server in ")); codeHint.Inlines.Add(Mono("~/.ccs/auth/setup-code"));
        setup.Children.Add(codeHint);
        setupRegion = new Collapsible(setup, ContentWidth);
        deviceText = Hint(); deviceText.Margin = new Thickness(0, -2, 0, 12);
        deviceRegion = new Collapsible(deviceText, ContentWidth);
        stepsRegion = new Collapsible(stepsHost, ContentWidth);
        foreach (var region in new[] { addrRegion, addrHintRegion, guideRegion, credsRegion, setupRegion, deviceRegion, stepsRegion }) shake.Children.Add(region);
        formStack.Children.Add(shake);

        msgText = new TextBlock { FontSize = 12, TextWrapping = TextWrapping.Wrap, LineHeight = 17.4, LineStackingStrategy = LineStackingStrategy.BlockLineHeight };
        var msgRow = new Grid { RenderTransform = msgShift, Opacity = 0 };
        msgRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(23) });
        msgRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        msgIcon.VerticalAlignment = VerticalAlignment.Top; msgIcon.Margin = new Thickness(0, 1, 0, 0);
        msgRow.Children.Add(msgIcon);
        Grid.SetColumn(msgText, 1); msgRow.Children.Add(msgText);
        msg = new Grid { MinHeight = 34, Margin = new Thickness(0, 0, 0, 10), Uid = "si-msg" };
        msg.Children.Add(msgRow);
        formStack.Children.Add(msg);

        alt = new Button { Style = (Style)Application.Current.FindResource("AtlasButton"), Height = 36, FontSize = 13, Padding = new Thickness(16, 0, 16, 0), Content = "Cancel", Margin = new Thickness(8, 0, 0, 0), Uid = "si-alt" };
        alt.Click += (_, _) => AltClicked?.Invoke();
        primary.Button.Click += (_, _) => { if (primary.Mode == "normal" && primary.Button.IsEnabled) Submitted?.Invoke(); };
        acts = new Grid { Margin = new Thickness(0, 1, 0, 1) };
        acts.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        acts.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        acts.Children.Add(primary.Button);
        Grid.SetColumn(alt, 1); acts.Children.Add(alt);
        actsRegion = new Collapsible(acts, ContentWidth);
        formStack.Children.Add(actsRegion);
        form = formStack;
        stack.Children.Add(form);

        footText = new TextBlock { FontSize = 12, Foreground = Theme.Brush("Ink3"), TextWrapping = TextWrapping.Wrap, LineHeight = 17.4, LineStackingStrategy = LineStackingStrategy.BlockLineHeight };
        var footRow = new Grid();
        footRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(23) });
        footRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        footIcon.VerticalAlignment = VerticalAlignment.Top; footIcon.Margin = new Thickness(0, 1, 0, 0);
        footRow.Children.Add(footIcon); Grid.SetColumn(footText, 1); footRow.Children.Add(footText);
        foot = new Border { BorderBrush = Theme.Brush("Rule2"), BorderThickness = new Thickness(0, 1, 0, 0), Margin = new Thickness(0, 14, 0, 0), Padding = new Thickness(0, 12, 0, 0), Child = footRow, Uid = "si-foot" };
        stack.Children.Add(foot);

        card = new Border
        {
            Width = CardWidth, Background = Theme.Brush("Card"), BorderBrush = Theme.Brush("Rule"), BorderThickness = new Thickness(1), CornerRadius = new CornerRadius(8),
            Padding = new Thickness(26, 24, 26, 20), Child = stack, HorizontalAlignment = HorizontalAlignment.Left, VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(28, 16, 28, 16), SnapsToDevicePixels = true, Uid = "si-card",
            RenderTransformOrigin = new Point(0.5, 0.5), RenderTransform = new TransformGroup { Children = { cardScale, cardShift } },
        };
        PaintShadow();
        Theme.Changed += PaintShadow;
        Panel.SetZIndex(card, 2);
        Children.Add(card);

        // Typing in a field marked wrong clears it and the message; the setup fields repaint the strength meter.
        foreach (var field in Fields())
        {
            var f = field;
            f.Edited += () =>
            {
                if (f.Bad) { f.Bad = false; HideMessage(); }
                if (Model.State == SignInState.SetupCode && f.Key is "pass" or "confirm") PaintStrength();
            };
            if (f.Key is "pass" or "confirm")
            {
                f.FocusChanged += _ => PaintCaps();
                f.PreviewKeyUp += (_, _) => PaintCaps();
            }
        }
        countdown.Tick += (_, _) => PaintCountdown();
    }

    private IEnumerable<SiField> Fields() => new[] { Addr, User, Pass, Confirm, Code };

    // ------------------------------------------------------------------ small builders

    private static DrawingBrush Graticule()
    {
        var minor = new GeometryGroup(); var major = new GeometryGroup();
        for (int k = 0; k < 4; k++)
        {
            var at = k * 24 + 0.5;
            (k == 0 ? major : minor).Children.Add(new LineGeometry(new Point(at, 0), new Point(at, 96)));
            (k == 0 ? major : minor).Children.Add(new LineGeometry(new Point(0, at), new Point(96, at)));
        }
        // The concept draws the minor pattern under the major one, so the major lines also carry a minor line.
        minor.Children.Add(new LineGeometry(new Point(0.5, 0), new Point(0.5, 96)));
        minor.Children.Add(new LineGeometry(new Point(0, 0.5), new Point(96, 0.5)));
        var drawing = new DrawingGroup();
        drawing.Children.Add(new GeometryDrawing(Brushes.Transparent, null, new RectangleGeometry(new Rect(0, 0, 96, 96))));
        drawing.Children.Add(new GeometryDrawing(null, new Pen(Theme.Brush("PgMinor"), 1), minor));
        drawing.Children.Add(new GeometryDrawing(null, new Pen(Theme.Brush("PgMajor"), 1), major));
        return new DrawingBrush(drawing) { TileMode = TileMode.Tile, Viewport = new Rect(0, 0, 96, 96), ViewportUnits = BrushMappingMode.Absolute, Viewbox = new Rect(0, 0, 96, 96), ViewboxUnits = BrushMappingMode.Absolute, Stretch = Stretch.None, AlignmentX = AlignmentX.Left, AlignmentY = AlignmentY.Top };
    }

    private void PaintShadow()
    {
        var dark = Theme.IsDark;
        card.Effect = new DropShadowEffect { Color = dark ? Colors.Black : (Color)ColorConverter.ConvertFromString("#15202B"), BlurRadius = dark ? 36 : 32, ShadowDepth = dark ? 14 : 12, Direction = 270, Opacity = dark ? 0.4 : 0.09, RenderingBias = RenderingBias.Performance };
    }

    private static TextBlock Hint() => new() { FontSize = 12, Foreground = Theme.Brush("Ink3"), TextWrapping = TextWrapping.Wrap, LineHeight = 17.4, LineStackingStrategy = LineStackingStrategy.BlockLineHeight, Margin = new Thickness(0, -4, 0, 12) };

    private static Run Bold(string text, string ink = "Ink2") => new(text) { FontWeight = FontWeights.SemiBold, Foreground = Theme.Brush(ink) };
    private static Run Mono(string text) => new(text) { FontFamily = Theme.Mono, FontSize = 11, Foreground = Theme.Brush("Ink2") };

    private static string HostOf(string address)
    {
        var text = (address ?? "").Trim();
        var scheme = text.IndexOf("://", StringComparison.Ordinal);
        if (scheme >= 0) text = text[(scheme + 3)..];
        var slash = text.IndexOf('/');
        return slash >= 0 ? text[..slash] : text;
    }

    internal static string When(DateTimeOffset at)
    {
        var local = at.ToLocalTime();
        var clock = local.ToString("h:mm tt", CultureInfo.CurrentCulture);
        return local.Date == DateTime.Now.Date ? "today at " + clock : "on " + local.ToString("MMM d", CultureInfo.CurrentCulture) + " at " + clock;
    }

    /// <summary>The verified dashboard on one line with Change at its end (the concept's atLine).</summary>
    private FrameworkElement AtLine(bool sub = false)
    {
        var line = new DockPanel { LastChildFill = true, Margin = new Thickness(0, sub ? 8 : 0, 0, 0) };
        var change = new TextBlock { FontSize = 13, Margin = new Thickness(10, 0, 0, 0), VerticalAlignment = VerticalAlignment.Bottom };
        var link = new Hyperlink(new Run("Change")) { Foreground = Theme.Brush("AccentText"), TextDecorations = null, Cursor = Cursors.Hand, Focusable = true };
        link.Click += (_, _) => ChangeClicked?.Invoke();
        System.Windows.Automation.AutomationProperties.SetName(link, "Change the dashboard address");
        change.Inlines.Add(link);
        DockPanel.SetDock(change, Dock.Right); line.Children.Add(change);
        var host = new TextBlock { FontSize = 13, TextTrimming = TextTrimming.CharacterEllipsis, VerticalAlignment = VerticalAlignment.Bottom, ToolTip = Ui.Tip(Model.Verified) };
        host.Inlines.Add(Bold(HostOf(Model.Verified)));
        line.Children.Add(host);
        return line;
    }

    private static TextBlock Lede(params Inline[] inlines)
    {
        var text = new TextBlock { FontSize = 13, Foreground = Theme.Brush("Ink3"), TextWrapping = TextWrapping.Wrap, LineHeight = 18.85, LineStackingStrategy = LineStackingStrategy.BlockLineHeight };
        text.Inlines.AddRange(inlines);
        return text;
    }

    // ------------------------------------------------------------------ the states

    private sealed record BannerSpec(string Icon, string Tone, string Title, Func<IEnumerable<Inline>> Body, bool Countdown = false);
    private sealed record StepSpec(string Title, Func<IEnumerable<Inline>> Sub);
    private sealed record MsgSpec(string Icon, string? Bold, string? Sub, bool Info = false);
    private sealed record PrimarySpec(string Label, string Busy, string Done);

    private sealed class Spec
    {
        public string Title = "";
        public Func<FrameworkElement>? Lede;
        public BannerSpec? Banner;
        public bool Addr, AddrHint, Guide, Creds, Setup, Device, Busy, Dim, Disabled, Shake;
        public StepSpec[]? Steps;
        public MsgSpec? Msg;
        public PrimarySpec? Primary;
        public string? Alt;
        public (string Icon, string Text)? Foot;
        public string? Bad;
    }

    private const string Where = "Settings › Dashboard sign-in";
    private const string KeyLine = "The device key is kept with Windows data protection, readable only by your Windows account.";
    private const string LanNote = "Your password is sent once over your local network to pair this tray, then forgotten. The tray keeps a device key with Windows data protection, readable only by your Windows account.";
    private const string LegacyNote = "The tray keeps your password with Windows data protection, readable only by your Windows account, and trades it for a device key once the dashboard can pair it.";

    private StepSpec[] StepsFor(SignInFlow flow)
    {
        var host = HostOf(Model.Verified);
        if (flow == SignInFlow.Secure) return new[]
        {
            new StepSpec("Rollback copy kept", () => new Inline[] { Mono("connection.v1-rollback.dpapi"), new Run(", until the new key has worked once") }),
            new StepSpec("Device key issued", () => new Inline[] { new Run("For Windows tray on " + Model.DeviceName + ", traded once for the saved password") }),
            new StepSpec("Checking that the key works", () => new Inline[] { new Run("One request to the dashboard with the new key") }),
            new StepSpec("Deleting the saved password", () => new Inline[] { new Run("And the rollback copy") }),
        };
        return new[]
        {
            flow == SignInFlow.Setup
                ? new StepSpec("Sign-in created", () => new Inline[] { new Run("Username "), Bold(Model.Username), new Run(" on " + host) })
                : new StepSpec("Password checked", () => new Inline[] { new Run("Sent once to " + host) }),
            new StepSpec("Device key issued", () => new Inline[] { new Run("For Windows tray on " + Model.DeviceName + "; the dashboard keeps only a hash of it") }),
            new StepSpec("Saving the key", () => new Inline[] { new Run("With Windows data protection (DPAPI) for your account, in "), Mono(@"%LOCALAPPDATA%\CCS Bar\connection.dpapi") }),
            new StepSpec("Forgetting the password", () => new Inline[] { new Run("It is never written to disk") }),
        };
    }

    /// <summary>Re-pair: the current key works until Pair is pressed (the dashboard replaces it as soon as it answers).
    /// Upgrade: the saved password works until pairing finishes. Neither shows for the password sign-in.</summary>
    private BannerSpec? RepairBanner()
    {
        if (Model.Legacy) return null;
        if (Model.Repair) return new BannerSpec("key", "info", "Re-pairing replaces this tray's device key", () => new Inline[] { new Run("The current key keeps working until you press Pair. Cancel before then changes nothing.") });
        if (Model.Upgrade) return new BannerSpec("key", "info", "Pairing replaces the saved password", () => new Inline[] { new Run("This tray keeps signing in with its saved password until it is paired, so Cancel changes nothing.") });
        return null;
    }

    private Spec SpecFor(SignInState state)
    {
        var sp = new Spec();
        var cancel = Model.HasCurrent ? "Cancel" : null;
        var firstFoot = ("house", "Pairing works from your home network, or over your home VPN.");
        void Password()
        {
            sp.Title = Model.Legacy ? "Sign in with your password" : Model.Repair ? "Sign in to re-pair" : Model.Upgrade ? "Sign in to pair this tray" : "Sign in to pair";
            sp.Lede = () => AtLine();
            sp.Banner = RepairBanner(); sp.Creds = true; sp.Device = !Model.Legacy;
            sp.Primary = Model.Legacy ? new PrimarySpec("Sign in", "Checking password", "Signed in") : new PrimarySpec("Pair this PC", "Checking password", "Paired");
            sp.Alt = cancel;
            sp.Foot = ("key", Model.Legacy ? LegacyNote : LanNote);
        }
        switch (state)
        {
            case SignInState.FirstRun:
                sp.Title = Model.HasCurrent ? "Change the address" : "Connect this PC";
                sp.Lede = () => Lede(new Run("Enter the address of your AI Account Center dashboard. You sign in once; this tray then keeps its own device key."));
                sp.Banner = Model.DisconnectedAt is DateTimeOffset gone
                    ? new BannerSpec("signOut", "info", "Disconnected at " + Formatting.Clock(gone), () => new Inline[] { new Run("This tray forgot its device key and the dashboard revoked it. Pair again to see usage.") })
                    : RepairBanner();
                sp.Addr = true; sp.AddrHint = true; sp.Primary = new PrimarySpec("Continue", "Checking address", "Done"); sp.Alt = cancel; sp.Foot = firstFoot;
                break;
            case SignInState.Password: Password(); break;
            case SignInState.WrongPassword:
                Password();
                sp.Bad = "pass"; sp.Shake = true;
                sp.Msg = new MsgSpec("err", "Username or password isn't right.", $"{Model.TriesLeft} {(Model.TriesLeft == 1 ? "try" : "tries")} left before pairing pauses for 15 minutes.");
                break;
            case SignInState.RateLimited:
                Password();
                sp.Banner = new BannerSpec("timer", "warn", "Try again in " + Minutes(Model.LimitTotal), () => new Inline[]
                {
                    new Run("Too many tries from this computer. Pairing, and sign-in from this computer's browser, open again at "),
                    Bold(Formatting.Clock(Model.LimitUntil), "Ink"), new Run("."),
                }, Countdown: true);
                sp.Disabled = true; sp.Dim = true;
                break;
            case SignInState.SetupCode:
                sp.Title = "Set up sign-in";
                sp.Lede = () => Lede(Bold(HostOf(Model.Verified)), new Run(" has no sign-in yet. Choose one here; your browser and both trays use it."));
                sp.Creds = true; sp.Setup = true; sp.Primary = new PrimarySpec("Create sign-in and pair", "Creating sign-in", "Paired");
                sp.Foot = ("terminal", "The setup code proves you can reach the dashboard machine. It works once and expires after 60 minutes.");
                break;
            case SignInState.Pairing:
                sp.Title = "Pairing this PC";
                sp.Lede = () => Lede(Bold(HostOf(Model.Verified)), new Run(" is issuing this PC its own device key."));
                sp.Steps = StepsFor(Model.Flow); sp.Busy = true; sp.Primary = new PrimarySpec("Pair this PC", "Pairing", "Paired");
                sp.Foot = ("key", LanNote);
                break;
            case SignInState.Securing:
                sp.Title = "Securing this tray";
                sp.Lede = () => Lede(new Run("An earlier version kept your dashboard password here. The tray is trading it for a device key, once, by itself."));
                sp.Steps = StepsFor(SignInFlow.Secure); sp.Busy = true;
                sp.Foot = ("key", "There is nothing to do. " + KeyLine);
                break;
            case SignInState.NotLocal:
                sp.Title = "This address isn't on your local network";
                sp.Lede = Model.Peer is { Length: > 0 } peer
                    ? () => Lede(new Run("The dashboard sees this PC at "), Bold(peer), new Run(", which isn't on your local network. Pairing sends your password over plain HTTP, so it works only from your home network or your home VPN."))
                    : () => Lede(Bold(HostOf(Model.Tried)), new Run(" is a public address. Pairing sends your password over plain HTTP, so it works only from your home network or your home VPN."));
                sp.Addr = true; sp.Guide = true; sp.Bad = "addr"; sp.Primary = new PrimarySpec("Try again", "Checking address", "Done"); sp.Alt = cancel;
                sp.Foot = ("lock", "Nothing was sent: the tray checks the address before it asks for your password.");
                break;
            case SignInState.PairingOff:
                sp.Title = "Pairing is turned off for remote computers";
                sp.Lede = () =>
                {
                    var both = new StackPanel();
                    both.Children.Add(Lede(new Run("On the dashboard, open " + Where + " and turn on "), Bold("Trust this local network"), new Run(".")));
                    both.Children.Add(AtLine(sub: true));
                    if (Model.PasswordFallback) both.Children.Add(PasswordOption());
                    return both;
                };
                sp.Banner = RepairBanner(); sp.Primary = new PrimarySpec("Try again", "Checking", "Done"); sp.Alt = cancel;
                sp.Foot = ("info", "Only a browser on the dashboard machine itself can turn it on.");
                break;
            case SignInState.Unreachable:
                sp.Title = "Can't reach that address";
                sp.Lede = () => Lede(Bold(HostOf(Model.Tried)), new Run(" didn't answer."));
                // "Unchanged" only while that is known: a pair request that got no answer may have replaced the key.
                sp.Banner = Model.KeyUncertain
                    ? new BannerSpec("shield", "warn", "The current key may have been replaced", () => new Inline[] { new Run("The dashboard stopped answering after the pair request was sent. Retry to pair again. If the key was replaced, this tray signs out at its next refresh.") })
                    : Model.HasCurrent && Model.LocalAddress is { Length: > 0 } current
                    ? new BannerSpec("shield", "info", "Your current connection is unchanged", () => new Inline[] { new Run("Nothing was saved. This tray keeps using "), Bold(HostOf(current), "Ink"), new Run(" until a new address answers and pairs.") })
                    : null;
                sp.Addr = true;
                sp.Msg = Model.TimedOut ? new MsgSpec("err", "No answer after 10 seconds.", "Check the address, and that the dashboard is running.")
                    : new MsgSpec("err", "Nothing answered at that address.", "Check the address, and that the dashboard is running.");
                sp.Primary = new PrimarySpec("Retry", "Checking", "Done"); sp.Alt = !Model.HasCurrent ? null : Model.KeyUncertain ? "Back to usage" : "Keep current connection";
                sp.Foot = ("lock", "The tray checks an address before it saves anything.");
                break;
            case SignInState.WrongAddress:
                sp.Title = "That isn't a dashboard address";
                sp.Lede = () => Lede(Bold(HostOf(Model.Tried)), new Run(" answered, but not as AI Account Center."));
                sp.Addr = true; sp.Bad = "addr";
                sp.Msg = new MsgSpec("err", "Check the host name and the port.", "The dashboard shows its address under " + Where + ".");
                sp.Primary = new PrimarySpec("Retry", "Checking", "Done"); sp.Alt = cancel; sp.Foot = firstFoot;
                break;
            case SignInState.SignedOut:
            case SignInState.SignedOutAll:
            case SignInState.Expired:
                sp.Title = "This tray was signed out";
                var at = Model.SignedOutAt is DateTimeOffset when ? ", " + When(when) : "";
                var by = Model.RevokedBy is { Length: > 0 } who ? who : null;
                sp.Lede = state switch
                {
                    SignInState.SignedOut => () => by is null ? Lede(new Run("Revoked from the dashboard" + at + "."))
                        : Lede(new Run("Revoked from the dashboard by "), Bold(by), new Run(at + ".")),
                    SignInState.SignedOutAll => () => by is null ? Lede(new Run("Signed out with Sign out all devices in the dashboard" + at + "."))
                        : Lede(Bold(by), new Run(" chose Sign out all devices in the dashboard" + at + ".")),
                    _ => () => Lede(new Run("Not used for 90 days, so its device key expired.")),
                };
                sp.Creds = true; sp.Device = true; sp.Primary = new PrimarySpec("Pair again", "Checking password", "Paired");
                sp.Foot = ("signOut", state == SignInState.Expired || Model.LastSample is null ? "The tray won't retry on its own."
                    : "Usage stopped updating at " + Formatting.Clock(Model.LastSample.Value) + ". The tray won't retry on its own.");
                break;
            case SignInState.Success:
                sp.Title = Model.Flow == SignInFlow.Secure ? "This tray is secured" : "Paired";
                sp.Lede = () => Lede(new Run("Opening your accounts."));
                sp.Steps = StepsFor(Model.Flow);
                sp.Primary = Model.Flow == SignInFlow.Secure ? null : new PrimarySpec("Pair this PC", "Pairing", "Paired");
                sp.Foot = ("key", "The device key is saved and the password is gone.");
                break;
        }
        if (Model.Note is { } note) { sp.Msg = new MsgSpec(note.Info ? "info" : "err", note.Bold, note.Sub, note.Info); sp.Bad = Model.NoteField ?? sp.Bad; }
        return sp;
    }

    private static string Minutes(TimeSpan span) => span.TotalMinutes >= 1.5 ? $"{Math.Round(span.TotalMinutes)} minutes" : "a minute";

    // ------------------------------------------------------------------ applying a state

    /// <summary>The title and the primary's label for the current state (checks and E2E read these).</summary>
    internal string TitleText => title.Text;
    internal string PrimaryLabel => primary.Label;
    internal string PrimaryMode => primary.Mode;
    internal bool PrimaryEnabled => primary.Button.IsEnabled;
    internal Button PrimaryButton => primary.Button;
    internal Button AltButton => alt;
    internal string MessageText => msgShown ?? "";
    internal string FootText => footText.Text;
    internal string LedeText => TextOf(lede.Content as DependencyObject);
    internal string BannerText => bannerRegion.IsOpen ? TextOf(bannerHost.Content as DependencyObject) : "";
    internal bool RegionOpen(string key) => key switch
    {
        "banner" => bannerRegion.IsOpen, "addr" => addrRegion.IsOpen, "guide" => guideRegion.IsOpen, "creds" => credsRegion.IsOpen,
        "setup" => setupRegion.IsOpen, "device" => deviceRegion.IsOpen, "steps" => stepsRegion.IsOpen, "acts" => actsRegion.IsOpen, _ => false,
    };
    internal Border Card => card;
    internal int StepsDone => stepRows.Count(row => row.Done);
    internal bool AltVisible => alt.Visibility == Visibility.Visible;
    /// <summary>From the moment a pair request is sent until its key is saved or refused, Cancel rests: the dashboard
    /// may already have replaced the current key, so nothing may walk away from its answer.</summary>
    internal bool AltHeld { get => !alt.IsEnabled; set => alt.IsEnabled = !value; }

    private static string TextOf(DependencyObject? element)
    {
        if (element is null) return "";
        var text = new StringBuilder();
        void Walk(DependencyObject node)
        {
            if (node is TextBlock block) { text.Append(new TextRange(block.ContentStart, block.ContentEnd).Text).Append(' '); return; }
            foreach (var child in LogicalTreeHelper.GetChildren(node).OfType<DependencyObject>()) Walk(child);
        }
        Walk(element);
        return text.ToString().Trim();
    }

    /// <summary>Shows a state. Instant for a mount; otherwise text cross-fades, regions animate, the button moves
    /// between its layers and, for a repeated refusal or a wrong password, the fields shake.</summary>
    public void Apply(SignInState state, bool instant = false)
    {
        var repeat = state == Model.State && !instant;
        Model.State = state;
        var sp = SpecFor(state);
        countdown.Stop();
        SwapText(title, sp.Title, instant);
        SwapContent(lede, sp.Lede?.Invoke(), instant);
        if (sp.Banner is not null) SwapContent(bannerHost, Banner(sp.Banner), instant || !bannerRegion.IsOpen);
        if (sp.Guide) guideHost.Content = Guide();
        if (sp.Steps is not null) BuildSteps(sp.Steps);
        PaintSteps(state == SignInState.Success ? stepRows.Count : Model.Step);
        bannerRegion.Set(sp.Banner is not null, instant);
        addrRegion.Set(sp.Addr, instant); addrHintRegion.Set(sp.AddrHint, instant); guideRegion.Set(sp.Guide, instant);
        credsRegion.Set(sp.Creds, instant); setupRegion.Set(sp.Setup, instant); deviceRegion.Set(sp.Device, instant);
        stepsRegion.Set(sp.Steps is not null, instant); actsRegion.Set(sp.Primary is not null, instant);
        deviceText.Inlines.Clear(); deviceText.Inlines.Add(new Run("Pairs as ")); deviceText.Inlines.Add(Bold("Windows tray", "Ink")); deviceText.Inlines.Add(new Run(" on " + Model.DeviceName));
        foreach (var field in Fields()) field.Bad = field.Key == sp.Bad;
        var off = sp.Disabled || sp.Busy || state == SignInState.Success;
        foreach (var field in Fields()) field.Off = off;
        User.Aside.Text = state == SignInState.SetupCode ? "letters, numbers, - and _" : "";
        if (state == SignInState.SetupCode) PaintStrength();
        ShowMessage(sp.Msg, instant);
        if (sp.Primary is not null) primary.SetLabels(sp.Primary.Label, sp.Primary.Busy, sp.Primary.Done);
        primary.SetMode(state == SignInState.Success ? "done" : sp.Busy ? "busy" : "normal", instant);
        primary.SetRested(sp.Disabled);
        primary.Button.IsDefault = sp.Primary is not null && !sp.Disabled;
        alt.Visibility = sp.Alt is null ? Visibility.Collapsed : Visibility.Visible;
        if (sp.Alt is not null) alt.Content = sp.Alt;
        Grid.SetColumnSpan(primary.Button, sp.Alt is null ? 2 : 1);
        SwapFoot(sp.Foot, instant);
        motif.SetBusy(sp.Busy);
        motif.SetDim(sp.Dim);
        motif.SetSuccess(state == SignInState.Success, !instant);
        if (state == SignInState.RateLimited) { PaintCountdown(); countdown.Start(); }
        if (sp.Shake && !instant || repeat && sp.Bad is not null && state is SignInState.NotLocal or SignInState.WrongAddress) Shake();
        capsShown = false;
        StateApplied?.Invoke(state);
    }

    /// <summary>Busy without a state change: the button shows its progress layer, the fields rest and the bar sweeps.</summary>
    public void SetBusy(bool busy)
    {
        primary.SetMode(busy ? "busy" : "normal", false);
        foreach (var field in Fields()) field.Off = busy;
        motif.SetBusy(busy);
    }

    /// <summary>A local refusal (an empty or malformed field): the field turns red, the message says why, it shakes.</summary>
    public void Fail(string field, string text)
    {
        foreach (var each in Fields()) each.Bad = each.Key == field;
        ShowMessage(new MsgSpec("err", null, text), false);
        Shake();
        Fields().FirstOrDefault(each => each.Key == field)?.FocusInput(false);
    }

    public void SetStep(int step) { Model.Step = step; PaintSteps(step); }

    public void FocusFor(SignInState state)
    {
        var target = state switch
        {
            SignInState.FirstRun or SignInState.NotLocal or SignInState.Unreachable or SignInState.WrongAddress => Addr,
            SignInState.Password or SignInState.SetupCode => User.Value.Length > 0 && state == SignInState.Password ? Pass : User,
            SignInState.WrongPassword or SignInState.SignedOut or SignInState.SignedOutAll or SignInState.Expired => Pass,
            _ => null,
        };
        if (target is not null && !target.Off) target.FocusInput(state == SignInState.WrongPassword);
        else if (state == SignInState.PairingOff) primary.Button.Focus();
    }

    private void SwapText(TextBlock block, string text, bool instant)
    {
        if (block.Text == text) return;
        if (instant || !Motion.Enabled || !block.IsLoaded) { block.BeginAnimation(OpacityProperty, null); block.Opacity = 1; block.Text = text; return; }
        Motion.To(block, OpacityProperty, 0, 150, Motion.InOut, completed: (_, _) => { block.Text = text; Motion.To(block, OpacityProperty, 1, 160, Motion.InOut); });
    }

    private static void SwapContent(ContentControl host, object? content, bool instant)
    {
        if (instant || !Motion.Enabled || !host.IsLoaded || host.Content is null) { host.BeginAnimation(OpacityProperty, null); host.Opacity = 1; host.Content = content; return; }
        Motion.To(host, OpacityProperty, 0, 150, Motion.InOut, completed: (_, _) => { host.Content = content; Motion.To(host, OpacityProperty, 1, 160, Motion.InOut); });
    }

    private void SwapFoot((string Icon, string Text)? next, bool instant)
    {
        foot.Visibility = next is null ? Visibility.Collapsed : Visibility.Visible;
        if (next is not { } value) return;
        void Set() { footIcon.Content = SignInIcons.Icon(value.Icon, 15, Theme.Brush("Ink3")); footText.Text = value.Text; }
        if (footText.Text == value.Text) return;
        if (instant || !Motion.Enabled || !foot.IsLoaded) { Set(); return; }
        Motion.To(foot.Child, OpacityProperty, 0, 150, Motion.InOut, completed: (_, _) => { Set(); Motion.To(foot.Child, OpacityProperty, 1, 160, Motion.InOut); });
    }

    private FrameworkElement Banner(BannerSpec spec)
    {
        var tone = spec.Tone switch { "warn" => "WarnText", "crit" => "CritText", _ => "AccentText" };
        var grid = new Grid { Uid = "si-banner" };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(28) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        var icon = SignInIcons.Icon(spec.Icon, 17, Theme.Brush(tone), Theme.Brush("Card")); icon.VerticalAlignment = VerticalAlignment.Top; icon.HorizontalAlignment = HorizontalAlignment.Left; icon.Margin = new Thickness(0, 1, 0, 0);
        grid.Children.Add(icon);
        var words = new StackPanel();
        words.Children.Add(Ui.Text(spec.Title, 13, "Ink", FontWeights.SemiBold, wrap: true));
        var body = new TextBlock { FontSize = 12.5, Foreground = Theme.Brush("Ink2"), TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 2, 0, 0), LineHeight = 18.1, LineStackingStrategy = LineStackingStrategy.BlockLineHeight };
        body.Inlines.AddRange(spec.Body());
        words.Children.Add(body);
        Grid.SetColumn(words, 1); grid.Children.Add(words);
        countNumber = null; countDrain = null;
        if (spec.Countdown)
        {
            var count = new StackPanel { Margin = new Thickness(12, 1, 0, 0), HorizontalAlignment = HorizontalAlignment.Right };
            countNumber = new TextBlock { FontFamily = Theme.Numerals, FontSize = 22, FontWeight = FontWeights.SemiBold, Foreground = Theme.Brush("WarnText"), HorizontalAlignment = HorizontalAlignment.Right, Uid = "si-count" };
            count.Children.Add(countNumber);
            var cap = Ui.Text("left", 11.5, "Ink3"); cap.HorizontalAlignment = HorizontalAlignment.Right; cap.Margin = new Thickness(0, 4, 0, 0); count.Children.Add(cap);
            Grid.SetColumn(count, 2); grid.Children.Add(count);
            var track = new Grid { Height = 5, Margin = new Thickness(0, 9, 0, 0), ClipToBounds = true };
            track.Children.Add(new Border { Background = Theme.Brush("Track"), CornerRadius = new CornerRadius(3) });
            countDrain = new ScaleTransform(1, 1);
            track.Children.Add(new Border { Background = Theme.Gradient("warn"), CornerRadius = new CornerRadius(3), RenderTransform = countDrain });
            Grid.SetRow(track, 1); Grid.SetColumn(track, 1); Grid.SetColumnSpan(track, 2); grid.Children.Add(track);
        }
        var wrap = new StackPanel();
        wrap.Children.Add(new Border { Padding = new Thickness(0, 0, 0, 14), Child = grid });
        wrap.Children.Add(new Border { Height = 1, Background = Theme.Brush("Rule2"), Margin = new Thickness(0, 0, 0, 14) });
        return wrap;
    }

    private void PaintCountdown()
    {
        var left = Model.LimitUntil - DateTimeOffset.UtcNow;
        if (left < TimeSpan.Zero) left = TimeSpan.Zero;
        var seconds = (int)Math.Ceiling(left.TotalSeconds);
        if (countNumber is not null) countNumber.Text = $"{seconds / 60}:{seconds % 60:00}";
        if (countDrain is not null)
        {
            var share = Model.LimitTotal.TotalSeconds <= 0 ? 0 : Math.Clamp(left.TotalSeconds / Model.LimitTotal.TotalSeconds, 0, 1);
            Motion.To(countDrain, ScaleTransform.ScaleXProperty, share, countdown.IsEnabled ? 1000 : 0, Motion.Linear);
        }
        if (left <= TimeSpan.Zero && Model.State == SignInState.RateLimited) { countdown.Stop(); CountdownFinished?.Invoke(); }
    }

    internal string CountdownText => countNumber?.Text ?? "";

    /// <summary>One option row of the concept's .si-opt: icon, a bold head with a line under it, and an action.</summary>
    private static FrameworkElement Option(string icon, string head, IEnumerable<Inline> body, Button? action, bool first)
    {
        var grid = new Grid { Margin = new Thickness(0, first ? 0 : 9, 0, 9) };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(26) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var glyph = SignInIcons.Icon(icon, 16, Theme.Brush("Ink2")); glyph.VerticalAlignment = VerticalAlignment.Top; glyph.HorizontalAlignment = HorizontalAlignment.Left; glyph.Margin = new Thickness(0, 1, 0, 0);
        grid.Children.Add(glyph);
        var words = new StackPanel();
        words.Children.Add(Ui.Text(head, 12.5, "Ink", FontWeights.SemiBold, wrap: true));
        var sub = new TextBlock { FontSize = 12, Foreground = Theme.Brush("Ink3"), TextWrapping = TextWrapping.Wrap, LineHeight = 17.4, LineStackingStrategy = LineStackingStrategy.BlockLineHeight };
        sub.Inlines.AddRange(body);
        words.Children.Add(sub);
        Grid.SetColumn(words, 1); grid.Children.Add(words);
        if (action is not null) { action.Margin = new Thickness(10, 0, 0, 0); action.VerticalAlignment = VerticalAlignment.Center; Grid.SetColumn(action, 2); grid.Children.Add(action); }
        return grid;
    }

    /// <summary>Pairing turned off, on a tray without a device key: the password sign-in it always had, verified before
    /// it is saved, so a changed dashboard password can still be entered while pairing waits for the owner.</summary>
    private FrameworkElement PasswordOption()
    {
        var stack = new StackPanel { Margin = new Thickness(0, 14, 0, 0), Uid = "si-password-option" };
        stack.Children.Add(new Border { Height = 1, Background = Theme.Brush("Rule2"), Margin = new Thickness(0, 0, 0, 10) });
        var use = Ui.Button("Use password"); use.Height = 28; use.Uid = "si-use-password";
        use.Click += (_, _) => UsePasswordClicked?.Invoke();
        stack.Children.Add(Option("key", "Or use your password for now", new Inline[] { new Run("The tray keeps it with Windows data protection, and pairs by itself once Trust this local network is on.") }, use, true));
        return stack;
    }

    private FrameworkElement Guide()
    {
        var stack = new StackPanel { Margin = new Thickness(0, 0, 0, 8), Uid = "si-guide" };
        Button? use = null;
        IEnumerable<Inline> localBody;
        if (Model.LocalAddress is { Length: > 0 } local)
        {
            use = Ui.Button("Use this"); use.Height = 28; use.Uid = "si-use-local";
            use.Click += (_, _) => UseLocalClicked?.Invoke();
            localBody = new Inline[] { Bold(HostOf(local)), new Run(", as shown under " + Where) };
        }
        else localBody = new Inline[] { new Run("The dashboard shows it under " + Where + ".") };
        stack.Children.Add(Option("house", "Use the dashboard's local address", localBody, use, true));
        stack.Children.Add(new Border { Height = 1, Background = Theme.Brush("Rule2") });
        stack.Children.Add(Option("shield", "Or connect through your home VPN first", new Inline[] { new Run("Away from home, turn on the VPN, then use the local address. It puts this PC on your local network.") }, null, false));
        return stack;
    }

    // ---------------- steps: pending ring, then the progress ring, then a check that scales in

    private sealed class StepRow
    {
        public readonly Grid Root;
        public readonly FrameworkElement Pending, Check;
        public readonly Grid Ring;
        public readonly RotateTransform Turn;
        public readonly ScaleTransform CheckScale = new(0.6, 0.6);
        public readonly TextBlock Title;
        public bool Done, Active;

        public StepRow(StepSpec spec)
        {
            Root = new Grid { Margin = new Thickness(0, 0, 0, 11) };
            Root.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(28) });
            Root.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            var icon = new Grid { Width = 18, Height = 18, VerticalAlignment = VerticalAlignment.Top, HorizontalAlignment = HorizontalAlignment.Left, Margin = new Thickness(0, 1, 0, 0) };
            Pending = SignInIcons.Icon("pending", 18, Theme.Brush("Ink4"));
            Ring = SignInIcons.Ring(18, Theme.Brush("AccentText"), out Turn); Ring.Opacity = 0;
            Check = SignInIcons.Icon("check", 18, Theme.Brush("Calm"), Theme.Brush("SiOnCalm")); Check.Opacity = 0;
            Check.RenderTransformOrigin = new Point(0.5, 0.5); Check.RenderTransform = CheckScale;
            icon.Children.Add(Pending); icon.Children.Add(Ring); icon.Children.Add(Check);
            Root.Children.Add(icon);
            var words = new StackPanel();
            Title = Ui.Text(spec.Title, 13, "Ink3", FontWeights.Medium, wrap: true);
            words.Children.Add(Title);
            var sub = new TextBlock { FontSize = 12, Foreground = Theme.Brush("Ink3"), TextWrapping = TextWrapping.Wrap, LineHeight = 16.8, LineStackingStrategy = LineStackingStrategy.BlockLineHeight, Margin = new Thickness(0, 1, 0, 0) };
            sub.Inlines.AddRange(spec.Sub());
            words.Children.Add(sub);
            Grid.SetColumn(words, 1); Root.Children.Add(words);
        }

        public void Paint(bool done, bool active)
        {
            Done = done; Active = active;
            Motion.To(Pending, UIElement.OpacityProperty, done || active ? 0 : 1, 200, Motion.InOut);
            Motion.To(Ring, UIElement.OpacityProperty, active ? 1 : 0, 200, Motion.InOut);
            SignInIcons.Spin(Turn, active);
            Motion.To(Check, UIElement.OpacityProperty, done ? 1 : 0, 200, Motion.InOut);
            Motion.To(CheckScale, ScaleTransform.ScaleXProperty, done ? 1 : 0.6, Motion.Med, Motion.Spring);
            Motion.To(CheckScale, ScaleTransform.ScaleYProperty, done ? 1 : 0.6, Motion.Med, Motion.Spring);
            Title.Foreground = Theme.Brush(done || active ? "Ink" : "Ink3");
        }
    }

    private void BuildSteps(StepSpec[] steps)
    {
        // The words under each step name the dashboard, the user and this PC, so they are part of the key.
        var key = string.Join("|", steps.Select(step => step.Title)) + "|" + Model.Verified + "|" + Model.Username + "|" + Model.DeviceName;
        if (key == stepsKey) return;
        stepsKey = key;
        foreach (var row in stepRows) SignInIcons.Spin(row.Turn, false);
        stepRows.Clear(); stepsHost.Children.Clear();
        foreach (var step in steps) { var row = new StepRow(step); stepRows.Add(row); stepsHost.Children.Add(row.Root); }
        if (stepsHost.Children.Count > 0) ((FrameworkElement)stepsHost.Children[^1]).Margin = new Thickness(0);
    }

    private void PaintSteps(int step)
    {
        var busy = Model.State is SignInState.Pairing or SignInState.Securing;
        for (int i = 0; i < stepRows.Count; i++) stepRows[i].Paint(i < step, busy && i == step);
    }

    // ---------------- message slot (its height is kept, so nothing below moves)

    private void ShowMessage(MsgSpec? spec, bool instant)
    {
        var row = (FrameworkElement)msg.Children[0];
        if (spec is null)
        {
            msgShown = null;
            if (instant || !Motion.Enabled) { row.BeginAnimation(OpacityProperty, null); row.Opacity = 0; return; }
            Motion.To(row, OpacityProperty, 0, Motion.Med, Motion.InOut); Motion.To(msgShift, TranslateTransform.YProperty, -3, Motion.Med);
            return;
        }
        var text = (spec.Bold ?? "") + (spec.Sub is null ? "" : (spec.Bold is null ? "" : " ") + spec.Sub);
        void Set()
        {
            msgIcon.Content = SignInIcons.Icon(spec.Icon, 15, Theme.Brush(spec.Info ? "AccentText" : "CritText"));
            msgText.Inlines.Clear();
            msgText.Foreground = Theme.Brush(spec.Info ? "Ink2" : "CritText");
            if (spec.Bold is not null) msgText.Inlines.Add(new Run(spec.Bold) { FontWeight = FontWeights.SemiBold });
            if (spec.Sub is not null)
            {
                if (spec.Bold is not null) msgText.Inlines.Add(new LineBreak());
                msgText.Inlines.Add(new Run(spec.Sub) { Foreground = spec.Bold is null ? msgText.Foreground : Theme.Brush("Ink3") });
            }
            msgShown = text;
        }
        if (instant || !Motion.Enabled) { Set(); row.BeginAnimation(OpacityProperty, null); row.Opacity = 1; msgShift.Y = 0; return; }
        if (row.Opacity > 0.5 && msgShown != text)
        {
            Motion.To(row, OpacityProperty, 0, 120, Motion.InOut, completed: (_, _) => { Set(); Motion.To(row, OpacityProperty, 1, Motion.Med, Motion.InOut); Motion.To(msgShift, TranslateTransform.YProperty, 0, Motion.Med, from: -3); });
            return;
        }
        Set();
        Motion.To(row, OpacityProperty, 1, Motion.Med, Motion.InOut); Motion.To(msgShift, TranslateTransform.YProperty, 0, Motion.Med, from: -3);
    }

    internal void HideMessage() { if (msgShown is not null) ShowMessage(null, false); }

    /// <summary>Caps Lock while typing a password: a calm note in the message slot (never over an error).</summary>
    private void PaintCaps()
    {
        var focused = Pass.Input.IsKeyboardFocusWithin || Confirm.Input.IsKeyboardFocusWithin;
        var on = focused && Keyboard.IsKeyToggled(Key.CapsLock);
        if (on && (msgShown is null || capsShown)) { if (!capsShown) ShowMessage(new MsgSpec("capsLock", null, "Caps Lock is on.", Info: true), false); capsShown = true; }
        else if (!on && capsShown) { capsShown = false; ShowMessage(null, false); }
    }

    // ---------------- password strength (setup): one meter, severity colours, eases to its width

    internal static (int Level, double Share, string Word, string Hint) Strength(string password)
    {
        if (password.Length == 0) return (0, 0, "", "At least 8 characters. Longer is stronger.");
        var length = new StringInfo(password).LengthInTextElements;
        var kinds = new[] { "[a-z]", "[A-Z]", "[0-9]", "[^A-Za-z0-9]" }.Count(pattern => System.Text.RegularExpressions.Regex.IsMatch(password, pattern));
        if (length < 8) return (1, (14 + length * 2) / 100.0, "Too short", $"{8 - length} more character{(8 - length == 1 ? "" : "s")} needed");
        if (Encoding.UTF8.GetByteCount(password) > 72) return (1, 1, "Too long", "Keep it within 72 bytes");
        if (System.Text.RegularExpressions.Regex.IsMatch(password, @"^(.)\1+$") || System.Text.RegularExpressions.Regex.IsMatch(password, "^(password|12345678|qwerty|letmein|admin)", System.Text.RegularExpressions.RegexOptions.IgnoreCase))
            return (2, 0.30, "Weak", "Easy to guess; avoid common words");
        var score = (length >= 20 ? 3 : length >= 14 ? 2 : length >= 10 ? 1 : 0) + (kinds >= 3 ? 1 : 0) + (kinds >= 2 ? 0.5 : 0);
        if (score >= 3) return (5, 1, "Strong", $"{length} characters, {kinds} kinds");
        if (score >= 2) return (4, 0.80, "Good", $"{length} characters; 14 or more is stronger");
        if (score >= 1) return (3, 0.58, "Fair", "Add length: a few words with dashes works well");
        return (2, 0.36, "Weak", "Add length or mix letters, digits and symbols");
    }

    private void PaintStrength()
    {
        var (level, share, word, hint) = Strength(Pass.Value);
        Motion.To(strengthFill, WidthProperty, ContentWidth * share, Motion.Slow, Motion.Out);
        strengthFill.Background = Theme.Gradient(level >= 4 ? "calm" : level == 3 ? "warn" : "crit");
        strengthWord.Text = word;
        strengthWord.Foreground = Theme.Brush(level >= 4 ? "GoodText" : level == 3 ? "WarnText" : level >= 1 ? "CritText" : "Ink3");
        strengthHint.Text = hint;
        var matches = Confirm.Value.Length > 0 && Confirm.Value == Pass.Value;
        Confirm.Matched = matches;
        Confirm.Aside.Inlines.Clear();
        if (matches)
        {
            Confirm.Aside.Inlines.Add(new InlineUIContainer(SignInIcons.Icon("check", 13, Theme.Brush("GoodText"), Theme.Brush("SiOnCalm"))) { BaselineAlignment = BaselineAlignment.Center });
            Confirm.Aside.Inlines.Add(new Run(" Matches") { Foreground = Theme.Brush("GoodText"), FontWeight = FontWeights.SemiBold });
        }
    }

    // ---------------- motion: shake, entrance, hand-off

    /// <summary>The error shake: a decaying 7 px oscillation, x = 7 (1 - p)^2 sin(8 pi p), over 440 ms.</summary>
    public void Shake()
    {
        if (!Motion.Enabled) return;
        var frames = new DoubleAnimationUsingKeyFrames { Duration = TimeSpan.FromMilliseconds(440) };
        for (int i = 0; i <= 26; i++)
        {
            var p = i / 26.0;
            frames.KeyFrames.Add(new LinearDoubleKeyFrame(7 * Math.Pow(1 - p, 2) * Math.Sin(p * 8 * Math.PI), KeyTime.FromTimeSpan(TimeSpan.FromMilliseconds(440 * p))));
        }
        shakeShift.BeginAnimation(TranslateTransform.XProperty, frames);
    }

    internal double ShakeOffset => shakeShift.X;

    /// <summary>The dashboard's entrance: the card's parts rise in with a stagger, then the contours grow from the
    /// summit and the scale bar fills.</summary>
    public void PlayEntrance(bool animate)
    {
        Opacity = 1; cardShift.Y = 0; cardScale.ScaleX = cardScale.ScaleY = 1; motif.ResetLeave();
        var items = new[] { head, bannerWrap, form, foot };
        for (int i = 0; i < items.Length; i++)
        {
            var item = items[i];
            if (item.RenderTransform is not TranslateTransform shift) item.RenderTransform = shift = new TranslateTransform();
            if (!animate || !Motion.Enabled) { item.BeginAnimation(OpacityProperty, null); item.Opacity = 1; shift.BeginAnimation(TranslateTransform.YProperty, null); shift.Y = 0; continue; }
            Motion.To(item, OpacityProperty, 1, Motion.Slow, Motion.InOut, delay: 60 + i * 64, from: 0);
            Motion.To(shift, TranslateTransform.YProperty, 0, Motion.Slow, Motion.Out, delay: 60 + i * 64, from: 12);
        }
        motif.Enter(animate);
    }

    /// <summary>The hand-off: the screen fades and lifts away in 340 ms (the panel header never moves).</summary>
    public void Leave(Action done)
    {
        motif.Leave();
        Motion.To(cardShift, TranslateTransform.YProperty, -8, 340, Motion.Out);
        Motion.To(cardScale, ScaleTransform.ScaleXProperty, 0.99, 340, Motion.Out);
        Motion.To(cardScale, ScaleTransform.ScaleYProperty, 0.99, 340, Motion.Out);
        Motion.To(this, OpacityProperty, 0, 340, Motion.InOut, completed: (_, _) => done());
    }

    internal void StopTimers()
    {
        countdown.Stop();
        motif.SetBusy(false);
        primary.SetMode("normal", true);
        foreach (var row in stepRows) SignInIcons.Spin(row.Turn, false);
    }
}
