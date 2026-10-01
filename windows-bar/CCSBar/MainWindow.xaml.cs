using System;
using System.ComponentModel;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.Linq;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Threading;

namespace CCSBar;

public partial class MainWindow : Window
{
    private static readonly Brush Muted = ColorBrush("#A4C1E4");
    private static readonly Brush Accent = ColorBrush("#3498FF");
    private static readonly Brush Card = ColorBrush("#102033");
    private static readonly string[] ProviderOrder = { "claude", "codex", "cursor", "muse", "antigravity", "kimi-code", "qwen", "zai", "opencode-go" };
    private DashboardClient? client;
    private ConnectionSettings? connection;
    private AccountDashboard? dashboard;
    private readonly DispatcherTimer timer;
    private bool busy;
    private bool settingsVisible;
    private readonly HashSet<string> expandedProviders = new(StringComparer.Ordinal);
    private readonly HashSet<string> expandedAccounts = new(StringComparer.Ordinal);
    private bool autoDetailsVisible;
    private bool staleSample;
    private bool confirmationVisible;
    private DateTimeOffset lastFailure = DateTimeOffset.MinValue;

    public MainWindow()
    {
        InitializeComponent();
        Background = Brushes.Transparent;
        Width = Math.Min(760, SystemParameters.WorkArea.Width - 24);
        Height = Math.Min(850, SystemParameters.WorkArea.Height - 24);
        Deactivated += (_, _) => { if (!settingsVisible && !confirmationVisible) Hide(); };
        PreviewKeyDown += (_, e) => { if (e.Key == Key.Escape) { settingsVisible = false; Hide(); } };
        timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(60) };
        timer.Tick += async (_, _) => await Refresh(false);
        timer.Start();
        try
        {
            connection = SecureStore.Load();
            if (connection is not null) client = new DashboardClient(connection);
        }
        catch { StatusText.Text = "Stored connection could not be unlocked. Enter it again in Connection."; }
        if (client is null) RenderConnection();
    }

    public bool IsConfigured => client is not null;
    internal bool AllowClose { get; set; }

    public async Task OpenPopup()
    {
        PositionPopup();
        Show(); Activate();
        if (!settingsVisible) await Refresh(true);
    }

    private void PositionPopup()
    {
        // Keep the entire popup inside the taskbar's monitor, including scaled desktops.
        var monitor = System.Windows.Forms.Screen.FromPoint(System.Windows.Forms.Control.MousePosition);
        var dpi = VisualTreeHelper.GetDpi(this);
        var area = monitor.WorkingArea;
        Width = Math.Min(760, area.Width / dpi.DpiScaleX - 24);
        Height = Math.Min(850, area.Height / dpi.DpiScaleY - 24);
        Left = area.Right / dpi.DpiScaleX - Width - 10;
        Top = area.Bottom / dpi.DpiScaleY - Height - 10;
    }

    public async Task Refresh(bool force)
    {
        if (busy || settingsVisible || client is null) return;
        if (!force && DateTimeOffset.UtcNow - lastFailure < TimeSpan.FromMinutes(1)) return;
        busy = true; RefreshButton.IsEnabled = false;
        StatusText.Text = dashboard is null ? "Loading accounts…" : "Refreshing usage…";
        StatusDot.Fill = Accent;
        try
        {
            dashboard = await client.Dashboard(force);
            timer.Interval = TimeSpan.FromSeconds(dashboard.Settings?.ValidatedInterval ?? 60);
            staleSample = false;
            lastFailure = DateTimeOffset.MinValue;
            UpdateDashboardStatus();
            RenderDashboard();
        }
        catch (Exception error)
        {
            lastFailure = DateTimeOffset.UtcNow;
            StatusText.Text = DisplayError(error);
            StatusDot.Fill = ColorBrush("#DDBE72");
            if (dashboard is null) RenderEmpty("Usage is unavailable", "Check the dashboard connection or try Refresh.");
            else MarkStale();
        }
        finally { busy = false; RefreshButton.IsEnabled = true; }
    }

    private void UpdateDashboardStatus()
    {
        if (dashboard is null) return;
        var groups = dashboard.Accounts.GroupBy(account => account.Provider).ToArray();
        var reporting = groups.Count(group => group.Any(account => account.Status == "ok" || account.Status == "cached" && account.HasUsableUsage));
        var cached = groups.Any(group => !group.Any(account => account.Status == "ok") && group.Any(account => account.Status == "cached" && account.HasUsableUsage));
        StatusText.Text = $"{reporting}/{groups.Length} providers reporting" + (cached ? " · cached usage" : "") + $" · {Formatting.Sampled(dashboard.UpdatedAt)}";
        StatusDot.Fill = ColorBrush(reporting == groups.Length && !cached ? "#30CE88" : "#DDBE72");
    }

    private void MarkStale()
    {
        staleSample = true;
        var active = dashboard?.Accounts.FirstOrDefault(account => account.Provider == "codex" && account.IsActive);
        ActiveCodexText.Text = "Codex active: " + (active?.Email ?? active?.Label ?? "unavailable") + " · last confirmed";
        ActiveCodexText.ToolTip = ActiveCodexText.Text;
        // Keep the last sample visible, but never present it as a successful fresh poll.
        if (ContentPanel.Children.Count > 0 && ContentPanel.Children[0] is Border previous && previous.Tag as string == "stale")
            ContentPanel.Children.RemoveAt(0);
        var warning = new Border { Background = ColorBrush("#293449"), CornerRadius = new CornerRadius(7), Padding = new Thickness(9), Margin = new Thickness(0, 0, 0, 8), Tag = "stale" };
        warning.Child = Text("Showing the last confirmed sample. Refresh to verify current usage, account and settings.", 11, ColorBrush("#DDBE72"), wrap: true);
        ContentPanel.Children.Insert(0, warning);
        // Prevent decisions made from stale active-account information.
        SetButtonsEnabled(ContentPanel, false);
    }

    private void RenderDashboard()
    {
        if (dashboard is null) return;
        ContentPanel.Children.Clear();
        var active = dashboard.Accounts.FirstOrDefault(account => account.Provider == "codex" && account.IsActive);
        ActiveCodexText.Text = "Codex active: " + (active?.Email ?? active?.Label ?? "unavailable") + (staleSample ? " · last confirmed" : "");
        ActiveCodexText.ToolTip = ActiveCodexText.Text;
        ContentPanel.Children.Add(AutoCard(dashboard.CodexAutoSwitch));
        foreach (var provider in ProviderOrder)
        {
            var accounts = dashboard.Accounts.Where(account => account.Provider == provider).ToArray();
            if (accounts.Length == 0) continue;
            if (provider is "claude" or "codex")
            {
                var label = Section((provider == "claude" ? "CLAUDE" : "CODEX") + $"  ({accounts.Length})");
                label.Margin = new Thickness(3, 3, 0, 6);
                ContentPanel.Children.Add(label);
                foreach (var account in accounts) ContentPanel.Children.Add(AccountOverview(account));
            }
            else ContentPanel.Children.Add(ProviderGroup(provider, accounts));
        }
        if (dashboard.Accounts.Count == 0) RenderEmpty("No accounts found", "Your signed-in accounts will appear here when CCS discovers them.");
        else if (staleSample) MarkStale();
    }

    private UIElement AutoCard(AutoSwitchStatus status)
    {
        var panel = new StackPanel();
        var row = new Grid();
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var left = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        left.Children.Add(Text("Codex auto-switch", 13, Muted));
        var toggle = new CheckBox { IsChecked = status.Enabled, IsEnabled = !status.ActivationInProgress, Style = (Style)FindResource("SwitchStyle"), Margin = new Thickness(13, 0, 0, 0), ToolTip = "Automatic Codex account switching" };
        toggle.Click += async (_, _) =>
        {
            var requested = toggle.IsChecked == true;
            // Show only confirmed state while the server persists the setting.
            toggle.IsChecked = status.Enabled;
            await Action(async () => { if (client is not null) await client.SetAutoSwitch(requested); }, "Automatic switching updated.");
        };
        left.Children.Add(toggle); row.Children.Add(left);
        var threshold = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(15, 0, 12, 0) };
        var caption = Text("Threshold", 12, Muted); caption.VerticalAlignment = VerticalAlignment.Center; caption.Margin = new Thickness(0, 0, 9, 0); threshold.Children.Add(caption);
        int currentUsed = 100 - (int)Math.Round(status.ThresholdPercent);
        var picker = new ComboBox { Width = 92, Style = (Style)FindResource("ThresholdPickerStyle"), IsEnabled = !status.ActivationInProgress, ToolTip = "Codex used percentage that triggers an idle account switch" };
        var choices = new[] { 85, 90, 95, 98 }.Append(currentUsed).Distinct().OrderBy(value => value);
        foreach (var value in choices) picker.Items.Add(new ComboBoxItem { Content = value + "% used", Tag = value });
        picker.SelectedItem = picker.Items.Cast<ComboBoxItem>().First(item => (int)item.Tag == currentUsed);
        picker.SelectionChanged += async (_, _) =>
        {
            if (picker.SelectedItem is not ComboBoxItem selected || (int)selected.Tag == currentUsed) return;
            var remaining = 100 - (int)selected.Tag;
            picker.SelectedItem = picker.Items.Cast<ComboBoxItem>().First(item => (int)item.Tag == currentUsed);
            await Action(async () => { if (client is not null) await client.SetAutoSwitch(status.Enabled, remaining); }, "Automatic switch threshold updated.");
        };
        threshold.Children.Add(picker);
        Grid.SetColumn(threshold, 1); row.Children.Add(threshold);
        var details = new Button { Content = "ⓘ", Padding = new Thickness(7, 1, 7, 1), Background = Brushes.Transparent, Tag = "read-only", ToolTip = "Automatic switch settings and status" };
        details.Click += (_, _) => { autoDetailsVisible = !autoDetailsVisible; RenderDashboard(); };
        Grid.SetColumn(details, 2); row.Children.Add(details); panel.Children.Add(row);
        if (autoDetailsVisible)
        {
            var help = Text($"Check every {status.PollIntervalSeconds}s; switch at {100 - status.ThresholdPercent:0}% used ({status.ThresholdPercent:0}% remaining) once Codex is idle. Claude stays manual.", 11, Muted, wrap: true);
            help.Margin = new Thickness(0, 10, 0, 0); panel.Children.Add(help);
            if (!string.IsNullOrWhiteSpace(status.Message)) panel.Children.Add(Text(status.Message, 11, Muted, wrap: true));
        }
        return CardBorder(panel);
    }

    private UIElement AccountOverview(DashboardAccount account)
    {
        var group = new StackPanel();
        var row = CompactRow();
        var identity = Identity(account.Provider, account.Email ?? account.Label,
            string.Join(" · ", new[] { account.Provider == "codex" && account.IsActive ? "✓ Active" : null, account.Plan, AccountStatus(account) }.Where(value => !string.IsNullOrWhiteSpace(value))));
        row.Children.Add(identity);
        var usage = PrimaryUsage(account);
        Grid.SetColumn(usage, 1); row.Children.Add(usage);
        var controls = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        AddControls(controls, account, compact: true);
        controls.Children.Add(InfoButton("View all usage windows and details for this account", () =>
        {
            if (!expandedAccounts.Add(account.Id)) expandedAccounts.Remove(account.Id);
            RenderPreservingScroll();
        }));
        Grid.SetColumn(controls, 2); row.Children.Add(controls); group.Children.Add(row);
        if (expandedAccounts.Contains(account.Id))
        {
            group.Children.Add(Divider());
            group.Children.Add(AccountCard(account, includeControls: false));
        }
        var plate = CompactPlate(group); plate.Tag = "account-row:" + account.Id;
        if (account.Provider == "codex" && account.IsActive)
        {
            plate.Background = ColorBrush("#15314C"); plate.BorderBrush = ColorBrush("#3498FF");
            plate.BorderThickness = new Thickness(2); plate.Padding = new Thickness(9, 4, 8, 4);
        }
        return plate;
    }

    private UIElement ProviderGroup(string provider, DashboardAccount[] accounts)
    {
        var representative = accounts.FirstOrDefault(account => account.IsActive)
            ?? accounts.FirstOrDefault(account => account.Status == "ok")
            ?? accounts.FirstOrDefault(account => account.Status == "cached") ?? accounts[0];
        var group = new StackPanel();
        var row = CompactRow();
        var label = provider switch { "muse" => "Muse Code", "cursor" => "Cursor", "antigravity" => "Google Antigravity CLI", "kimi-code" => "Kimi Code", "qwen" => "Qwen Token Plan", "zai" => "Z.ai Coding Plan", "opencode-go" => "OpenCode Go", _ => representative.ProviderLabel };
        row.Children.Add(Identity(provider, label + (accounts.Length > 1 ? $"  ({accounts.Length})" : ""), AccountStatus(representative)));
        var usage = PrimaryUsage(representative); Grid.SetColumn(usage, 1); row.Children.Add(usage);
        var info = InfoButton("View all accounts and usage details", () =>
        {
            if (!expandedProviders.Add(provider)) expandedProviders.Remove(provider);
            RenderPreservingScroll();
        });
        Grid.SetColumn(info, 2); row.Children.Add(info); group.Children.Add(row);
        if (expandedProviders.Contains(provider))
        {
            group.Children.Add(Divider());
            foreach (var account in accounts) group.Children.Add(AccountCard(account, includeControls: false));
        }
        return CompactPlate(group);
    }

    private static Grid CompactRow()
    {
        var row = new Grid { MinHeight = 43 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(210) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        return row;
    }

    private static Grid Identity(string provider, string label, string status)
    {
        var identity = new Grid { Margin = new Thickness(0, 0, 9, 0) };
        identity.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(38) });
        identity.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        identity.Children.Add(ProviderIcon(provider));
        var words = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        var title = Text(label, label.Length > 23 ? 10.5 : 12, weight: FontWeights.SemiBold);
        title.TextTrimming = TextTrimming.CharacterEllipsis; title.ToolTip = label; words.Children.Add(title);
        var detail = Text(status, 9, Muted); detail.Margin = new Thickness(0, 3, 0, 0);
        detail.TextTrimming = TextTrimming.CharacterEllipsis; detail.ToolTip = status; words.Children.Add(detail);
        Grid.SetColumn(words, 1); identity.Children.Add(words); return identity;
    }

    private static string AccountStatus(DashboardAccount account) => account.Status switch
    {
        "ok" => "Usage available", "cached" => "Cached usage", "needs_sign_in" => "Sign-in required", _ => "Usage unavailable"
    };

    private static Grid PrimaryUsage(DashboardAccount account)
    {
        var windows = PrimaryWindows(account);
        var usage = new Grid { VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(0, 0, 5, 0) };
        if (windows.Length == 0) usage.Children.Add(Text(account.Status == "needs_sign_in" ? "Sign-in required" : "Usage unavailable", 11, Muted));
        else for (int i = 0; i < windows.Length; i++)
        {
            usage.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            var glance = PrimaryWindow(windows[i], account.Provider == "codex");
            Grid.SetColumn(glance, i); usage.Children.Add(glance);
        }
        return usage;
    }

    private static Border CompactPlate(UIElement child)
    {
        var plate = CardBorder(child); plate.Padding = new Thickness(10, 5, 9, 5); plate.Margin = new Thickness(0, 0, 0, 6);
        return plate;
    }

    private static Border Divider() => new() { BorderBrush = ColorBrush("#20364E"), BorderThickness = new Thickness(0, 1, 0, 0), Margin = new Thickness(0, 9, 0, 8) };

    private static Button InfoButton(string tooltip, System.Action click)
    {
        var button = new Button { Content = "ⓘ", Width = 22, Height = 24, Padding = new Thickness(2), Background = Brushes.Transparent, Tag = "read-only", ToolTip = tooltip, Margin = new Thickness(4, 0, 0, 0), VerticalAlignment = VerticalAlignment.Center };
        button.Click += (_, _) => click(); return button;
    }

    private void RenderPreservingScroll()
    {
        var offset = ContentScroll.VerticalOffset; RenderDashboard();
        Dispatcher.BeginInvoke(new System.Action(() => ContentScroll.ScrollToVerticalOffset(offset)), DispatcherPriority.Loaded);
    }

    private static QuotaWindow[] PrimaryWindows(DashboardAccount account)
    {
        var regular = account.Windows.Where(window => window.Kind is not ("balance" or "extra_usage")).ToArray();
        if (regular.Length == 0) return account.Windows.Take(3).ToArray();
        var selected = new List<QuotaWindow>();
        foreach (var duration in new[] { 300d, 10080d, 43200d })
        {
            var window = regular.FirstOrDefault(candidate => candidate.WindowMinutes == duration);
            if (window is not null && !selected.Contains(window)) selected.Add(window);
        }
        foreach (var window in regular) if (selected.Count < 3 && !selected.Contains(window)) selected.Add(window);
        var balance = account.Windows.FirstOrDefault(window => window.Kind == "balance");
        if (balance is not null && account.Provider is not ("claude" or "codex")) return selected.Take(2).Concat(new[] { balance }).ToArray();
        return selected.Take(3).ToArray();
    }

    private static UIElement PrimaryWindow(QuotaWindow window, bool percentOnly)
    {
        var panel = new StackPanel { Margin = new Thickness(6, 0, 9, 0) };
        var heading = new Grid();
        heading.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        heading.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var name = Text(window.Label, 10, Muted); name.TextTrimming = TextTrimming.CharacterEllipsis; name.ToolTip = window.Label; heading.Children.Add(name);
        var amount = window.Enabled == false ? "Disabled" : window.Unlimited ? "Unlimited" : percentOnly && window.DisplayPercent is double codex ? $"{codex:0}%" : window.Used is double used && window.Limit is double limit ? $"{used:0.##} / {limit:0.##}" + (string.IsNullOrEmpty(window.Unit) ? "" : " " + window.Unit) : window.DisplayPercent is double percentage ? $"{percentage:0}%" : window.Remaining is double remaining ? Formatting.Amount(remaining, window.Unit) : "—";
        var value = Text(amount, 10, Muted); value.Margin = new Thickness(7, 0, 0, 0); Grid.SetColumn(value, 1); heading.Children.Add(value); panel.Children.Add(heading);
        if (window.Kind == "balance") value.Visibility = Visibility.Collapsed;
        var track = new Border { Height = 6, Background = ColorBrush("#203A55"), CornerRadius = new CornerRadius(3), Margin = new Thickness(0, 5, 0, 0) };
        if (window.DisplayPercent is double percent && !window.Unlimited && window.Enabled != false)
        {
            var fill = new Border { Height = 6, Background = Accent, CornerRadius = new CornerRadius(3), HorizontalAlignment = HorizontalAlignment.Left };
            track.Child = fill; track.SizeChanged += (_, _) => fill.Width = track.ActualWidth * Math.Clamp(percent, 0, 100) / 100;
        }
        if (window.Kind == "balance")
        {
            var chip = new Border { Child = Text(window.Enabled == false ? "Disabled" : window.Unlimited ? "Unlimited" : window.Remaining is double left ? Formatting.Amount(left, window.Unit) : "Balance unavailable", 10, Muted), Background = ColorBrush("#142C46"), BorderBrush = ColorBrush("#274566"), BorderThickness = new Thickness(1), CornerRadius = new CornerRadius(5), Padding = new Thickness(6, 3, 6, 3), Margin = new Thickness(0, 3, 0, 0) };
            panel.Children.Add(chip);
        }
        else panel.Children.Add(track);
        var timing = window.Kind is "balance" or "extra_usage" ? Formatting.Expiration(window.ExpiresAt) : Formatting.Reset(window.ResetAt);
        var reset = Text(window.Kind is "balance" or "extra_usage" ? timing : Formatting.ShortReset(window.ResetAt), 9, Muted); reset.Margin = new Thickness(0, 4, 0, 0);
        reset.TextTrimming = TextTrimming.CharacterEllipsis; reset.ToolTip = timing; panel.Children.Add(reset);
        return panel;
    }

    private static UIElement ProviderIcon(string provider)
    {
        var plate = new Border { Width = 31, Height = 34, CornerRadius = new CornerRadius(7), Background = ColorBrush("#16283F"), HorizontalAlignment = HorizontalAlignment.Left, VerticalAlignment = VerticalAlignment.Center };
        if (provider is "claude" or "codex" or "cursor" or "kimi-code" or "qwen" or "zai" or "antigravity")
            plate.Child = new Image { Source = new BitmapImage(new Uri($"pack://application:,,,/CCSBar;component/Resources/Providers/{provider}.png")), Width = 25, Height = 25 };
        else if (provider == "muse")
        {
            plate.Child = new System.Windows.Shapes.Path { Data = Geometry.Parse("M3,23 L7,7 Q9,3 12,7 L16,21 L20,7 Q23,3 25,7 L29,23"), Stroke = ColorBrush("#7E5EFF"), StrokeThickness = 5, StrokeStartLineCap = PenLineCap.Round, StrokeEndLineCap = PenLineCap.Round, StrokeLineJoin = PenLineJoin.Round, Stretch = Stretch.Uniform, Width = 25, Height = 25 };
        }
        else { var infinity = Text("∞", 30, ColorBrush("#756EFF"), FontWeights.SemiBold); infinity.HorizontalAlignment = HorizontalAlignment.Center; infinity.Margin = new Thickness(0, -5, 0, 0); plate.Child = infinity; }
        return plate;
    }

    private UIElement AccountCard(DashboardAccount account, bool includeControls = true)
    {
        var panel = new StackPanel();
        var title = new DockPanel { LastChildFill = true };
        if (account.IsActive)
        {
            var badge = Badge("ACTIVE", "#143D58", "#82C9FF");
            DockPanel.SetDock(badge, Dock.Right); title.Children.Add(badge);
        }
        if (!string.IsNullOrEmpty(account.Plan))
        {
            var badge = Badge(account.Plan.ToUpperInvariant(), "#203651", "#B7D7FF");
            DockPanel.SetDock(badge, Dock.Right); title.Children.Add(badge);
        }
        title.Children.Add(Text(string.IsNullOrWhiteSpace(account.Label) ? account.ProviderLabel : account.Label, 13, weight: FontWeights.SemiBold));
        panel.Children.Add(title);
        var identity = Text(account.Email ?? account.Label, 11, Muted);
        identity.TextTrimming = TextTrimming.CharacterEllipsis;
        identity.ToolTip = account.Email ?? account.Label;
        identity.Margin = new Thickness(0, 3, 0, 7);
        panel.Children.Add(identity);
        foreach (var quota in account.Windows) panel.Children.Add(QuotaRow(quota, account.Provider == "codex"));
        if (account.Windows.Count == 0)
            panel.Children.Add(Text(account.Status == "needs_sign_in" ? "Sign-in required" : "Usage unavailable", 11, ColorBrush("#DBAB4F")));
        if (!string.IsNullOrWhiteSpace(account.Message))
            panel.Children.Add(Text(account.Message, 11, Muted, wrap: true));
        var detail = Formatting.Sampled(account.SampledAt ?? account.FetchedAt);
        if (account.Status == "cached") detail += " · cached";
        if (!string.IsNullOrEmpty(account.Platform)) detail += " · " + (account.Platform == "mac" ? "Mac" : account.Platform == "windows" ? "Windows" : "Ubuntu");
        var sampled = Text(detail, 10, Muted); sampled.Margin = new Thickness(0, 6, 0, 0); panel.Children.Add(sampled);
        if (includeControls) AddControls(panel, account);
        return CardBorder(panel);
    }

    private UIElement QuotaRow(QuotaWindow quota, bool percentOnly)
    {
        var panel = new StackPanel { Margin = new Thickness(0, 2, 0, 5) };
        var line = new Grid();
        line.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(180) });
        line.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        line.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(43) });
        var label = Text(quota.Label, 11, Muted, wrap: true); label.ToolTip = quota.Label;
        line.Children.Add(label);
        var track = new Border { Background = ColorBrush("#203A55"), CornerRadius = new CornerRadius(3), Height = 6, Margin = new Thickness(3, 5, 7, 0), VerticalAlignment = VerticalAlignment.Top };
        Grid.SetColumn(track, 1);
        if (quota.DisplayPercent is double used && !quota.Unlimited && quota.Enabled != false)
        {
            var fill = new Border { Background = Band(used), CornerRadius = new CornerRadius(3), HorizontalAlignment = HorizontalAlignment.Left };
            track.Child = fill;
            track.SizeChanged += (_, _) => fill.Width = track.ActualWidth * Math.Clamp(used, 0, 100) / 100;
        }
        if (quota.Unlimited || quota.Enabled == false)
        {
            var state = Text(quota.Enabled == false ? "Disabled" : "Unlimited", 11, Muted);
            state.Margin = new Thickness(3, 0, 0, 0); Grid.SetColumn(state, 1); line.Children.Add(state);
        }
        else line.Children.Add(track);
        var percent = Text(!quota.Unlimited && quota.Enabled != false && quota.DisplayPercent is double value ? value.ToString("0", CultureInfo.CurrentCulture) + "%" : quota.Unlimited || quota.Enabled == false ? "" : "—", 11);
        percent.HorizontalAlignment = HorizontalAlignment.Right; Grid.SetColumn(percent, 2); line.Children.Add(percent);
        panel.Children.Add(line);
        var reset = Text(Formatting.Reset(quota.ResetAt), 10, Muted); reset.Margin = new Thickness(183, 2, 0, 0); reset.TextWrapping = TextWrapping.Wrap;
        panel.Children.Add(reset);
        var counts = new System.Collections.Generic.List<string>();
        if (quota.Used is double amount) counts.Add("Used " + Formatting.Amount(amount, quota.Unit));
        if (quota.Limit is double limit) counts.Add("Limit " + Formatting.Amount(limit, quota.Unit));
        if (quota.Remaining is double remaining) counts.Add("Remaining " + Formatting.Amount(remaining, quota.Unit));
        if (counts.Count > 0 && (!percentOnly || quota.Kind is "balance" or "extra_usage"))
        {
            var detail = Text(string.Join(" · ", counts), 10, Muted, wrap: true);
            detail.Margin = new Thickness(0, 2, 0, 0); panel.Children.Add(detail);
        }
        if (quota.ExpiresAt is not null || quota.Kind is "balance" or "extra_usage")
        {
            var expires = Text(Formatting.Expiration(quota.ExpiresAt), 10, Muted, wrap: true);
            expires.Margin = new Thickness(0, 2, 0, 0); panel.Children.Add(expires);
        }
        return panel;
    }

    private void AddControls(StackPanel panel, DashboardAccount account, bool compact = false)
    {
        if (account.Provider == "codex" && Formatting.IsSafeProfile(account.Capabilities.CodexProfile))
        {
            var button = new Button { Content = compact ? account.IsActive ? "✓ Active" : "⇄" : account.IsActive ? "Active" : "Activate", IsEnabled = !account.IsActive && dashboard?.CodexAutoSwitch.ActivationInProgress != true, HorizontalAlignment = HorizontalAlignment.Right, Margin = new Thickness(0, compact ? 0 : 8, 0, 0), Background = account.IsActive ? ColorBrush("#143D58") : ColorBrush("#1979D2") };
            if (compact) { button.Width = account.IsActive ? 62 : 30; button.Height = 26; button.Padding = new Thickness(3); button.FontSize = account.IsActive ? 10 : 15; }
            button.ToolTip = account.IsActive ? "Active Codex account" : "Activate this Codex account";
            button.Click += async (_, _) => await ActivateAccount(account);
            panel.Children.Add(button);
        }
        else if (account.Provider == "claude" && Formatting.IsWindowsClaudeProfile(account.Capabilities.ClaudeProfileId))
        {
            var row = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right, Margin = new Thickness(0, compact ? 0 : 8, 0, 0) };
            var choices = account.Capabilities.ClaudePlatforms.Where(p => p is "windows" or "mac").Distinct().ToList();
            foreach (var platform in new[] { "mac", "windows" }.Where(choices.Contains))
            {
                var button = new Button { Content = PlatformGlyph(platform), Width = 30, Height = 26, Padding = new Thickness(5), Margin = new Thickness(5, 0, 0, 0), ToolTip = "Open this Claude account on " + (platform == "mac" ? "Mac" : "Windows") };
                button.Click += async (_, _) => await Action(async () =>
                {
                    var id = account.Capabilities.ClaudeProfileId!;
                    if (!Formatting.IsWindowsClaudeProfile(id)) throw new InvalidOperationException("Choose a configured Claude account.");
                    if (platform == "mac") { if (client is not null) await client.OpenClaudeOnMac(id); }
                    else Process.Start(new ProcessStartInfo("ccs-claude://launch/" + id) { UseShellExecute = true });
                }, "Claude profile launch requested.");
                row.Children.Add(button);
            }
            panel.Children.Add(row);
        }
    }

    private async Task ActivateAccount(DashboardAccount account)
    {
        if (busy || staleSample || client is null) return;
        busy = true; RefreshButton.IsEnabled = false; SetButtonsEnabled(ContentPanel, false);
        StatusText.Text = "Checking running Codex programs…";
        try
        {
            var switched = await CodexSwitchFlow.Run(account.Capabilities.CodexProfile!,
                async token => { await client.Activate(account.Capabilities.CodexProfile!, token); },
                confirmation =>
                {
                    confirmationVisible = true;
                    try
                    {
                        var dialog = new CodexSwitchDialog(confirmation, account.Email ?? account.Label) { Owner = this };
                        return Task.FromResult(dialog.ShowDialog() == true);
                    }
                    finally { confirmationVisible = false; }
                });
            if (!switched) { RenderDashboard(); StatusText.Text = "Switch cancelled. The Codex account was not changed."; return; }
            dashboard = await client.Dashboard(false); staleSample = false; RenderDashboard();
            StatusText.Text = "Codex account switched.";
        }
        catch (Exception error)
        {
            StatusText.Text = DisplayError(error);
            if (dashboard is not null) { RenderDashboard(); MarkStale(); }
        }
        finally { busy = false; RefreshButton.IsEnabled = true; }
    }

    private async Task Action(Func<Task> action, string success)
    {
        if (busy || client is null) return;
        busy = true; RefreshButton.IsEnabled = false; SetButtonsEnabled(ContentPanel, false);
        StatusText.Text = "Working…";
        try
        {
            await action();
            StatusText.Text = success;
            dashboard = await client.Dashboard(false);
            staleSample = false;
            RenderDashboard();
        }
        catch (Exception error)
        {
            StatusText.Text = DisplayError(error);
            if (dashboard is not null) { RenderDashboard(); MarkStale(); }
        }
        finally { busy = false; RefreshButton.IsEnabled = true; }
    }

    private void RenderConnection()
    {
        settingsVisible = true; ContentPanel.Children.Clear();
        StatusText.Text = "Dashboard connection"; StatusDot.Fill = ColorBrush("#69809A");
        ContentPanel.Children.Add(Section("DASHBOARD CONNECTION"));
        var panel = new StackPanel();
        panel.Children.Add(Text("CCS Bar uses your CCS dashboard login.", 12, wrap: true));
        var url = Field(panel, "Dashboard URL", connection?.BaseURL ?? "http://192.168.50.179:3000");
        var user = Field(panel, "Username", connection?.Username ?? "");
        panel.Children.Add(Text("Password", 11, Muted));
        var password = new PasswordBox { Margin = new Thickness(0, 4, 0, 10) };
        panel.Children.Add(password);
        if (connection is not null) panel.Children.Add(Text("Leave password blank to keep the saved password.", 10, Muted, wrap: true));
        panel.Children.Add(Text("Saved privately for your Windows user. Provider credentials stay on the CCS server.", 11, Muted, wrap: true));
        var buttons = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right, Margin = new Thickness(0, 14, 0, 0) };
        if (client is not null)
        {
            var cancel = new Button { Content = "Cancel", Margin = new Thickness(0, 0, 7, 0) };
            cancel.Click += (_, _) => { settingsVisible = false; RenderDashboard(); };
            buttons.Children.Add(cancel);
        }
        var save = new Button { Content = "Connect", Background = Accent };
        save.Click += async (_, _) =>
        {
            if (busy) return;
            try
            {
                var candidate = new ConnectionSettings { BaseURL = url.Text.Trim(), Username = user.Text.Trim(), Password = password.Password.Length > 0 ? password.Password : connection?.Password ?? "" };
                candidate.Validate();
                SecureStore.Save(candidate);
                connection = candidate; client?.Dispose(); client = new DashboardClient(candidate);
                password.Clear(); settingsVisible = false;
                await Refresh(true);
            }
            catch (Exception error) { StatusText.Text = DisplayError(error); }
        };
        buttons.Children.Add(save); panel.Children.Add(buttons); ContentPanel.Children.Add(CardBorder(panel));
    }

    private static TextBox Field(StackPanel panel, string label, string value)
    {
        var title = Text(label, 11, Muted); title.Margin = new Thickness(0, 12, 0, 4); panel.Children.Add(title);
        var input = new TextBox { Text = value }; panel.Children.Add(input); return input;
    }

    private void RenderEmpty(string title, string message)
    {
        ContentPanel.Children.Clear(); var panel = new StackPanel();
        panel.Children.Add(Text(title, 13, weight: FontWeights.SemiBold));
        panel.Children.Add(Text(message, 11, Muted, wrap: true));
        ContentPanel.Children.Add(CardBorder(panel));
    }

    private static Border CardBorder(UIElement child) => new() { Child = child, Background = Card, BorderBrush = ColorBrush("#20364E"), BorderThickness = new Thickness(1), CornerRadius = new CornerRadius(10), Padding = new Thickness(11), Margin = new Thickness(0, 0, 0, 8) };
    private static TextBlock Section(string title) { var text = Text(title, 10, Muted, FontWeights.SemiBold); text.Margin = new Thickness(0, 0, 0, 8); return text; }
    private static Border Badge(string title, string background, string foreground) => new() { CornerRadius = new CornerRadius(5), Background = ColorBrush(background), Child = Text(title, 9, ColorBrush(foreground), FontWeights.Bold), Padding = new Thickness(5, 2, 5, 2), Margin = new Thickness(5, 0, 0, 0), MaxWidth = 112 };
    private static TextBlock Text(string text, double size, Brush? foreground = null, FontWeight? weight = null, bool wrap = false) => new() { Text = text, FontSize = size, Foreground = foreground ?? Brushes.WhiteSmoke, FontWeight = weight ?? FontWeights.Normal, TextWrapping = wrap ? TextWrapping.Wrap : TextWrapping.NoWrap };
    private static Brush ColorBrush(string hex) { var brush = new SolidColorBrush((Color)ColorConverter.ConvertFromString(hex)); brush.Freeze(); return brush; }
    private static Brush Band(double used) => Accent;
    private static UIElement PlatformGlyph(string platform)
    {
        var data = platform == "windows" ? "M0,1 L7,0V7H0Z M8,0H15V7H8Z M0,8H7V15L0,14Z M8,8H15V16H8Z" : "M11,3 C9,3 8,4 7,4 C6,4 5,3 3,4 C0,5 0,8 1,11 C2,14 3,16 5,16 C6,16 7,15 8,15 C9,15 10,16 11,16 C13,16 14,13 15,11 C12,10 12,6 15,5 C14,3 12,3 11,3Z M8,3 C8,1 9,0 12,0 C12,2 10,3 8,3Z";
        return new System.Windows.Shapes.Path { Data = Geometry.Parse(data), Fill = ColorBrush("#B8D9FF"), Width = 15, Height = 16, Stretch = Stretch.Uniform };
    }
    private static string DisplayError(Exception error) => error is InvalidOperationException or ArgumentException ? error.Message : error is TaskCanceledException ? "The dashboard took too long to respond. Try Refresh." : "Could not reach the dashboard. Check Connection or try Refresh.";
    private static void SetButtonsEnabled(DependencyObject parent, bool enabled)
    {
        foreach (var child in LogicalTreeHelper.GetChildren(parent))
        {
            if (child is Button button && button.Tag as string != "read-only") button.IsEnabled = enabled;
            if (child is CheckBox checkbox) checkbox.IsEnabled = enabled;
            if (child is ComboBox picker) picker.IsEnabled = enabled;
            if (child is DependencyObject dependency) SetButtonsEnabled(dependency, enabled);
        }
    }

    private async void RefreshClicked(object sender, RoutedEventArgs e) { settingsVisible = false; await Refresh(true); }
    private void SettingsClicked(object sender, RoutedEventArgs e) { if (!busy) RenderConnection(); }
    private void DashboardClicked(object sender, RoutedEventArgs e) { if (client is not null) Process.Start(new ProcessStartInfo(client.BaseURL.ToString()) { UseShellExecute = true }); }
    private void QuitClicked(object sender, RoutedEventArgs e) => ((App)System.Windows.Application.Current).Quit();
    protected override void OnClosing(CancelEventArgs e)
    {
        if (!AllowClose) { e.Cancel = true; Hide(); }
        base.OnClosing(e);
    }
    protected override void OnClosed(EventArgs e) { timer.Stop(); client?.Dispose(); base.OnClosed(e); }
}
