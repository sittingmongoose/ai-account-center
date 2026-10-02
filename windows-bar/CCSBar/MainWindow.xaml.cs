using System;
using System.ComponentModel;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Text.RegularExpressions;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Documents;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Threading;

namespace CCSBar;

/// <summary>
/// The Daylight Atlas Windows panel. Rows are rebuilt from each sample, but meters and the selected-row platter are
/// kept by key across rebuilds, so a changed reading animates from what was on screen and an account switch glides.
/// </summary>
public partial class MainWindow : Window
{
    private sealed record Column(string Key, string Caption, Func<DashboardAccount, QuotaWindow?> Pick);

    private DashboardClient? client;
    private ConnectionSettings? connection;
    private AccountDashboard? dashboard;
    private readonly DispatcherTimer timer;
    private readonly Preferences preferences;
    private bool busy, staleSample, settingsVisible, signInVisible, confirmationVisible, popupOpen, openedOnce;
    private readonly HashSet<string> expanded = new(StringComparer.Ordinal);
    private readonly Dictionary<string, Meter> meters = new(StringComparer.Ordinal);
    private readonly Dictionary<string, double?> shownAtHide = new(StringComparer.Ordinal);
    private readonly Dictionary<string, Border> platters = new(StringComparer.Ordinal);
    private readonly Dictionary<string, string?> platterFor = new(StringComparer.Ordinal);
    private readonly Dictionary<string, FrameworkElement> activeRows = new(StringComparer.Ordinal);
    private readonly HashSet<string> drawCheck = new(StringComparer.Ordinal);
    private DateTimeOffset lastFailure = DateTimeOffset.MinValue;
    private DateTime lastHidden = DateTime.MinValue;
    private string? statusFlash;
    private Popup? openPopup;
    private readonly RotateTransform refreshTurn = new(), gearTurn = new();

    /// <summary>Raised after every new sample or connection change (the tray tooltip follows it).</summary>
    public event Action? SampleChanged;
    /// <summary>Wired by App: the global hotkey's state and toggle, and Quit.</summary>
    /// <summary>"registered", "in-use" (another app owns it), "unavailable" or "off".</summary>
    internal Func<string>? HotkeyState;
    internal Func<bool, string>? SetHotkey;
    internal Action? QuitRequested;
    internal bool AllowClose { get; set; }
    public bool IsConfigured => client is not null;
    public bool IsStale => staleSample;
    public AccountDashboard? Dashboard => dashboard;

    public MainWindow(Preferences? preferences = null, bool loadConnection = true)
    {
        this.preferences = preferences ?? new Preferences();
        InitializeComponent();
        FontFamily = Theme.Sans;
        TextOptions.SetTextFormattingMode(this, TextFormattingMode.Ideal);
        Width = Math.Min(760, SystemParameters.WorkArea.Width - 24);
        Height = Math.Min(850, SystemParameters.WorkArea.Height - 24);
        // Large surfaces take the shared brushes so a theme change cross-fades them with the cards.
        Root.Background = Theme.Brush("Panel"); Root.BorderBrush = Theme.Brush("Rule");
        Footer.Background = Theme.Brush("Panel"); Footer.BorderBrush = Theme.Brush("Rule");
        SettingsLayer.Background = Theme.Brush("Panel"); SignInLayer.Background = Theme.Brush("Panel");
        HeaderName.Foreground = Theme.Brush("Ink"); StatusText.Foreground = Theme.Brush("Ink3");
        HeaderLogo.Content = Icons.Logo(22, Theme.Brush("Ink"));
        DashboardButton.Content = Ui.LabelWithIcon("Dashboard", Icons.Icon("external", 13, Theme.Brush("Ink3")), trailing: true);
        DashboardButton.ToolTip = Ui.Tip("Open the dashboard in your browser");
        RefreshButton.Content = Spinner("refresh", refreshTurn, 16);
        RefreshButton.ToolTip = Ui.Tip("Refresh usage");
        SettingsButton.Content = Spinner("settings", gearTurn, 16);
        SettingsButton.ToolTip = Ui.Tip("Settings");
        System.Windows.Automation.AutomationProperties.SetName(RefreshButton, "Refresh usage");
        System.Windows.Automation.AutomationProperties.SetName(SettingsButton, "Settings");
        PaintFade(); Theme.Changed += PaintFade;
        ContentScroll.ScrollChanged += (_, _) => UpdateFade();
        Deactivated += (_, _) => { if (!confirmationVisible && !popupOpen && !signInVisible) HidePopup(); };
        PreviewKeyDown += OnPreviewKeyDown;
        timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(60) };
        timer.Tick += async (_, _) => await Refresh(false);
        if (loadConnection)
        {
            timer.Start();
            try
            {
                connection = SecureStore.Load();
                if (connection is not null) client = new DashboardClient(connection);
            }
            catch { statusFlash = "The stored connection could not be unlocked. Sign in again."; }
        }
        RenderFooter();
        if (client is null && loadConnection) ShowSignIn(firstRun: true);
        UpdateStatus();
    }

    /// <summary>Fixture renders only: a loopback connection object that is never requested.</summary>
    internal void UseFixtureConnection()
    {
        connection = new ConnectionSettings { BaseURL = "http://127.0.0.1:3000", Username = "fixture", Password = "fixture-only" };
        client = new DashboardClient(connection);
        timer.Stop(); RenderFooter();
    }
    internal void ToggleDetailsForCheck(string id) => ToggleDetails(id);
    internal Border? PlatterFor(string provider) => platters.TryGetValue(provider, out var platter) ? platter : null;
    internal void SetRefreshingForCheck(bool spinning) => SetRefreshing(spinning);
    internal bool RefreshSpinning => refreshTurn.HasAnimatedProperties;

    private static FrameworkElement Spinner(string icon, RotateTransform turn, double size)
    {
        var glyph = Icons.Icon(icon, size, Theme.Brush("Ink2"));
        glyph.RenderTransformOrigin = new Point(0.5, 0.5); glyph.RenderTransform = turn;
        return glyph;
    }

    // ------------------------------------------------------------------ open, hide, position

    public async Task OpenPopup(bool settings = false)
    {
        ShowPanel();
        App.Trace("panel shown, visible=" + IsVisible);
        if (settings && !settingsVisible) OpenSettings();
        if (!signInVisible) await Refresh(true);
    }

    /// <summary>Positions, shows and activates the panel with the open animation (no refresh).</summary>
    internal void ShowPanel()
    {
        var wasVisible = IsVisible;
        PositionPopup();
        if (!wasVisible)
        {
            Show(); PlayOpen();
            if (busy) SetRefreshing(true);
            // A sample that arrived while the panel was hidden had no layout yet: place the selected-row platter now.
            Dispatcher.BeginInvoke(new Action(() => { PlacePlatters(); UpdateFade(); }), DispatcherPriority.Loaded);
        }
        Activate();
    }

    public async Task TogglePopup()
    {
        if (IsVisible && IsActive) { HidePopup(); return; }
        // A click on the tray icon first deactivates (and hides) the panel; do not reopen it on that same click.
        if (!IsVisible && (DateTime.UtcNow - lastHidden).TotalMilliseconds < 350) return;
        await OpenPopup();
    }

    public void HidePopup()
    {
        if (!IsVisible) return;
        App.Trace("panel hidden");
        openPopup?.SetCurrentValue(Popup.IsOpenProperty, false);
        if (settingsVisible) CloseSettings(animate: false);
        foreach (var (key, meter) in meters) shownAtHide[key] = meter.Target;
        Hide();
        if (busy) SetRefreshing(true); // stops the spin while hidden; it resumes if the panel opens before the sample lands
        lastHidden = DateTime.UtcNow;
    }

    private void PositionPopup()
    {
        // Keep the entire panel inside the taskbar's monitor, including scaled desktops.
        var monitor = System.Windows.Forms.Screen.FromPoint(System.Windows.Forms.Control.MousePosition);
        var dpi = VisualTreeHelper.GetDpi(this);
        var area = monitor.WorkingArea;
        Width = Math.Min(760, area.Width / dpi.DpiScaleX - 24);
        Height = Math.Min(850, area.Height / dpi.DpiScaleY - 24);
        Left = area.Right / dpi.DpiScaleX - Width - 12;
        Top = area.Bottom / dpi.DpiScaleY - Height - 12;
    }

    /// <summary>Panel open: the window fades and rises 12 px, blocks follow staggered; bars sweep from 0 on the
    /// first open of a session, and on later opens only readings that changed since the last open move.</summary>
    private void PlayOpen()
    {
        var first = !openedOnce; openedOnce = true;
        Root.Opacity = 0;
        Motion.To(Root, OpacityProperty, 1, 200, from: 0);
        Motion.To(RootShift, TranslateTransform.YProperty, 0, 320, from: 12);
        var items = new List<FrameworkElement> { HeaderBar };
        items.AddRange(ContentPanel.Children.OfType<FrameworkElement>());
        items.Add(Footer);
        for (int i = 0; i < items.Count; i++)
        {
            var delay = first ? i * 26 : Math.Min(150, i * 7);
            var item = items[i];
            if (item.RenderTransform is not TranslateTransform shift) item.RenderTransform = shift = new TranslateTransform();
            item.Opacity = 0;
            Motion.To(item, OpacityProperty, 1, 240, delay: delay, from: 0);
            Motion.To(shift, TranslateTransform.YProperty, 0, 380, delay: delay, from: 8);
        }
        foreach (var (key, meter) in meters)
        {
            if (meter.Target is not double target) continue;
            double from = first ? 0 : shownAtHide.TryGetValue(key, out var previous) && previous is double old ? old : target;
            if (Math.Abs(from - target) < 0.0001) continue;
            meter.BeginAnimation(Meter.ShownProperty, null);
            meter.Shown = from;
            Motion.To(meter, Meter.ShownProperty, target, Motion.Draw, Motion.Out, delay: first ? 120 : 60, from: from);
        }
    }

    private void OnPreviewKeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key != Key.Escape) return;
        e.Handled = true;
        if (openPopup is { IsOpen: true }) { openPopup.IsOpen = false; return; }
        if (settingsVisible) { CloseSettings(); return; }
        if (signInVisible && client is not null) { CloseSignIn(); return; }
        HidePopup();
    }

    // ------------------------------------------------------------------ data

    public async Task Refresh(bool force)
    {
        if (busy || client is null) return;
        if (!force && DateTimeOffset.UtcNow - lastFailure < TimeSpan.FromMinutes(1)) return;
        busy = true; SetRefreshing(true); DisableMutations();
        statusFlash = dashboard is null ? "Loading accounts" : "Refreshing usage";
        UpdateStatus();
        try { ApplyDashboardSample(await client.Dashboard(force)); }
        catch (Exception error)
        {
            lastFailure = DateTimeOffset.UtcNow;
            statusFlash = DisplayError(error);
            if (dashboard is null) RenderEmpty("Usage is unavailable", "Check the dashboard connection in Settings, or try Refresh.");
            else MarkStale();
        }
        finally { FinishRequest(); }
    }

    /// <summary>Shows a sample. Used by Refresh and by the offline fixture renderer.</summary>
    public void ApplyDashboardSample(AccountDashboard sample)
    {
        dashboard = sample;
        timer.Interval = TimeSpan.FromSeconds(sample.Settings?.ValidatedInterval ?? 60);
        staleSample = false; lastFailure = DateTimeOffset.MinValue; statusFlash = null;
        RenderDashboard();
        UpdateStatus();
        SampleChanged?.Invoke();
    }

    private void FinishRequest()
    {
        busy = false; SetRefreshing(false);
        if (dashboard is not null) RenderDashboard();
        UpdateStatus();
        SampleChanged?.Invoke();
    }

    private void MarkStale()
    {
        staleSample = true;
        RenderDashboard();
        SampleChanged?.Invoke();
    }

    private void SetRefreshing(bool spinning)
    {
        RefreshButton.IsEnabled = !spinning && client is not null;
        // Only a visible panel spins: a background refresh while hidden runs no animation clock (no idle CPU).
        if (spinning && Motion.Enabled && IsVisible)
            refreshTurn.BeginAnimation(RotateTransform.AngleProperty, new DoubleAnimation(0, 360, TimeSpan.FromMilliseconds(900)) { RepeatBehavior = RepeatBehavior.Forever });
        else
        {
            // Ease out of the spin to the next full turn instead of snapping back.
            var angle = refreshTurn.Angle % 360;
            refreshTurn.BeginAnimation(RotateTransform.AngleProperty, null);
            refreshTurn.Angle = angle;
            if (angle > 0.5 && Motion.Enabled && IsVisible) Motion.To(refreshTurn, RotateTransform.AngleProperty, 360, 700, Motion.Out, completed: (_, _) => { refreshTurn.BeginAnimation(RotateTransform.AngleProperty, null); refreshTurn.Angle = 0; });
            else refreshTurn.Angle = 0;
        }
    }

    private void UpdateStatus()
    {
        StatusText.Inlines.Clear();
        if (statusFlash is not null) { StatusText.Inlines.Add(new Run(statusFlash)); StatusText.ToolTip = null; return; }
        if (client is null) { StatusText.Inlines.Add(new Run("Not connected")); return; }
        if (dashboard is null) { StatusText.Inlines.Add(new Run("Connecting")); return; }
        var hidden = dashboard.Hidden;
        var providers = dashboard.Accounts.GroupBy(account => account.Provider).Where(group => !hidden.Contains(group.Key)).ToArray();
        var reporting = providers.Count(group => group.Any(account => account.Status is "ok" or "cached" && account.HasUsableUsage || account.Status == "ok"));
        var cached = providers.All(group => group.All(account => account.Status != "ok"));
        StatusText.Inlines.Add(new Run($"{reporting} of {providers.Length}") { Foreground = Theme.Brush("Ink2"), FontWeight = FontWeights.Medium });
        StatusText.Inlines.Add(new Run(" reporting · " + (cached ? "cached" : "live") + " · updated " + (DateTimeOffset.TryParse(dashboard.UpdatedAt, out var at) ? Formatting.Clock(at) : "time unavailable")));
        if (staleSample) StatusText.Inlines.Add(new Run(" · refresh failed") { Foreground = Theme.Brush("WarnText"), FontWeight = FontWeights.SemiBold });
        var hiddenNames = hidden.Select(provider => Formatting.ProviderName(provider)).ToArray();
        StatusText.ToolTip = Ui.Tip($"Updated {Formatting.Relative(dashboard.UpdatedAt)}. Every reading is the dashboard's sample." + (hiddenNames.Length > 0 ? " Hidden on the dashboard: " + string.Join(", ", hiddenNames) + "." : ""));
    }

    // ------------------------------------------------------------------ rendering

    private void RenderDashboard()
    {
        if (dashboard is null) return;
        var offset = ContentScroll.VerticalOffset;
        foreach (var meter in meters.Values) Ui.Detach(meter);
        foreach (var platter in platters.Values) Ui.Detach(platter);
        activeRows.Clear();
        ContentPanel.Children.Clear();
        if (staleSample) ContentPanel.Children.Add(StaleBanner());
        var hidden = dashboard.Hidden;
        foreach (var provider in Formatting.ProviderOrder(dashboard.Accounts))
        {
            if (hidden.Contains(provider)) continue;
            var accounts = dashboard.Accounts.Where(account => account.Provider == provider).ToArray();
            ContentPanel.Children.Add(provider is "claude" or "codex" or "antigravity" ? AccountSection(provider, accounts) : ProviderCard(provider, accounts));
        }
        if (dashboard.Accounts.Count == 0) RenderEmpty("No accounts found", "Your signed-in accounts appear here when AI Account Center discovers them.");
        RenderFooter();
        if (busy || staleSample) DisableMutations();
        Dispatcher.BeginInvoke(new Action(() => { ContentScroll.ScrollToVerticalOffset(offset); PlacePlatters(); UpdateFade(); }), DispatcherPriority.Loaded);
    }

    private void RenderEmpty(string title, string message)
    {
        ContentPanel.Children.Clear();
        var panel = new StackPanel { Margin = new Thickness(14, 12, 14, 12) };
        panel.Children.Add(Ui.Text(title, 13, "Ink", FontWeights.SemiBold));
        var text = Ui.Text(message, 12, "Ink3", wrap: true); text.Margin = new Thickness(0, 3, 0, 0); panel.Children.Add(text);
        ContentPanel.Children.Add(Card(panel));
    }

    private Border StaleBanner()
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal };
        row.Children.Add(Icons.Icon("alertCircle", 15, Theme.Brush("WarnText")));
        var text = Ui.Text("Showing the last confirmed sample. Refresh to verify current usage, accounts and settings.", 12, "WarnText", wrap: true);
        text.Margin = new Thickness(8, 0, 0, 0); row.Children.Add(text);
        return new Border { Background = Theme.Brush("WarnSoft"), BorderBrush = Theme.Brush("WarnLine"), BorderThickness = new Thickness(1), CornerRadius = new CornerRadius(8), Padding = new Thickness(10, 8, 10, 8), Margin = new Thickness(0, 0, 0, 6), Child = row, Uid = "stale" };
    }

    private static Border Card(UIElement child) => new()
    {
        Child = child, Background = Theme.Brush("Card"), BorderBrush = Theme.Brush("Rule"), BorderThickness = new Thickness(1),
        CornerRadius = new CornerRadius(8), Margin = new Thickness(0, 0, 0, 6), SnapsToDevicePixels = true,
    };

    private Meter MeterFor(string key, MeterKind kind)
    {
        if (!meters.TryGetValue(key, out var meter) || meter.Kind != kind) { meter = new Meter(key, kind); meters[key] = meter; }
        else Ui.Detach(meter);
        return meter;
    }

    private Meter ShowMeter(string key, MeterKind kind, MeterSpec spec)
    {
        var meter = MeterFor(key, kind);
        meter.Update(spec, animate: IsVisible && meter.HasShownTarget);
        return meter;
    }

    // ---------------- account sections: Claude, Codex, Antigravity

    private static QuotaWindow[] Meters(DashboardAccount account) => Formatting.VisibleWindows(account)
        .Where(window => window.Kind is not ("balance" or "extra_usage" or "spend"))
        .Where(window => account.Provider != "qwen" || !Formatting.IsQwenDuplicateMetadata(window) && !window.Key.StartsWith("addon-", StringComparison.Ordinal))
        .ToArray();

    private static QuotaWindow? ClaudeWindow(DashboardAccount account, string key, double minutes)
    {
        var candidates = Meters(account).Where(window => !Formatting.IsFable(window)).ToArray();
        return candidates.FirstOrDefault(window => window.Key == key) ?? candidates.FirstOrDefault(window => window.WindowMinutes == minutes);
    }

    private List<Column> Columns(string provider, DashboardAccount[] accounts)
    {
        if (provider == "claude")
        {
            var columns = new List<Column> { new("5h", "5-hour", a => ClaudeWindow(a, "five_hour", 300)), new("week", "Weekly", a => ClaudeWindow(a, "seven_day", 10080)) };
            if (accounts.Any(account => Formatting.IsMaxPlan(account.Plan)))
                columns.Add(new("fable", "Fable", a => Formatting.VisibleWindows(a).FirstOrDefault(Formatting.IsFable)));
            return columns;
        }
        if (provider == "codex")
            return new() { new("5h", "5-hour", a => Formatting.CodexPrimaryWindows(a).FirstOrDefault(w => w.Key == "five_hour")), new("week", "Weekly", a => Formatting.CodexPrimaryWindows(a).FirstOrDefault(w => w.Key == "seven_day")) };
        // Antigravity: Gemini 5-hour, Gemini weekly and Claude and GPT weekly when reported; otherwise the first windows.
        var known = new[] { ("gemini-5h", "Gemini 5-hour"), ("gemini-weekly", "Gemini weekly"), ("3p-weekly", "Claude and GPT weekly") };
        var keys = accounts.SelectMany(Meters).Select(window => window.Key).Distinct(StringComparer.Ordinal).ToList();
        var result = known.Where(item => keys.Contains(item.Item1)).Select(item => new Column(item.Item1, item.Item2, a => Meters(a).FirstOrDefault(w => w.Key == item.Item1))).ToList();
        foreach (var key in keys)
        {
            if (result.Count >= 3) break;
            if (result.Any(column => column.Key == key)) continue;
            var sample = accounts.SelectMany(Meters).First(window => window.Key == key);
            result.Add(new Column(key, ShortLabel(provider, sample), a => Meters(a).FirstOrDefault(w => w.Key == key)));
        }
        return result;
    }

    private bool AntigravitySwitchable(DashboardAccount[] accounts) => accounts.Count(account => account.Status is "ok" or "cached") >= 2;

    private UIElement AccountSection(string provider, DashboardAccount[] accounts)
    {
        var columns = Columns(provider, accounts);
        var switchable = provider is "codex" or "antigravity";
        double acts = provider == "claude" ? 62 : 108;
        var stack = new StackPanel();
        var multiAg = provider == "antigravity" && accounts.Length > 1;
        stack.Children.Add(SectionHeader(provider, accounts, columns, acts, separateCaptions: multiAg));
        if (multiAg) stack.Children.Add(CaptionRow(columns, acts));
        var rowsGrid = new Grid { Margin = new Thickness(0, 0, 0, 0) };
        var rowsStack = new StackPanel();
        if (switchable)
        {
            if (!platters.TryGetValue(provider, out var platter))
            {
                platter = new Border { VerticalAlignment = VerticalAlignment.Top, Margin = new Thickness(4, 0, 4, 0), CornerRadius = new CornerRadius(6), BorderThickness = new Thickness(1), IsHitTestVisible = false, Opacity = 0, RenderTransform = new TranslateTransform() };
                platter.Background = Theme.Brush("Platter"); platter.BorderBrush = Theme.Brush("PlatterLine");
                platters[provider] = platter;
            }
            rowsGrid.Children.Add(platter);
        }
        rowsGrid.Children.Add(rowsStack);
        for (int i = 0; i < accounts.Length; i++)
        {
            var account = accounts[i];
            var separator = i > 0 && !(switchable && (account.IsActive || accounts[i - 1].IsActive));
            var row = AccountRow(provider, account, accounts, columns, acts, separator);
            rowsStack.Children.Add(row);
            if (switchable && account.IsActive) activeRows[provider] = row;
            if (expanded.Contains(account.Id)) rowsStack.Children.Add(DetailsHost(AccountDetails(provider, account, accounts), open: true));
        }
        stack.Children.Add(rowsGrid);
        var card = Card(stack); card.Uid = "section:" + provider; card.Padding = new Thickness(0, 0, 0, 2);
        return card;
    }

    private FrameworkElement SectionHeader(string provider, DashboardAccount[] accounts, List<Column> columns, double acts, bool separateCaptions)
    {
        var grid = Ui.SectionGrid(columns.Count, acts);
        grid.Margin = new Thickness(12, 6, 10, 0); grid.MinHeight = 28;
        var mark = Ui.Mark(provider, 16); grid.Children.Add(mark);
        var identity = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        var name = Ui.Text(Formatting.ProviderName(provider, accounts[0].ProviderLabel), 13.5, "Ink", FontWeights.SemiBold);
        name.Uid = "section-name";
        identity.Children.Add(name);
        var count = Ui.Text(accounts.Length.ToString(CultureInfo.CurrentCulture), 12, "Ink3"); count.Margin = new Thickness(7, 0, 0, 0); count.VerticalAlignment = VerticalAlignment.Bottom; count.Padding = new Thickness(0, 0, 0, 1);
        identity.Children.Add(count);
        var meta = SectionMeta(provider, accounts);
        if (meta is not null) { meta.Margin = new Thickness(9, 0, 0, 0); meta.VerticalAlignment = VerticalAlignment.Bottom; meta.Padding = new Thickness(0, 0, 0, 1); identity.Children.Add(meta); }
        if (separateCaptions)
        {
            // Two Antigravity accounts: name line with its own auto-switch tools, captions on the next line.
            var line = new DockPanel { Margin = new Thickness(12, 6, 10, 0), MinHeight = 28, LastChildFill = false };
            var head = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
            // The mark sits centred in the grid's 22 px mark column and the name starts on the identity column (22 + 14).
            var mark2 = Ui.Mark(provider, 16); mark2.Margin = new Thickness(3, 0, 17, 0); head.Children.Add(mark2); head.Children.Add(identity);
            line.Children.Add(head);
            var tools = AntigravityTools(accounts); DockPanel.SetDock(tools, Dock.Right); line.Children.Add(tools);
            return line;
        }
        Grid.SetColumn(identity, 2); grid.Children.Add(identity);
        for (int i = 0; i < columns.Count; i++)
        {
            var last = i == columns.Count - 1;
            var caption = Ui.Text(columns[i].Caption, 11, "Ink3", FontWeights.Medium, trim: !last);
            caption.VerticalAlignment = VerticalAlignment.Center;
            Grid.SetColumn(caption, Ui.MeterColumn(i)); if (last) Grid.SetColumnSpan(caption, 3);
            grid.Children.Add(caption);
        }
        return grid;
    }

    private static FrameworkElement CaptionRow(List<Column> columns, double acts)
    {
        var grid = Ui.SectionGrid(columns.Count, acts);
        grid.Margin = new Thickness(12, 6, 10, 0);
        for (int i = 0; i < columns.Count; i++)
        {
            var last = i == columns.Count - 1;
            var caption = Ui.Text(columns[i].Caption, 11, "Ink3", FontWeights.Medium, trim: !last);
            Grid.SetColumn(caption, Ui.MeterColumn(i)); if (last) Grid.SetColumnSpan(caption, 3);
            grid.Children.Add(caption);
        }
        return grid;
    }

    private static TextBlock? SectionMeta(string provider, DashboardAccount[] accounts)
    {
        if (provider == "claude" || provider == "antigravity" && accounts.Length < 2) return null;
        var active = accounts.FirstOrDefault(account => account.IsActive);
        var text = new TextBlock { FontSize = 12, Foreground = Theme.Brush("Ink3"), TextTrimming = TextTrimming.CharacterEllipsis };
        if (active is null) text.Inlines.Add(new Run(provider == "codex" ? "none active" : "active account not reported yet"));
        else
        {
            text.Inlines.Add(new Run(ShortName(active)) { Foreground = Theme.Brush("GoodText"), FontWeight = FontWeights.SemiBold });
            text.Inlines.Add(new Run(" active"));
        }
        return text;
    }

    private static string ShortName(DashboardAccount account) => (account.Email ?? account.Label).Split('@')[0];

    private FrameworkElement AccountRow(string provider, DashboardAccount account, DashboardAccount[] accounts, List<Column> columns, double acts, bool separator)
    {
        var switchable = provider is "codex" or "antigravity";
        var active = switchable && account.IsActive;
        var grid = Ui.SectionGrid(columns.Count, acts);
        grid.Margin = new Thickness(12, 6, 10, 6); grid.MinHeight = 33;
        var mark = Ui.Mark(provider, 20); grid.Children.Add(mark);
        var identity = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        var email = Ui.Text(account.Email ?? account.Label, 13, "Ink", active ? FontWeights.SemiBold : FontWeights.Medium, trim: true);
        identity.Children.Add(email);
        identity.Children.Add(RowMeta(account));
        identity.ToolTip = Ui.Tip(string.Join(" · ", new[] { account.Email ?? account.Label, Formatting.PlanLabel(account.Plan), Formatting.PlatformName(account.Platform), account.SampledAt is null ? "" : "sampled " + Formatting.Relative(account.SampledAt ?? account.FetchedAt) }.Where(part => !string.IsNullOrEmpty(part))));
        Grid.SetColumn(identity, 2); grid.Children.Add(identity);
        if (account.Status == "needs_sign_in" && !Meters(account).Any())
        {
            var quiet = Ui.Text(account.Message is { Length: > 0 } && account.Message.Length < 90 ? account.Message : "Sign-in required", 12, "Ink3", trim: true);
            quiet.VerticalAlignment = VerticalAlignment.Center;
            Grid.SetColumn(quiet, Ui.MeterColumn(0)); Grid.SetColumnSpan(quiet, columns.Count * 2 - 1); grid.Children.Add(quiet);
        }
        else for (int i = 0; i < columns.Count; i++)
        {
            var cell = Cell(provider, account, columns[i]);
            if (cell is null) continue;
            Grid.SetColumn(cell, Ui.MeterColumn(i)); grid.Children.Add(cell);
        }
        var slot = ActionSlot(provider, account, accounts);
        if (slot is not null) { Grid.SetColumn(slot, Ui.ActsColumn(columns.Count)); grid.Children.Add(slot); }
        return RowShell(grid, account.Id, separator, active, () => ToggleDetails(account.Id), "View every usage window, balance and reset for " + (account.Email ?? account.Label), columns.Count);
    }

    private static TextBlock RowMeta(DashboardAccount account)
    {
        var parts = new[] { Formatting.PlanLabel(account.Plan), Formatting.PlatformName(account.Platform), Formatting.Relative(account.SampledAt ?? account.FetchedAt) }.Where(part => !string.IsNullOrEmpty(part));
        var text = Ui.Text(string.Join(" · ", parts), 11.5, "Ink3", trim: true);
        if (account.Status == "needs_sign_in") { text.Text = ""; text.Inlines.Add(new Run("Sign-in needed") { Foreground = Theme.Brush("WarnText"), FontWeight = FontWeights.SemiBold }); text.Inlines.Add(new Run(" · " + Formatting.PlatformName(account.Platform))); }
        return text;
    }

    private FrameworkElement? Cell(string provider, DashboardAccount account, Column column)
    {
        var key = account.Id + "|" + column.Key;
        if (provider == "claude" && column.Key == "fable")
        {
            if (!Formatting.IsMaxPlan(account.Plan)) return null; // Pro: no Fable cell
            var fable = column.Pick(account);
            if (fable is null) return ShowMeter(key, MeterKind.Compact, new MeterSpec(null, "Fable usage is not reported yet. It appears here as its own weekly window once the dashboard sends one.", NaText: "Not reported yet"));
            return ShowMeter(key, MeterKind.Compact, CompactSpec(account, fable, null, 1));
        }
        var window = column.Pick(account);
        if (window is null) return null; // no reported window, no cell
        double? notch = null; double notchOpacity = 1;
        if (provider == "codex" && dashboard is not null)
        {
            notch = 100 - dashboard.CodexAutoSwitch.ThresholdPercent;
            notchOpacity = !dashboard.CodexAutoSwitch.Enabled ? 0.18 : account.IsActive ? 1 : 0.4;
        }
        else if (provider == "antigravity" && dashboard?.AntigravityAutoSwitch is { } ag && column.Key.StartsWith("gemini", StringComparison.Ordinal) && AntigravitySwitchable(dashboard.Accounts.Where(a => a.Provider == "antigravity").ToArray()))
        {
            notch = ag.ThresholdUsedPercent;
            notchOpacity = !ag.Enabled ? 0.18 : account.IsActive ? 1 : 0.4;
        }
        return ShowMeter(key, MeterKind.Compact, CompactSpec(account, window, notch, notchOpacity));
    }

    private static MeterSpec CompactSpec(DashboardAccount account, QuotaWindow window, double? notch, double notchOpacity)
    {
        var used = window.Unlimited || window.Enabled == false ? null : window.DisplayPercent;
        var na = window.Enabled == false ? "Disabled" : window.Unlimited ? "Unlimited" : "Unavailable";
        // A retained (cached) window keeps its sample time in the tooltip; the compact row stays one line.
        return new MeterSpec(used, MeterTip(account, window, notch), Reset: Formatting.ResetShort(window.ResetAt), ResetSoon: ResetSoon(window.ResetAt), Notch: notch, NotchOpacity: notchOpacity, NaText: na,
            ResetFallbacks: Formatting.ResetFallbacks(window.ResetAt));
    }

    private static bool ResetSoon(string? reset) => DateTimeOffset.TryParse(reset, out var at) && at - Formatting.Now() < TimeSpan.FromHours(2) && at > Formatting.Now();

    private static string MeterTip(DashboardAccount account, QuotaWindow window, double? notch)
    {
        var parts = new List<string> { Formatting.WindowLabel(account, window) };
        if (window.DisplayPercent is double used)
        {
            var text = Formatting.Percent(used) + " used";
            if (window.Used is double amount && window.Limit is double limit) text += $" ({Formatting.Amount(amount, null)} of {Formatting.Amount(limit, window.Unit)})";
            if (used > 100) text += " · +" + Formatting.Percent(used - 100) + " over";
            parts.Add(text);
        }
        else parts.Add(window.Enabled == false ? "Disabled" : window.Unlimited ? "Unlimited" : "Unavailable: no reading, not zero");
        parts.Add(window.ResetAt is null ? "No reset reported" : Formatting.ResetLong(window.ResetAt));
        if (notch is double at) parts.Add($"Notch: auto-switch at {Formatting.Percent(at)} used");
        if (window.Status == "cached") parts.Add("Cached · " + Formatting.WindowSample(window.SampledAt));
        else if (account.SampledAt is not null) parts.Add("Sampled " + Formatting.Relative(account.SampledAt));
        return string.Join(" · ", parts);
    }

    /// <summary>The fixed action slot. Claude: the Open pair, right-aligned. Codex and Antigravity: Activate, or the
    /// active label with no button chrome, its check in the button's 24 px leading space and "Active" on the label's x.</summary>
    private FrameworkElement? ActionSlot(string provider, DashboardAccount account, DashboardAccount[] accounts)
    {
        if (provider == "claude")
        {
            var id = account.Capabilities.ClaudeProfileId;
            if (!Formatting.IsSafeClaudeProfile(id)) return null;
            var pair = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right, VerticalAlignment = VerticalAlignment.Center };
            foreach (var platform in new[] { "mac", "windows" }.Where(account.Capabilities.ClaudePlatforms.Contains))
            {
                var name = platform == "mac" ? "Mac" : "Windows";
                var button = new Button { Style = (Style)FindResource("IconButton"), Content = Icons.PlatformGlyph(platform, 16, Theme.Brush("Ink2")), ToolTip = Ui.Tip("Open " + (account.Email ?? account.Label) + " in Claude on " + name), Margin = new Thickness(pair.Children.Count > 0 ? 6 : 0, 0, 0, 0), Uid = "mutation" };
                System.Windows.Automation.AutomationProperties.SetName(button, "Open on " + name);
                button.Click += async (_, _) => await OpenClaude(account, platform);
                pair.Children.Add(button);
            }
            return pair;
        }
        if (account.IsActive) return ActiveLabel(account, drawCheck.Remove(provider + ":" + account.Id));
        if (provider == "codex")
        {
            if (!Formatting.IsSafeProfile(account.Capabilities.CodexProfile)) return null;
            return ActivateButton(account, () => ActivateCodex(account), dashboard?.CodexAutoSwitch.ActivationInProgress == true);
        }
        // Antigravity
        var switchable = AntigravitySwitchable(accounts);
        if (switchable && account.Capabilities.AntigravityCanActivate && Formatting.IsSafeId(account.Capabilities.AntigravityProfileId))
            return ActivateButton(account, () => ActivateAntigravity(account), dashboard?.AntigravityAutoSwitch?.ActivationInProgress == true);
        if (accounts.Any(other => other.IsActive)) return null;
        return NotReportedLabel();
    }

    private Button ActivateButton(DashboardAccount account, Func<Task> activate, bool inProgress)
    {
        var button = new Button { Style = (Style)FindResource("AccentLineButton"), Content = new TextBlock { Text = "Activate" }, Padding = new Thickness(24, 0, 24, 0), HorizontalAlignment = HorizontalAlignment.Left, VerticalAlignment = VerticalAlignment.Center, HorizontalContentAlignment = HorizontalAlignment.Left, IsEnabled = !inProgress, Uid = "mutation:activate" };
        button.ToolTip = Ui.Tip("Make " + (account.Email ?? account.Label) + " the active " + Formatting.ProviderName(account.Provider) + " account");
        button.Click += async (_, _) => await activate();
        return button;
    }

    public static FrameworkElement ActiveLabel(DashboardAccount account, bool draw)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Left, VerticalAlignment = VerticalAlignment.Center, Uid = "active-label" };
        var check = Ui.CheckCircle(18, draw); check.Uid = "active-check"; check.VerticalAlignment = VerticalAlignment.Center;
        row.Children.Add(check);
        var words = new StackPanel { Margin = new Thickness(6, 0, 0, 0), VerticalAlignment = VerticalAlignment.Center };
        var title = Ui.Text("Active", 12.5, "Ink", FontWeights.SemiBold); title.Uid = "active-text"; title.LineHeight = 14; title.LineStackingStrategy = LineStackingStrategy.BlockLineHeight;
        words.Children.Add(title);
        var where = Formatting.PlatformName(account.Platform);
        if (where.Length > 0) { var on = Ui.Text("on " + where, 11, "Ink3"); on.LineHeight = 14; on.LineStackingStrategy = LineStackingStrategy.BlockLineHeight; words.Children.Add(on); }
        row.Children.Add(words);
        row.ToolTip = Ui.Tip("Active " + Formatting.ProviderName(account.Provider) + " account" + (where.Length > 0 ? " on " + where : ""));
        return row;
    }

    private static FrameworkElement NotReportedLabel()
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Left, VerticalAlignment = VerticalAlignment.Center };
        row.Children.Add(new System.Windows.Shapes.Ellipse { Width = 18, Height = 18, Stroke = Theme.Brush("Ink4"), StrokeThickness = 1.5, StrokeDashArray = new DoubleCollection { 2, 1.6 } });
        var words = new StackPanel { Margin = new Thickness(6, 0, 0, 0) };
        var title = Ui.Text("Not reported", 12.5, "Ink2", FontWeights.Medium); title.LineHeight = 14; title.LineStackingStrategy = LineStackingStrategy.BlockLineHeight; words.Children.Add(title);
        var sub = Ui.Text("as active", 11, "Ink3"); sub.LineHeight = 14; sub.LineStackingStrategy = LineStackingStrategy.BlockLineHeight; words.Children.Add(sub);
        row.Children.Add(words);
        row.ToolTip = Ui.Tip("The dashboard has not reported which Antigravity account is active");
        return row;
    }

    /// <summary>Full-row click target with a hover tint (140 ms), a press tint, the track lift and the chevron.
    /// Nested buttons keep their own action: they handle the mouse, so the row never sees their clicks.</summary>
    private FrameworkElement RowShell(FrameworkElement content, string id, bool separator, bool active, Action click, string tooltip, int meterColumns)
    {
        var root = new Grid { Background = Brushes.Transparent, Cursor = Cursors.Hand, Uid = "row:" + id, MinHeight = 45 };
        var hover = new Border { CornerRadius = new CornerRadius(8), Background = Theme.Brush(active ? "ActiveHover" : "RowHover"), Opacity = expanded.Contains(id) ? 1 : 0, Margin = active ? new Thickness(4, 0, 4, 0) : new Thickness(0) };
        var press = new Border { CornerRadius = new CornerRadius(8), Background = Theme.Brush("RowPress"), Opacity = 0, Margin = hover.Margin };
        root.Children.Add(hover); root.Children.Add(press);
        if (separator) root.Children.Add(new Border { Height = 1, VerticalAlignment = VerticalAlignment.Top, Margin = new Thickness(46, 0, 12, 0), Background = Theme.Brush("Rule2"), IsHitTestVisible = false });
        root.Children.Add(content);
        var chevron = Icons.Icon("chevRight", 14, Theme.Brush("Ink4"));
        var isOpen = expanded.Contains(id);
        chevron.Opacity = isOpen ? 1 : 0; chevron.RenderTransformOrigin = new Point(0.5, 0.5);
        var slide = new TranslateTransform(isOpen ? 0 : -3, 0); var turn = new RotateTransform(isOpen ? 90 : 0);
        chevron.RenderTransform = new TransformGroup { Children = { turn, slide } };
        chevron.HorizontalAlignment = HorizontalAlignment.Right; chevron.VerticalAlignment = VerticalAlignment.Center;
        if (content is Grid grid && meterColumns >= 0) { Grid.SetColumn(chevron, Ui.TailColumn(meterColumns)); grid.Children.Add(chevron); }
        else { chevron.HorizontalAlignment = HorizontalAlignment.Right; chevron.Margin = new Thickness(0, 0, 10, 0); root.Children.Add(chevron); }
        root.ToolTip = null;
        bool pressed = false;
        root.MouseEnter += (_, _) =>
        {
            Motion.To(hover, OpacityProperty, 1, Motion.Fast);
            if (!expanded.Contains(id)) { Motion.To(chevron, OpacityProperty, 1, 160); Motion.To(slide, TranslateTransform.XProperty, 0, 220); }
            foreach (var meter in FindMeters(content)) meter.SetHover(true);
        };
        root.MouseLeave += (_, _) =>
        {
            pressed = false; Motion.To(press, OpacityProperty, 0, Motion.Fast);
            if (!expanded.Contains(id)) { Motion.To(hover, OpacityProperty, 0, Motion.Fast); Motion.To(chevron, OpacityProperty, 0, 160); Motion.To(slide, TranslateTransform.XProperty, -3, 220); }
            foreach (var meter in FindMeters(content)) meter.SetHover(false);
        };
        root.MouseLeftButtonDown += (_, e) => { pressed = true; Motion.To(press, OpacityProperty, 1, 80); };
        root.MouseLeftButtonUp += (_, e) =>
        {
            Motion.To(press, OpacityProperty, 0, Motion.Fast);
            if (!pressed) return;
            pressed = false; e.Handled = true;
            Motion.To(turn, RotateTransform.AngleProperty, expanded.Contains(id) ? 0 : 90, 220);
            click();
            if (!expanded.Contains(id) && !root.IsMouseOver) Motion.To(hover, OpacityProperty, 0, Motion.Fast);
        };
        System.Windows.Automation.AutomationProperties.SetName(root, tooltip);
        return root;
    }

    private static IEnumerable<Meter> FindMeters(DependencyObject parent)
    {
        foreach (var child in LogicalTreeHelper.GetChildren(parent).OfType<DependencyObject>())
        {
            if (child is Meter meter) { yield return meter; continue; }
            foreach (var nested in FindMeters(child)) yield return nested;
        }
    }

    /// <summary>The selected-row platter: one per section, placed on the active row; on a switch it glides from the
    /// old row to the new one (translate and height, 440 ms ease-out, no overshoot).</summary>
    private void PlacePlatters()
    {
        foreach (var (provider, platter) in platters)
        {
            if (platter.Parent is not Grid rowsGrid) continue;
            var shift = (TranslateTransform)platter.RenderTransform;
            if (!activeRows.TryGetValue(provider, out var row) || !row.IsLoaded && row.ActualHeight == 0)
            {
                Motion.To(platter, OpacityProperty, 0, 200);
                platterFor[provider] = null;
                continue;
            }
            var y = row.TranslatePoint(new Point(0, 0), rowsGrid).Y;
            var height = row.ActualHeight;
            var id = row.Uid;
            platterFor.TryGetValue(provider, out var previous);
            var glide = previous is not null && previous != id && IsVisible && platter.Opacity > 0.5;
            if (glide)
            {
                Motion.To(shift, TranslateTransform.YProperty, y, 440, Motion.Out);
                Motion.To(platter, HeightProperty, height, 440, Motion.Out, from: double.IsNaN(platter.Height) ? height : platter.Height);
            }
            else
            {
                shift.BeginAnimation(TranslateTransform.YProperty, null); shift.Y = y;
                platter.BeginAnimation(HeightProperty, null); platter.Height = height;
            }
            if (platter.Opacity < 1) Motion.To(platter, OpacityProperty, 1, previous is null && IsVisible ? 260 : 0);
            platterFor[provider] = id;
        }
    }

    // ---------------- other providers: one row per provider (today's representative rule)

    private static int StatusRank(string status) => status switch { "ok" => 0, "cached" => 1, "needs_sign_in" => 2, "unavailable" => 3, "error" => 4, _ => 5 };

    private static DashboardAccount Representative(DashboardAccount[] accounts) => accounts
        .Select((account, index) => (account, index))
        .OrderBy(item => StatusRank(item.account.Status))
        .ThenByDescending(item => DateTimeOffset.TryParse(item.account.SampledAt ?? item.account.FetchedAt, out var at) ? at : DateTimeOffset.MinValue)
        .ThenBy(item => item.index).First().account;

    private static string StatusWord(DashboardAccount account) => account.Status switch
    {
        "ok" => "Live", "cached" => "Cached", "needs_sign_in" => "Sign-in needed", "error" => "Refresh failed", _ => "Unavailable"
    };

    private static string Period(QuotaWindow window)
    {
        var text = window.Key + " " + window.Label;
        if (window.WindowMinutes == 300 || Regex.IsMatch(text, @"five.?hour|5.?hour|\b5h\b|5 hours|rolling", RegexOptions.IgnoreCase)) return "5h";
        if (window.WindowMinutes == 10080 || Regex.IsMatch(text, @"seven.?day|weekly|\bweek\b|7.?day", RegexOptions.IgnoreCase)) return "week";
        if (Regex.IsMatch(text, "month", RegexOptions.IgnoreCase)) return "month";
        return "other";
    }

    public static string ShortLabel(string provider, QuotaWindow window)
    {
        var period = Period(window);
        if (Formatting.IsFable(window)) return "Fable";
        if (provider == "antigravity") return (window.Key.StartsWith("gemini", StringComparison.OrdinalIgnoreCase) ? "Gemini " : "Claude & GPT ") + (period == "5h" ? "5-hour" : "weekly");
        if (provider == "cursor") return window.Key switch { "plan-reported" => "Included", "autoPercentUsed" => "Cursor models", "apiPercentUsed" => "Other models", _ => window.Label };
        if (provider == "zai") return window.Key switch { "usage-1" => "5-hour tokens", "usage-2" => "Weekly tokens", "usage-3" => "Monthly requests", _ => window.Label };
        var per = period switch { "5h" => "5-hour", "week" => "Weekly", "month" => "Monthly", _ => null };
        if (provider == "opencode-go" && window.Key.StartsWith("console", StringComparison.OrdinalIgnoreCase)) return "Console " + (per ?? window.Label).ToLowerInvariant();
        return per ?? Formatting.WindowLabel(new DashboardAccount { Provider = provider }, window);
    }

    private static string? AmountText(QuotaWindow window)
    {
        if (window.Used is not double used || window.Limit is not double limit || limit <= 0 || !double.IsFinite(used)) return null;
        var limitText = limit >= 1e6 ? (limit / 1e6).ToString("0.##", CultureInfo.CurrentCulture) + "M" : Formatting.Amount(limit, null);
        return Formatting.Amount(used, null) + " of " + limitText + (string.IsNullOrWhiteSpace(window.Unit) ? "" : " " + window.Unit);
    }

    private UIElement ProviderCard(string provider, DashboardAccount[] accounts)
    {
        var representative = Representative(accounts);
        var windows = Meters(representative).Take(3).ToArray();
        var qwen = provider == "qwen";
        var grid = new Grid { Margin = new Thickness(12, 6, 10, 6), MinHeight = 40 };
        void Add(GridLength length) => grid.ColumnDefinitions.Add(new ColumnDefinition { Width = length });
        Add(new GridLength(24)); Add(new GridLength(12)); Add(new GridLength(170));
        int meterCount = qwen ? 1 : 3;
        for (int i = 0; i < meterCount; i++) { Add(new GridLength(12)); Add(new GridLength(1, GridUnitType.Star)); }
        if (qwen) { Add(new GridLength(12)); Add(GridLength.Auto); }
        Add(new GridLength(12)); Add(new GridLength(14));
        grid.Children.Add(Ui.Mark(provider, 24));
        var identity = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        identity.Children.Add(Ui.Text(Formatting.ProviderName(provider, representative.ProviderLabel), 13, "Ink", FontWeights.SemiBold, trim: true));
        var meta = accounts.Length > 1 ? $"{ShortName(representative)} · 1 of {accounts.Length}" : StatusWord(representative) + " " + Formatting.Relative(representative.SampledAt ?? representative.FetchedAt);
        var metaText = Ui.Text(meta.Trim(), 11.5, "Ink3", trim: true);
        if (accounts.Length > 1) metaText.ToolTip = Ui.Tip($"These meters are {representative.Email ?? representative.Label} ({StatusWord(representative).ToLowerInvariant()} {Formatting.Relative(representative.SampledAt)}). Details lists all {accounts.Length} accounts.");
        identity.Children.Add(metaText);
        Grid.SetColumn(identity, 2); grid.Children.Add(identity);
        if (windows.Length == 0)
        {
            var empty = Ui.Text(representative.Message is { Length: > 0 and < 120 } ? representative.Message : representative.Status == "needs_sign_in" ? "Sign-in required" : "Usage unavailable", 12, "Ink3", trim: true);
            empty.VerticalAlignment = VerticalAlignment.Center;
            Grid.SetColumn(empty, 4); Grid.SetColumnSpan(empty, meterCount * 2 - 1); grid.Children.Add(empty);
        }
        else for (int i = 0; i < Math.Min(meterCount, windows.Length); i++)
        {
            var window = windows[i];
            var used = window.Unlimited || window.Enabled == false ? null : window.DisplayPercent;
            var meter = ShowMeter(representative.Id + "|" + window.Key, MeterKind.Labeled, new MeterSpec(used, MeterTip(representative, window, null), Label: ShortLabel(provider, window),
                Reset: window.ResetAt is null ? null : Formatting.ResetShort(window.ResetAt), ResetSoon: ResetSoon(window.ResetAt),
                Amount: qwen || windows.Length == 1 ? AmountText(window) : null, NaText: window.Enabled == false ? "Disabled" : window.Unlimited ? "Unlimited" : "Unavailable"));
            Grid.SetColumn(meter, 4 + 2 * i); grid.Children.Add(meter);
        }
        if (qwen)
        {
            var packs = Formatting.QwenPacks(representative);
            if (packs.Length > 0) { var button = QwenPacksButton(packs); Grid.SetColumn(button, 6); grid.Children.Add(button); }
        }
        var row = RowShell(grid, "prov:" + provider, false, false, () => ToggleDetails("prov:" + provider), "View all " + Formatting.ProviderName(provider) + " usage windows, balances and reset times", -1);
        var stack = new StackPanel();
        stack.Children.Add(row);
        if (expanded.Contains("prov:" + provider)) stack.Children.Add(DetailsHost(ProviderDetails(provider, accounts), open: true));
        var card = Card(stack); card.Uid = "provider:" + provider;
        return card;
    }

    private Button QwenPacksButton(QuotaWindow[] packs)
    {
        var withCredit = packs.Where(pack => pack.Remaining is double left && double.IsFinite(left) && left > 0).ToArray();
        var next = withCredit.Select(pack => DateTimeOffset.TryParse(pack.ExpiresAt, out var at) ? at : (DateTimeOffset?)null).Where(at => at > Formatting.Now()).OrderBy(at => at).FirstOrDefault();
        var content = new Grid();
        content.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        content.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        content.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var pack = Icons.Icon("pack", 15, Theme.Brush("Ink3")); pack.Margin = new Thickness(0, 0, 8, 0); content.Children.Add(pack);
        var words = new StackPanel();
        words.Children.Add(Ui.Text($"{packs.Length} {(packs.Length == 1 ? "pack" : "packs")} · {withCredit.Length} with credit", 12, "Ink", FontWeights.SemiBold));
        words.Children.Add(Ui.Text(next is DateTimeOffset expires ? "next expires " + expires.ToLocalTime().ToString("MMM d", CultureInfo.CurrentCulture) : "none with credit", 11.5, "Ink3"));
        Grid.SetColumn(words, 1); content.Children.Add(words);
        var chevron = Icons.Icon("chevDown", 13, Theme.Brush("Ink3")); chevron.Margin = new Thickness(10, 0, 0, 0); chevron.RenderTransformOrigin = new Point(0.5, 0.5); chevron.RenderTransform = new RotateTransform();
        Grid.SetColumn(chevron, 2); content.Children.Add(chevron);
        var button = new Button { Style = (Style)FindResource("AtlasButton"), Content = content, Height = 38, Padding = new Thickness(10, 0, 9, 0), HorizontalAlignment = HorizontalAlignment.Right, VerticalAlignment = VerticalAlignment.Center, Uid = "qwen-packs" };
        button.ToolTip = Ui.Tip("Every Qwen credit pack, with what is left and when it expires");
        button.Click += (_, _) =>
        {
            var list = new StackPanel { MaxWidth = 380 };
            foreach (var item in packs)
            {
                var detail = new StackPanel { Margin = new Thickness(8, 6, 8, 6) };
                detail.Children.Add(Ui.Text(item.Label + " · " + Formatting.PackStatus(item), 12.5, "Ink", FontWeights.SemiBold, wrap: true));
                var amounts = new List<string>();
                if (item.Used is double used && double.IsFinite(used) && used >= 0) amounts.Add("Used " + Formatting.Amount(used, item.Unit));
                if (item.Limit is double limit && double.IsFinite(limit) && limit >= 0) amounts.Add("Limit " + Formatting.Amount(limit, item.Unit));
                if (item.Remaining is double remaining && double.IsFinite(remaining) && remaining >= 0) amounts.Add("Remaining " + Formatting.Amount(remaining, item.Unit));
                detail.Children.Add(Ui.Text(amounts.Count > 0 ? string.Join(" · ", amounts) : "Pack amount unavailable", 12, "Ink2", wrap: true));
                detail.Children.Add(Ui.Text(Formatting.Expiration(item.ExpiresAt), 11.5, "Ink3", wrap: true));
                if (item.Status == "cached") detail.Children.Add(Ui.Text("Cached · " + Formatting.WindowSample(item.SampledAt), 11.5, "Ink3", wrap: true));
                list.Children.Add(detail);
            }
            var scroll = new ScrollViewer { Content = list, MaxHeight = Math.Min(460, SystemParameters.WorkArea.Height - 80), VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
            scroll.Resources.Add(typeof(ScrollBar), FindResource("ThinScrollBar"));
            ShowPopup(Menus.Create(button, scroll, PlacementMode.Bottom, 300), chevron);
        };
        return button;
    }

    private void ShowPopup(Popup popup, FrameworkElement? chevron = null)
    {
        openPopup?.SetCurrentValue(Popup.IsOpenProperty, false);
        openPopup = popup; popupOpen = true;
        if (chevron?.RenderTransform is RotateTransform turn) Motion.To(turn, RotateTransform.AngleProperty, 180, 220);
        popup.Closed += (_, _) =>
        {
            popupOpen = false;
            if (chevron?.RenderTransform is RotateTransform back) Motion.To(back, RotateTransform.AngleProperty, 0, 220);
            if (ReferenceEquals(openPopup, popup)) openPopup = null;
            Dispatcher.BeginInvoke(new Action(() => { if (!IsActive && IsVisible && !confirmationVisible && !signInVisible) HidePopup(); }), DispatcherPriority.Background);
        };
        popup.IsOpen = true;
    }

    // ---------------- details (inline expansion, animated height)

    private void ToggleDetails(string id)
    {
        if (!expanded.Add(id)) expanded.Remove(id);
        // Find the row and its details host in the current tree and animate in place.
        var row = FindByUid(ContentPanel, "row:" + id);
        if (row?.Parent is not StackPanel stack) { RenderDashboard(); return; }
        var index = stack.Children.IndexOf(row);
        var next = index + 1 < stack.Children.Count ? stack.Children[index + 1] as FrameworkElement : null;
        if (next?.Uid == "details")
        {
            if (expanded.Contains(id)) return;
            CollapseDetails(next);
            return;
        }
        if (!expanded.Contains(id) || dashboard is null) return;
        FrameworkElement body;
        if (id.StartsWith("prov:", StringComparison.Ordinal))
        {
            var provider = id[5..];
            body = ProviderDetails(provider, dashboard.Accounts.Where(account => account.Provider == provider).ToArray());
        }
        else
        {
            var account = dashboard.Accounts.FirstOrDefault(candidate => candidate.Id == id);
            if (account is null) return;
            body = AccountDetails(account.Provider, account, dashboard.Accounts.Where(candidate => candidate.Provider == account.Provider).ToArray());
        }
        stack.Children.Insert(index + 1, DetailsHost(body, open: false));
        if (busy || staleSample) DisableMutations();
    }

    private static FrameworkElement? FindByUid(DependencyObject parent, string uid)
    {
        foreach (var child in LogicalTreeHelper.GetChildren(parent).OfType<FrameworkElement>())
        {
            if (child.Uid == uid) return child;
            var nested = FindByUid(child, uid);
            if (nested is not null) return nested;
        }
        return null;
    }

    private FrameworkElement DetailsHost(FrameworkElement body, bool open)
    {
        var inner = new Border { Padding = new Thickness(46, 10, 14, 14), Child = body, RenderTransform = new TranslateTransform() };
        var host = new Border { Child = inner, ClipToBounds = true, Uid = "details" };
        if (open || !Motion.Enabled) return host;
        host.Height = 0; inner.Opacity = 0;
        host.Loaded += (_, _) =>
        {
            inner.Measure(new Size(Math.Max(100, host.ActualWidth), double.PositiveInfinity));
            var height = inner.DesiredSize.Height;
            Motion.To(host, HeightProperty, height, 320, Motion.InOut, from: 0, completed: (_, _) => { host.BeginAnimation(HeightProperty, null); host.Height = double.NaN; UpdateFade(); });
            Motion.To(inner, OpacityProperty, 1, 220, delay: 60, from: 0);
            Motion.To((TranslateTransform)inner.RenderTransform, TranslateTransform.YProperty, 0, 300, delay: 60, from: -4);
        };
        host.SizeChanged += (_, _) => PlacePlatters();
        return host;
    }

    private void CollapseDetails(FrameworkElement host)
    {
        if (!Motion.Enabled) { Ui.Detach(host); PlacePlatters(); UpdateFade(); return; }
        Motion.To(host, HeightProperty, 0, 300, Motion.InOut, from: host.ActualHeight, completed: (_, _) => { Ui.Detach(host); PlacePlatters(); UpdateFade(); });
        if (host is Border { Child: UIElement inner }) Motion.To(inner, OpacityProperty, 0, 180);
    }

    private FrameworkElement AccountDetails(string provider, DashboardAccount account, DashboardAccount[] siblings)
    {
        var panel = new StackPanel();
        panel.Children.Add(DetailWindows(account));
        AddDetailFooter(panel, account);
        var actions = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 10, 0, 0) };
        if (provider == "claude" && Formatting.IsSafeClaudeProfile(account.Capabilities.ClaudeProfileId))
            foreach (var platform in new[] { "mac", "windows" }.Where(account.Capabilities.ClaudePlatforms.Contains))
            {
                var name = platform == "mac" ? "Mac" : "Windows";
                var button = Ui.Button("Open on " + name, icon: Icons.PlatformGlyph(platform, 14, Theme.Brush("Ink2")));
                button.Margin = new Thickness(0, 0, 8, 0); button.Uid = "mutation";
                button.Click += async (_, _) => await OpenClaude(account, platform);
                actions.Children.Add(button);
            }
        else if (provider is "codex" or "antigravity")
        {
            var slot = ActionSlot(provider, account, siblings);
            if (slot is not null) actions.Children.Add(slot);
        }
        if (actions.Children.Count > 0) panel.Children.Add(actions);
        return panel;
    }

    private FrameworkElement ProviderDetails(string provider, DashboardAccount[] accounts)
    {
        var panel = new StackPanel();
        for (int i = 0; i < accounts.Length; i++)
        {
            var account = accounts[i];
            var block = new StackPanel { Margin = new Thickness(0, i == 0 ? 0 : 12, 0, 0) };
            if (accounts.Length > 1)
            {
                var head = Ui.Text(string.Join(" · ", new[] { account.Email ?? account.Label, Formatting.PlanLabel(account.Plan), StatusWord(account) }.Where(part => !string.IsNullOrEmpty(part))), 12, "Ink2", FontWeights.SemiBold, trim: true);
                head.Margin = new Thickness(0, 0, 0, 8); block.Children.Add(head);
            }
            block.Children.Add(DetailWindows(account));
            AddDetailFooter(block, account);
            if (i > 0) panel.Children.Add(new Border { Height = 1, Background = Theme.Brush("Rule2"), Margin = new Thickness(0, 12, 0, 0) });
            panel.Children.Add(block);
        }
        return panel;
    }

    private FrameworkElement DetailWindows(DashboardAccount account)
    {
        var all = Formatting.VisibleWindows(account).Where(window => account.Provider != "qwen" || !Formatting.IsQwenDuplicateMetadata(window)).ToArray();
        var meterWindows = all.Where(window => window.Kind is not ("balance" or "extra_usage" or "spend") && !(account.Provider == "qwen" && window.Key.StartsWith("addon-", StringComparison.Ordinal))).ToArray();
        var amounts = all.Except(meterWindows).ToArray();
        var panel = new StackPanel();
        if (meterWindows.Length > 0)
        {
            var grid = new Grid();
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(18) });
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            for (int i = 0; i < meterWindows.Length; i++)
            {
                if (i % 2 == 0) grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
                var window = meterWindows[i];
                var used = window.Unlimited || window.Enabled == false ? null : window.DisplayPercent;
                var reset = window.ResetAt is null ? "No reset reported" : Formatting.ResetLong(window.ResetAt);
                if (window.Status == "cached") reset += " · cached " + Formatting.Relative(window.SampledAt);
                var meter = ShowMeter("detail:" + account.Id + "|" + window.Key, MeterKind.Detail, new MeterSpec(used, MeterTip(account, window, null), Label: Formatting.WindowLabel(account, window), Reset: reset, Amount: AmountText(window),
                    NaText: window.Enabled == false ? "Disabled" : window.Unlimited ? "Unlimited" : "Unavailable"));
                meter.Margin = new Thickness(0, i >= 2 ? 12 : 0, 8, 0);
                Grid.SetRow(meter, i / 2); Grid.SetColumn(meter, i % 2 == 0 ? 0 : 2); grid.Children.Add(meter);
            }
            panel.Children.Add(grid);
        }
        else panel.Children.Add(Ui.Text(account.Status == "needs_sign_in" ? "Sign-in required" : "Usage unavailable", 12, "WarnText"));
        if (amounts.Length > 0)
        {
            var list = new StackPanel { Margin = new Thickness(0, 12, 0, 0) };
            for (int i = 0; i < amounts.Length; i++)
            {
                var window = amounts[i];
                var row = new Grid { Margin = new Thickness(0, 0, 0, 0) };
                row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(18) });
                row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(8) });
                row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
                row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
                row.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
                row.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
                var icon = Icons.Icon(window.Kind == "balance" && window.Key.Contains("pack", StringComparison.OrdinalIgnoreCase) ? "pack" : "wallet", 14, Theme.Brush("Ink3"));
                row.Children.Add(icon);
                var label = Ui.Text(Formatting.WindowLabel(account, window), 12.5, "Ink2", trim: true); Grid.SetColumn(label, 2); row.Children.Add(label);
                var value = Ui.Text(AmountValue(window), 12.5, "Ink", FontWeights.SemiBold); Grid.SetColumn(value, 3); row.Children.Add(value);
                var subParts = new List<string>();
                if (window.ExpiresAt is not null || window.Kind is "balance") subParts.Add(Formatting.Expiration(window.ExpiresAt));
                if (window.Status == "cached") subParts.Add("Cached · " + Formatting.WindowSample(window.SampledAt));
                if (subParts.Count > 0) { var sub = Ui.Text(string.Join(" · ", subParts), 11.5, "Ink3", trim: true); Grid.SetRow(sub, 1); Grid.SetColumn(sub, 2); Grid.SetColumnSpan(sub, 2); row.Children.Add(sub); }
                var line = new Border { Child = row, Padding = new Thickness(0, 6, 0, 6), BorderBrush = Theme.Brush("Rule"), BorderThickness = new Thickness(0, i == 0 ? 0 : 1, 0, 0) };
                list.Children.Add(line);
            }
            panel.Children.Add(list);
        }
        return panel;
    }

    private static string AmountValue(QuotaWindow window)
    {
        if (window.Enabled == false) return "Disabled";
        if (window.Unlimited) return "Unlimited";
        if (window.Remaining is double remaining && double.IsFinite(remaining) && remaining >= 0) return Formatting.Amount(remaining, window.Unit) + " left";
        if (window.Used is double used && window.Limit is double limit) return Formatting.Amount(used, null) + " of " + Formatting.Amount(limit, window.Unit);
        if (window.Used is double spent && double.IsFinite(spent)) return Formatting.Amount(spent, window.Unit) + " used";
        if (window.DisplayPercent is double percent) return Formatting.Percent(percent) + " used";
        return "Unavailable";
    }

    private static void AddDetailFooter(StackPanel panel, DashboardAccount account)
    {
        if (!string.IsNullOrWhiteSpace(account.Message) && account.Message.Length < 400 && (account.Provider != "codex" || !Formatting.IsChatPass(account.Message)))
        {
            var message = Ui.Text(account.Message, 12, "Ink3", wrap: true); message.Margin = new Thickness(0, 10, 0, 0); panel.Children.Add(message);
        }
        var parts = new List<string> { Formatting.Sampled(account.SampledAt ?? account.FetchedAt) };
        if (account.Status == "cached") parts.Add("cached");
        var platform = Formatting.PlatformName(account.Platform); if (platform.Length > 0) parts.Add(platform);
        var meta = Ui.Text(string.Join(" · ", parts), 11.5, "Ink3"); meta.Margin = new Thickness(0, 8, 0, 0); panel.Children.Add(meta);
    }

    // ---------------- footer: Codex auto-switch, Dashboard, Refresh, Settings

    private void RenderFooter()
    {
        DashboardButton.IsEnabled = client is not null;
        RefreshButton.IsEnabled = client is not null && !busy;
        if (client is null || dashboard is null)
        {
            AutoSwitchPanel.Content = Ui.Text(client is null ? "Usage appears after you connect this tray" : "Loading accounts", 12, "Ink3");
            return;
        }
        var status = dashboard.CodexAutoSwitch;
        var row = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        var toggle = new ToggleSwitch(status.Enabled, "Codex auto-switch", "Automatically switch Codex accounts when usage reaches the threshold and Codex is idle. Claude stays manual.") { Uid = "mutation:toggle" };
        toggle.SetEnabled(!status.ActivationInProgress && !busy && !staleSample);
        toggle.Toggled += async requested => await Action(async () => { if (client is not null) await client.SetAutoSwitch(requested); }, requested ? "Codex auto-switch is on." : "Codex auto-switch is off.");
        row.Children.Add(toggle);
        var currentUsed = 100 - (int)Math.Round(status.ThresholdPercent);
        var drop = ThresholdDrop(currentUsed, "Codex switch threshold", used => Action(async () => { if (client is not null) await client.SetAutoSwitch(status.Enabled, 100 - used); }, "Codex switch threshold updated."));
        drop.Margin = new Thickness(10, 0, 4, 0); drop.IsEnabled = !status.ActivationInProgress && !busy && !staleSample;
        row.Children.Add(drop);
        var info = Ui.IconButton("info", "How Codex auto-switch works", 28, "BareIconButton", 16);
        info.Click += (_, _) =>
        {
            var text = new StackPanel { MaxWidth = 340, Margin = new Thickness(8, 6, 8, 6) };
            text.Children.Add(Ui.Text("Codex auto-switch", 12.5, "Ink", FontWeights.SemiBold));
            var body = Ui.Text($"Checks every {status.PollIntervalSeconds} s and switches at {currentUsed}% used ({Formatting.Percent(status.ThresholdPercent)} left) once Codex is idle. Claude stays manual.", 12, "Ink2", wrap: true);
            body.Margin = new Thickness(0, 4, 0, 0); text.Children.Add(body);
            if (!string.IsNullOrWhiteSpace(status.Message) && status.Message.Length < 300) { var message = Ui.Text(status.Message, 12, "Ink3", wrap: true); message.Margin = new Thickness(0, 6, 0, 0); text.Children.Add(message); }
            ShowPopup(Menus.Create(info, text, PlacementMode.Top, 260));
        };
        row.Children.Add(info);
        AutoSwitchPanel.Content = row;
    }

    private Button ThresholdDrop(int currentUsed, string name, Func<int, Task> choose)
    {
        var content = new StackPanel { Orientation = Orientation.Horizontal };
        content.Children.Add(Ui.Text("at ", 12.5, "Ink3"));
        var value = new TextBlock { Text = currentUsed + "%", FontSize = 12.5, FontWeight = FontWeights.Medium, Foreground = Theme.Brush("Ink") };
        content.Children.Add(value);
        var chevron = Icons.Icon("chevDown", 13, Theme.Brush("Ink3")); chevron.Margin = new Thickness(5, 0, 0, 0); chevron.RenderTransformOrigin = new Point(0.5, 0.5); chevron.RenderTransform = new RotateTransform();
        content.Children.Add(chevron);
        var drop = new Button { Style = (Style)FindResource("AtlasButton"), Content = content, Padding = new Thickness(9, 0, 7, 0), Uid = "mutation:drop" };
        System.Windows.Automation.AutomationProperties.SetName(drop, name);
        drop.Click += (_, _) =>
        {
            var list = new StackPanel();
            Popup? popup = null;
            foreach (var choice in new[] { 85, 90, 95, 98 }.Append(currentUsed).Distinct().OrderBy(x => x))
                list.Children.Add(Menus.Item("at " + choice + "% used", choice == currentUsed, async () => { if (popup is not null) popup.IsOpen = false; if (choice != currentUsed) await choose(choice); }));
            popup = Menus.Create(drop, list, PlacementMode.Top, 150);
            ShowPopup(popup, chevron);
        };
        return drop;
    }

    private FrameworkElement AntigravityTools(DashboardAccount[] accounts)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        var status = dashboard?.AntigravityAutoSwitch;
        if (!AntigravitySwitchable(accounts) || status is null)
        {
            var off = new ToggleSwitch(false, "Auto-switch", "Switching moves between Antigravity accounts, so it starts once a second account is signed in.");
            off.SetEnabled(false); row.Children.Add(off);
            var note = Ui.Text(status is null ? "not reported by this server" : "starts with a second signed-in account", 12, "Ink3"); note.Margin = new Thickness(8, 0, 0, 0); note.VerticalAlignment = VerticalAlignment.Center;
            row.Children.Add(note);
            return row;
        }
        var toggle = new ToggleSwitch(status.Enabled, "Auto-switch", "Automatically switch Antigravity accounts at the threshold once Antigravity is idle.") { Uid = "mutation:toggle" };
        toggle.SetEnabled(!status.ActivationInProgress && !busy && !staleSample);
        toggle.Toggled += async requested => await Action(async () => { if (client is not null) await client.SetAntigravityAutoSwitch(enabled: requested); }, requested ? "Antigravity auto-switch is on." : "Antigravity auto-switch is off.");
        row.Children.Add(toggle);
        var used = (int)Math.Round(status.ThresholdUsedPercent);
        var drop = ThresholdDrop(used, "Antigravity switch threshold", value => Action(async () => { if (client is not null) await client.SetAntigravityAutoSwitch(thresholdUsedPercent: value); }, "Antigravity switch threshold updated."));
        drop.Margin = new Thickness(10, 0, 0, 0); drop.IsEnabled = !status.ActivationInProgress && !busy && !staleSample;
        row.Children.Add(drop);
        return row;
    }

    // ------------------------------------------------------------------ actions

    private async Task OpenClaude(DashboardAccount account, string platform)
    {
        await Action(async () =>
        {
            var id = account.Capabilities.ClaudeProfileId;
            if (!Formatting.IsSafeClaudeProfile(id)) throw new InvalidOperationException("Choose a configured Claude account.");
            if (platform == "mac") { if (client is not null) await client.OpenClaudeOnMac(id!); }
            else Process.Start(new ProcessStartInfo("ccs-claude://launch/" + id) { UseShellExecute = true });
        }, "Opening " + ShortName(account) + " in Claude on " + (platform == "mac" ? "Mac" : "Windows") + ".", refresh: false);
    }

    private async Task ActivateCodex(DashboardAccount account)
    {
        if (busy || staleSample || client is null) return;
        var profile = account.Capabilities.CodexProfile!;
        await Switch(account, "Checking running Codex programs", () => CodexSwitchFlow.Run(profile,
            async token => { await client.Activate(profile, token); },
            confirmation => Confirm("Codex", account, confirmation.Warning, confirmation.Processes, () => confirmation.Expired)));
    }

    private async Task ActivateAntigravity(DashboardAccount account)
    {
        if (busy || staleSample || client is null) return;
        var profile = account.Capabilities.AntigravityProfileId!;
        await Switch(account, "Checking running Antigravity programs", () => AntigravitySwitchFlow.Run(profile,
            async token => { await client.ActivateAntigravity(profile, token); },
            confirmation => Confirm("Antigravity", account, confirmation.Warning, confirmation.Processes, () => confirmation.Expired)));
    }

    private Task<bool> Confirm(string provider, DashboardAccount account, string warning, IReadOnlyList<CodexSwitchProcess> processes, Func<bool> expired)
    {
        confirmationVisible = true;
        try
        {
            var dialog = new SwitchConfirmDialog(provider, account.Email ?? account.Label, warning, processes, expired) { Owner = this };
            return Task.FromResult(dialog.ShowDialog() == true);
        }
        finally { confirmationVisible = false; }
    }

    private async Task Switch(DashboardAccount account, string checking, Func<Task<bool>> run)
    {
        if (client is null) return;
        busy = true; DisableMutations();
        statusFlash = checking; UpdateStatus();
        try
        {
            if (!await run()) { statusFlash = "Switch cancelled. The " + Formatting.ProviderName(account.Provider) + " account was not changed."; return; }
            drawCheck.Add(account.Provider + ":" + account.Id);
            var sample = await client.Dashboard(false);
            dashboard = sample; staleSample = false;
            statusFlash = Formatting.ProviderName(account.Provider) + " switched to " + ShortName(account) + ".";
        }
        catch (Exception error)
        {
            statusFlash = DisplayError(error);
            if (dashboard is not null) staleSample = true;
        }
        finally
        {
            FinishRequest();
            ClearFlashLater();
        }
    }

    private async Task Action(Func<Task> action, string success, bool refresh = true)
    {
        if (busy || staleSample && refresh || client is null) return;
        busy = true; DisableMutations();
        statusFlash = "Working"; UpdateStatus();
        try
        {
            await action();
            statusFlash = success;
            if (refresh) { dashboard = await client.Dashboard(false); staleSample = false; }
        }
        catch (Exception error)
        {
            statusFlash = DisplayError(error);
            if (refresh && dashboard is not null) staleSample = true;
        }
        finally { FinishRequest(); ClearFlashLater(); }
    }

    private void ClearFlashLater()
    {
        var flash = statusFlash;
        var clear = new DispatcherTimer { Interval = TimeSpan.FromSeconds(4) };
        clear.Tick += (_, _) => { clear.Stop(); if (statusFlash == flash) { statusFlash = null; UpdateStatus(); } };
        clear.Start();
    }

    private void DisableMutations()
    {
        foreach (var element in Descendants(ContentPanel).Concat(Descendants(AutoSwitchPanel)))
        {
            if (element is Button button && button.Uid.StartsWith("mutation", StringComparison.Ordinal)) button.IsEnabled = false;
            if (element is ToggleSwitch toggle) toggle.SetEnabled(false);
        }
    }

    private static IEnumerable<FrameworkElement> Descendants(DependencyObject parent)
    {
        foreach (var child in LogicalTreeHelper.GetChildren(parent).OfType<FrameworkElement>())
        {
            yield return child;
            foreach (var nested in Descendants(child)) yield return nested;
        }
    }

    private static string DisplayError(Exception error) => error is InvalidOperationException or ArgumentException ? error.Message : error is TaskCanceledException ? "The dashboard took too long to respond. Try Refresh." : "Could not reach the dashboard. Check Settings or try Refresh.";

    // ------------------------------------------------------------------ fade under the list

    private void PaintFade()
    {
        var panel = Theme.Color("Panel", Theme.IsDark);
        Fade.Background = new LinearGradientBrush(Color.FromArgb(0, panel.R, panel.G, panel.B), panel, 90);
    }

    private void UpdateFade()
    {
        var more = ContentScroll.ScrollableHeight - ContentScroll.VerticalOffset > 1;
        Motion.To(Fade, OpacityProperty, more ? 1 : 0, 200);
    }

    // ------------------------------------------------------------------ footer buttons

    private async void RefreshClicked(object sender, RoutedEventArgs e) => await Refresh(true);
    private void SettingsClicked(object sender, RoutedEventArgs e)
    {
        if (settingsVisible) CloseSettings(); else OpenSettings();
    }
    private void DashboardClicked(object sender, RoutedEventArgs e) => OpenDashboard();
    public void OpenDashboard() { if (client is not null) Process.Start(new ProcessStartInfo(client.BaseURL.ToString()) { UseShellExecute = true }); }

    protected override void OnClosing(CancelEventArgs e)
    {
        if (!AllowClose) { e.Cancel = true; HidePopup(); }
        base.OnClosing(e);
    }
    protected override void OnClosed(EventArgs e) { timer.Stop(); client?.Dispose(); Theme.Changed -= PaintFade; base.OnClosed(e); }
}
