using System;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Reflection;
using System.Threading;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Documents;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Animation;

namespace CCSBar;

/// <summary>Settings (in the panel, sliding over the list) and the sign-in screen.</summary>
public partial class MainWindow
{
    public bool SettingsOpen => settingsVisible;

    /// <summary>Settings slides in from the right (340 ms ease-out) while the list recedes; the gear stays pressed
    /// and turns 60 degrees (a control, so it may spring).</summary>
    public void OpenSettings()
    {
        if (settingsVisible) return;
        settingsVisible = true;
        SettingsButton.IsChecked = true;
        Motion.To(gearTurn, RotateTransform.AngleProperty, 60, 420, Motion.Spring);
        RenderSettings();
        // Over the sign-in screen, its form (and its Enter-to-connect default button) rests until Settings closes.
        SignInLayer.IsEnabled = false;
        SettingsLayer.Visibility = Visibility.Visible;
        var width = Math.Max(1, Body.ActualWidth > 0 ? Body.ActualWidth : Width);
        Motion.To(SettingsShift, TranslateTransform.XProperty, 0, 340, Motion.Out, from: width);
        Motion.To(ListShift, TranslateTransform.XProperty, -28, 340, Motion.Out);
        Motion.To(ListLayer, OpacityProperty, 0, 240, Motion.Out);
        // The sign-in screen recedes the same way under Settings.
        Motion.To(SignInShift, TranslateTransform.XProperty, -28, 340, Motion.Out);
        Motion.To(SignInLayer, OpacityProperty, 0, 240, Motion.Out);
        int i = 0;
        foreach (var card in SettingsPanel.Children.OfType<FrameworkElement>())
        {
            if (card.RenderTransform is not TranslateTransform shift) card.RenderTransform = shift = new TranslateTransform();
            card.Opacity = 0;
            Motion.To(card, OpacityProperty, 1, 260, delay: 60 + i * 26, from: 0);
            Motion.To(shift, TranslateTransform.XProperty, 0, 360, delay: 60 + i * 26, from: 14);
            i++;
        }
    }

    public void CloseSettings(bool animate = true)
    {
        if (!settingsVisible) return;
        settingsVisible = false;
        SettingsButton.IsChecked = false;
        SignInLayer.IsEnabled = true;
        Motion.To(gearTurn, RotateTransform.AngleProperty, 0, animate ? 420 : 0, Motion.Spring);
        var width = Math.Max(1, Body.ActualWidth);
        Motion.To(ListShift, TranslateTransform.XProperty, 0, animate ? 340 : 0, Motion.Out);
        Motion.To(ListLayer, OpacityProperty, 1, animate ? 240 : 0, Motion.Out);
        Motion.To(SignInShift, TranslateTransform.XProperty, 0, animate ? 340 : 0, Motion.Out);
        if (signInVisible) Motion.To(SignInLayer, OpacityProperty, 1, animate ? 240 : 0, Motion.Out);
        Motion.To(SettingsShift, TranslateTransform.XProperty, width, animate ? 220 : 0, Motion.In, completed: (_, _) =>
        {
            if (!settingsVisible) SettingsLayer.Visibility = Visibility.Collapsed;
        });
    }

    private void RenderSettings()
    {
        SettingsPanel.Children.Clear();
        // Header with the visible X.
        var head = new Grid { Margin = new Thickness(4, 2, 0, 10), MinHeight = 34 };
        head.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        head.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var title = Ui.Text("Settings", 15, "Ink", FontWeights.SemiBold); title.VerticalAlignment = VerticalAlignment.Center; head.Children.Add(title);
        var close = Ui.IconButton("x", "Close (Esc)", 28); close.Uid = "settings-close";
        close.Click += (_, _) => CloseSettings();
        Grid.SetColumn(close, 1); head.Children.Add(close);
        SettingsPanel.Children.Add(head);

        // Appearance: Light / Dark / Auto, live.
        var appearance = Segment(new[] { ("light", "Light", (string?)"sun"), ("dark", "Dark", "moon"), ("auto", "Auto", "monitor") }, preferences.Theme);
        appearance.Changed += value =>
        {
            preferences.Theme = value; preferences.Save();
            Theme.Apply(preferences.Mode, animate: true);
        };
        SettingsPanel.Children.Add(SettingCard(Row("Appearance", "Auto follows the Windows app mode.", appearance)));

        SettingsPanel.Children.Add(SettingCard(ConnectionCard()));

        // Start with Windows: the installer's logon task.
        var startup = StartupTask.Enabled();
        FrameworkElement startupControl;
        if (startup is bool startupOn)
        {
            var toggle = new ToggleSwitch(startupOn, "");
            toggle.Toggled += requested => { if (StartupTask.Set(requested)) toggle.SetOn(requested); };
            startupControl = toggle;
        }
        else startupControl = Ui.Text("Install with Install.ps1", 12, "Ink3");
        SettingsPanel.Children.Add(SettingCard(Row("Start with Windows", startup is null ? "The logon task is created by the installer." : "Opens in the notification area when you sign in.", startupControl)));

        // Keyboard shortcut: optional global hotkey.
        var hotkeyToggle = new ToggleSwitch(preferences.Hotkey, "");
        var hotkeySub = Ui.Text("", 12, "Ink3", wrap: true);
        void PaintHotkey(string? state)
        {
            hotkeySub.Inlines.Clear();
            hotkeySub.Inlines.Add(new Run(Hotkey.Display) { FontWeight = FontWeights.SemiBold, Foreground = Theme.Brush("Ink2") });
            if (state == "in-use") hotkeySub.Inlines.Add(new Run(" is in use by another app, so it is not set.") { Foreground = Theme.Brush("WarnText") });
            else if (state == "unavailable") hotkeySub.Inlines.Add(new Run(" could not be set in this Windows session.") { Foreground = Theme.Brush("WarnText") });
            else hotkeySub.Inlines.Add(new Run(" opens or hides this panel from anywhere. If AI Account Center is not running, open it from the Start menu or the desktop shortcut."));
        }
        PaintHotkey(HotkeyState?.Invoke());
        hotkeyToggle.Toggled += requested =>
        {
            preferences.Hotkey = requested; preferences.Save();
            var state = SetHotkey?.Invoke(requested);
            hotkeyToggle.SetOn(requested);
            PaintHotkey(state);
        };
        var shortcutText = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        shortcutText.Children.Add(Ui.Text("Keyboard shortcut", 13, "Ink", FontWeights.SemiBold));
        hotkeySub.Margin = new Thickness(0, 1, 0, 0); shortcutText.Children.Add(hotkeySub);
        SettingsPanel.Children.Add(SettingCard(Row(shortcutText, hotkeyToggle)));

        // From the dashboard (read only).
        var facts = new StackPanel();
        var factsTitle = new TextBlock { FontSize = 13, Foreground = Theme.Brush("Ink") };
        factsTitle.Inlines.Add(new Run("From the dashboard") { FontWeight = FontWeights.SemiBold }); factsTitle.Inlines.Add(new Run("  read only") { FontSize = 12, Foreground = Theme.Brush("Ink3") });
        facts.Children.Add(factsTitle);
        var kv = new StackPanel { Margin = new Thickness(0, 6, 0, 0) };
        if (dashboard is null) kv.Children.Add(Ui.Text("Nothing to show until this tray is connected.", 12, "Ink3"));
        else
        {
            var refresh = dashboard.Settings?.ValidatedInterval ?? 60;
            kv.Children.Add(Fact("Usage refresh", "Every " + (refresh < 120 ? refresh + " s" : Math.Round(refresh / 60.0) + " min"), true).Row);
            var codex = dashboard.CodexAutoSwitch;
            kv.Children.Add(Fact("Codex auto-switch", $"{(codex.Enabled ? "On" : "Off")} · switches at {Formatting.Percent(100 - codex.ThresholdPercent)} used · checks every {codex.PollIntervalSeconds} s").Row);
            var agAccounts = dashboard.Accounts.Where(account => account.Provider == "antigravity").ToArray();
            var ag = dashboard.AntigravityAutoSwitch;
            kv.Children.Add(Fact("Antigravity auto-switch", ag is null ? "Not reported by this server" : !AntigravitySwitchable(agAccounts) ? "Starts with a second signed-in account" : $"{(ag.Enabled ? "On" : "Off")} · switches at {Formatting.Percent(ag.ThresholdUsedPercent)} used").Row);
            // The trays follow only "Show in tray"; "Show on dashboard" is the dashboard's own switch.
            var hidden = dashboard.Hidden.Select(provider => Formatting.ProviderName(provider)).ToArray();
            var (hiddenRow, hiddenValue) = Fact("Hidden in the trays", (!dashboard.ReportsTrayVisibility ? "Not reported by this server" : hidden.Length > 0 ? string.Join(", ", hidden) : "None") + " · ");
            var link = new Hyperlink(new Run("Change in dashboard")) { Foreground = Theme.Brush("AccentText"), TextDecorations = null, Cursor = Cursors.Hand };
            link.Click += (_, _) => OpenDashboard();
            hiddenValue.Inlines.Add(link);
            kv.Children.Add(hiddenRow);
            if (client is not null) kv.Children.Add(Fact("Dashboard address", client.BaseURL.GetLeftPart(UriPartial.Authority)).Row);
        }
        facts.Children.Add(kv);
        SettingsPanel.Children.Add(SettingCard(facts));

        // About.
        var about = new Grid();
        about.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(44) });
        about.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        about.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var logo = Icons.Logo(32, Theme.Brush("Ink")); logo.HorizontalAlignment = HorizontalAlignment.Center; about.Children.Add(logo);
        var aboutWords = new StackPanel { Margin = new Thickness(12, 0, 12, 0), VerticalAlignment = VerticalAlignment.Center };
        aboutWords.Children.Add(Ui.Text("AI Account Center", 13, "Ink", FontWeights.SemiBold));
        var version = Assembly.GetExecutingAssembly().GetName().Version;
        aboutWords.Children.Add(Ui.Text($"Version {version?.ToString(3) ?? "unavailable"} · Windows tray · provider marks belong to their owners", 12, "Ink3", trim: true));
        Grid.SetColumn(aboutWords, 1); about.Children.Add(aboutWords);
        var aboutButtons = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        var notices = Ui.Button("Third-party notices", "GhostButton", Icons.Icon("fileText", 14, Theme.Brush("Ink3")));
        notices.Click += (_, _) =>
        {
            var file = Path.Combine(AppContext.BaseDirectory, "Resources", "Providers", "THIRD-PARTY-NOTICES.txt");
            if (File.Exists(file)) Launch(new ProcessStartInfo(file) { UseShellExecute = true });
        };
        aboutButtons.Children.Add(notices);
        var quit = Ui.Button("Quit", "GhostButton", Icons.Icon("power", 14, Theme.Brush("Ink3")));
        quit.Click += (_, _) => QuitRequested?.Invoke();
        aboutButtons.Children.Add(quit);
        Grid.SetColumn(aboutButtons, 2); about.Children.Add(aboutButtons);
        SettingsPanel.Children.Add(SettingCard(about));
    }

    /// <summary>
    /// Settings › Connection. Paired: "Paired as Windows tray, last synced …", the computer and the saved address, and
    /// "This connection: …" as the dashboard sees it (GET /api/auth/check, read when Settings opens), with Re-pair and
    /// Disconnect (an inline confirm). A version 1 tray says it still uses its saved password and offers Pair; an
    /// unpaired tray offers Pair.
    /// </summary>
    private FrameworkElement ConnectionCard()
    {
        var card = new StackPanel { Uid = "settings-connection" };
        card.Children.Add(Ui.Text("Connection", 13, "Ink", FontWeights.SemiBold));
        var device = new Grid { Margin = new Thickness(0, 9, 0, 0) };
        device.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(34) });
        device.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        device.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var paired = client is { Paired: true } && connection is { IsPaired: true };
        var deviceIcon = new Border { Width = 34, Height = 34, CornerRadius = new CornerRadius(8), BorderThickness = new Thickness(1), BorderBrush = Theme.Brush("Rule"), VerticalAlignment = VerticalAlignment.Top, Child = Icons.Icon(client is null ? "unplug" : "monitor", 18, Theme.Brush("Ink2")) };
        device.Children.Add(deviceIcon);
        var words = new StackPanel { Margin = new Thickness(12, 0, 12, 0), VerticalAlignment = VerticalAlignment.Center };
        var who = new TextBlock { FontSize = 13, Foreground = Theme.Brush("Ink"), TextTrimming = TextTrimming.CharacterEllipsis, Uid = "settings-connection-who" };
        var where = new TextBlock { FontSize = 12, Foreground = Theme.Brush("Ink3"), Margin = new Thickness(0, 1, 0, 0), TextTrimming = TextTrimming.CharacterEllipsis, Uid = "settings-connection-where" };
        words.Children.Add(who); words.Children.Add(where);
        var buttons = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        var confirm = new Border { Uid = "settings-disconnect-confirm" };
        string note;
        if (paired && client is not null && connection is not null)
        {
            who.Inlines.Add(new Run("Paired as ")); who.Inlines.Add(new Run("Windows tray") { FontWeight = FontWeights.SemiBold });
            who.Inlines.Add(new Run(dashboard is null ? (staleSample ? ", last refresh failed" : ", syncing") : ", last synced " + Formatting.Relative(dashboard.UpdatedAt)));
            where.Text = "On " + Environment.MachineName + " · " + client.BaseURL.GetLeftPart(UriPartial.Authority);
            var via = new TextBlock { FontSize = 12, Foreground = Theme.Brush("Ink3"), Margin = new Thickness(0, 1, 0, 0), TextTrimming = TextTrimming.CharacterEllipsis, Text = "This connection: checking", Uid = "settings-this-connection" };
            words.Children.Add(via);
            FillConnectionLine(via, client.BaseURL);
            // While a pair request's answer is being finished, neither may start: it could undo what that answer did.
            var repair = Ui.Button("Re-pair"); repair.Uid = "settings-repair"; repair.IsEnabled = !pairHeld;
            repair.Click += (_, _) => { if (pairHeld) return; CloseSettings(animate: false); ShowSignIn(SignInState.Password, repair: true); };
            var disconnect = Ui.Button("Disconnect"); disconnect.Foreground = Theme.Brush("CritText"); disconnect.Margin = new Thickness(8, 0, 0, 0); disconnect.Uid = "settings-disconnect"; disconnect.IsEnabled = !pairHeld;
            disconnect.Click += (_, _) => { if (!pairHeld) confirm.Child = DisconnectConfirm(confirm); };
            buttons.Children.Add(repair); buttons.Children.Add(disconnect);
            note = "This tray signs in with its own device key, not your password, so changing the dashboard password keeps it signed in. Revoking it in the dashboard signs it out.";
        }
        else if (client is not null && connection is not null)
        {
            who.Inlines.Add(new Run("Signed in as ")); who.Inlines.Add(new Run(connection.Username) { FontWeight = FontWeights.SemiBold }); who.Inlines.Add(new Run(" with a saved password"));
            where.Inlines.Add(new Run(dashboard is null ? "Connecting" : staleSample ? "Last refresh failed" : "Connected") { Foreground = Theme.Brush(staleSample ? "WarnText" : "GoodText"), FontWeight = FontWeights.SemiBold });
            where.Inlines.Add(new Run(" · " + client.BaseURL.GetLeftPart(UriPartial.Authority) + (dashboard is null ? "" : " · last synced " + Formatting.Relative(dashboard.UpdatedAt))));
            var pair = Ui.Button("Pair", "AtlasPrimaryButton"); pair.Uid = "settings-pair"; pair.IsEnabled = !pairHeld;
            // A version 1 tray pairs from here: the saved password keeps working until pairing finishes (Upgrade).
            pair.Click += (_, _) => { if (pairHeld) return; CloseSettings(animate: false); ShowSignIn(SignInState.Password, repair: true); };
            buttons.Children.Add(pair);
            note = "This tray still signs in with your saved dashboard password, kept with Windows data protection. Pair gives it its own device key and deletes the password.";
        }
        else
        {
            who.Inlines.Add(new Run(SignInStatus ?? "Not paired") { FontWeight = FontWeights.SemiBold });
            where.Text = "Pair with your dashboard username and password to see usage.";
            var pair = Ui.Button("Pair", "AtlasPrimaryButton"); pair.Uid = "settings-pair";
            pair.Click += (_, _) => { CloseSettings(); if (!signInVisible) ShowSignIn(SignInState.FirstRun); };
            buttons.Children.Add(pair);
            note = "The device key is kept with Windows data protection, readable only by your Windows account. Provider credentials stay on the AI Account Center server.";
        }
        Grid.SetColumn(words, 1); device.Children.Add(words);
        Grid.SetColumn(buttons, 2); device.Children.Add(buttons);
        card.Children.Add(device);
        card.Children.Add(confirm);
        card.Children.Add(Note("shield", note));
        return card;
    }

    /// <summary>"This connection: 192.168.50.31, trusted local network" (or "not trusted"), as the dashboard sees this
    /// computer right now, so the owner can confirm once that the home VPN counts.</summary>
    private async void FillConnectionLine(TextBlock line, Uri origin)
    {
        if (ConnectionLineForRender is { } given) { line.Text = ConnectionLine(given); return; }
        try
        {
            using var api = new AuthApi(origin, TimeSpan.FromSeconds(5));
            var check = await api.Check();
            line.Text = ConnectionLine(check);
        }
        catch { line.Text = "This connection: the dashboard did not answer"; }
    }

    /// <summary>Renders only: the dashboard's view of this computer, given instead of asked, so a render sends nothing.</summary>
    internal AuthCheck? ConnectionLineForRender { get; set; }

    internal static string ConnectionLine(AuthCheck check)
    {
        if (check.Connection is not { } seen) return "This connection: not reported by this dashboard";
        var peer = seen.Peer.Length is > 0 and <= 64 && seen.Peer.All(c => char.IsAsciiHexDigit(c) || c is ':' or '.') ? seen.Peer : "unknown";
        if (peer is "127.0.0.1" or "::1") return "This connection: this computer";
        if (seen.Trusted) return $"This connection: {peer}, trusted local network";
        return check.SecureTransport ? $"This connection: {peer}, secure connection" : $"This connection: {peer}, not trusted";
    }

    /// <summary>Disconnect asks inline first (the concept's confirm line), then revokes and forgets the key.</summary>
    private FrameworkElement DisconnectConfirm(Border slot)
    {
        var line = new Grid { Uid = "settings-disconnect-line" };
        line.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(23) });
        line.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        line.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        // The gap sits outside the box, so the confirm keeps clear of the "This connection" line above it.
        var box = new Border { Background = Theme.Brush("CritSoft"), BorderBrush = Theme.Brush("CritLine"), BorderThickness = new Thickness(1), CornerRadius = new CornerRadius(8), Padding = new Thickness(10, 9, 10, 9), Margin = new Thickness(0, 10, 0, 0), Child = line };
        var glyph = Icons.Icon("unplug", 15, Theme.Brush("CritText")); glyph.VerticalAlignment = VerticalAlignment.Center; glyph.HorizontalAlignment = HorizontalAlignment.Left;
        line.Children.Add(glyph);
        var text = Ui.Text("Disconnect this Windows tray? The dashboard revokes its device key and the tray forgets it. Pairing again needs the dashboard password.", 12, "Ink2", wrap: true);
        text.VerticalAlignment = VerticalAlignment.Center; text.Uid = "settings-disconnect-text";
        Grid.SetColumn(text, 1); line.Children.Add(text);
        var actions = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(10, 0, 0, 0) };
        var cancel = Ui.Button("Cancel", "GhostButton"); cancel.Uid = "settings-disconnect-cancel";
        cancel.Click += (_, _) => slot.Child = null;
        var yes = Ui.Button("Disconnect", "AtlasPrimaryButton"); yes.Margin = new Thickness(6, 0, 0, 0); yes.Uid = "settings-disconnect-yes";
        yes.Background = Theme.Brush("Crit"); yes.BorderBrush = Theme.Brush("Crit"); yes.Tag = Theme.Brush("CritText"); yes.Foreground = Theme.Brush("AccentInk");
        yes.Click += async (_, _) =>
        {
            yes.IsEnabled = cancel.IsEnabled = false;
            text.Text = "Disconnecting";
            var failure = await DisconnectTray();
            if (failure is null) return;
            text.Text = failure; text.Foreground = Theme.Brush("CritText");
            yes.IsEnabled = cancel.IsEnabled = true;
        };
        actions.Children.Add(cancel); actions.Children.Add(yes);
        Grid.SetColumn(actions, 2); line.Children.Add(actions);
        return box;
    }

    /// <summary>Checks only: the Settings Connection card's buttons and lines, by Uid.</summary>
    internal FrameworkElement? SettingsElement(string uid) => FixtureRender.FindUid(SettingsPanel, uid);

    private static Segmented Segment((string, string, string?)[] options, string value) => new(options, value);

    private static Border SettingCard(UIElement child) => new()
    {
        Child = child, Background = Theme.Brush("Card"), BorderBrush = Theme.Brush("Rule"), BorderThickness = new Thickness(1),
        CornerRadius = new CornerRadius(8), Padding = new Thickness(14, 13, 14, 13), Margin = new Thickness(0, 0, 0, 10),
    };

    private static Grid Row(string title, string subtitle, FrameworkElement control)
    {
        var words = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        words.Children.Add(Ui.Text(title, 13, "Ink", FontWeights.SemiBold));
        var sub = Ui.Text(subtitle, 12, "Ink3", wrap: true); sub.Margin = new Thickness(0, 1, 0, 0); words.Children.Add(sub);
        return Row(words, control);
    }

    private static Grid Row(FrameworkElement words, FrameworkElement control)
    {
        var grid = new Grid { MinHeight = 30 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.Children.Add(words);
        control.VerticalAlignment = VerticalAlignment.Center; control.Margin = new Thickness(12, 0, 0, 0);
        Grid.SetColumn(control, 1); grid.Children.Add(control);
        return grid;
    }

    private static (Grid Row, TextBlock Value) Fact(string key, string value, bool first = false)
    {
        var grid = new Grid();
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(170) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        if (!first) { var divider = new Border { Height = 1, VerticalAlignment = VerticalAlignment.Top, Background = Theme.Brush("Rule2"), IsHitTestVisible = false }; Grid.SetColumnSpan(divider, 2); grid.Children.Add(divider); }
        var margin = new Thickness(0, first ? 2 : 6, 0, 6);
        var name = Ui.Text(key, 12.5, "Ink3"); name.Margin = margin; grid.Children.Add(name);
        var text = Ui.Text(value, 12.5, "Ink", wrap: true); text.Margin = margin; Grid.SetColumn(text, 1); grid.Children.Add(text);
        return (grid, text);
    }

    private static FrameworkElement Note(string icon, string text)
    {
        var row = new Grid { Margin = new Thickness(0, 10, 0, 0) };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(22) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        var glyph = Icons.Icon(icon, 14, Theme.Brush("Ink3")); glyph.VerticalAlignment = VerticalAlignment.Top; glyph.Margin = new Thickness(0, 2, 0, 0); glyph.HorizontalAlignment = HorizontalAlignment.Left;
        row.Children.Add(glyph);
        var words = Ui.Text(text, 12, "Ink3", wrap: true); Grid.SetColumn(words, 1); row.Children.Add(words);
        return row;
    }

    // ------------------------------------------------------------------ the version 1 password sign-in (verify, then save)

    /// <summary>Where a verified connection is saved: the DPAPI store (<see cref="SecureStore.SettingsPath"/>), or a
    /// fixture file in an isolated folder for the checks.</summary>
    internal string ConnectionStorePath { get; set; } = SecureStore.SettingsPath;
    /// <summary>How long a connection check may take before it counts as a failure.</summary>
    internal TimeSpan ConnectionCheckTimeout { get; set; } = TimeSpan.FromSeconds(15);
    internal bool CheckingConnection => connectionCheck is not null;
    internal DashboardClient? ClientForCheck => client;
    internal ConnectionSettings? ConnectionForCheck => connection;

    /// <summary>Cancel, or Escape: stops a running connection check. Nothing is saved and the running client stays.</summary>
    internal void CancelConnectionCheck() => connectionCheck?.Cancel();

    /// <summary>
    /// Sign-in and Change: verify, then save. A temporary client built from the entered address and login signs in
    /// and reads the dashboard (<see cref="DashboardClient.Verify"/>). Only when that works is the connection written
    /// through the existing DPAPI writer (same path, entropy and JSON) and the live client replaced by the verified
    /// one. A wrong address, a rejected login, a timeout or Cancel leaves the stored file and the running client
    /// exactly as they were. Returns null on success, or the message to show under the form.
    /// </summary>
    internal async Task<string?> SubmitConnection(string address, string username, string password)
    {
        if (connectionCheck is not null) return "A connection check is already running.";
        var unchanged = connection is null ? "Nothing was saved." : "The saved connection was not changed.";
        ConnectionSettings candidate;
        try
        {
            // A blank password keeps the saved one, but only for the same dashboard: it is never sent to a new address
            // (DEPLOY-REVIEW-1 F4). It is checked again with the address and username.
            var keep = password.Length == 0 && connection?.Password is { Length: > 0 } saved && ConnectionSettings.SameOrigin(address, connection.BaseURL) ? saved : null;
            if (password.Length == 0 && keep is null) return "Enter the password for this dashboard. " + unchanged;
            candidate = new ConnectionSettings { BaseURL = address.Trim(), Username = username.Trim(), Password = password.Length > 0 ? password : keep, Extra = connection?.Extra };
            candidate.Validate();
        }
        catch (ArgumentException invalid) { return invalid.Message; }
        using var check = new CancellationTokenSource();
        connectionCheck = check;
        DashboardClient? verified = new DashboardClient(candidate);
        try
        {
            await verified.Verify(ConnectionCheckTimeout, check.Token);
            check.Token.ThrowIfCancellationRequested();
            try { SecureStore.Save(candidate, ConnectionStorePath); }
            catch (Exception) { return "Windows could not save the connection. " + unchanged; }
            var previous = client;
            connection = candidate; client = verified; verified = null;
            connectionGeneration++;
            dashboard = null; staleSample = false; lastFailure = DateTimeOffset.MinValue; statusFlash = null;
            // The replaced connection's Open stops reading it and its row rests; it is never resumed.
            openProgress.Clear();
            previous?.Dispose();
            return null;
        }
        catch (ConnectionCheckException failure) { return failure.Message + " " + unchanged; }
        catch (OperationCanceledException) { return "Connection check cancelled. " + unchanged; }
        catch (Exception) { return "The connection could not be checked. " + unchanged; }
        finally
        {
            verified?.Dispose();
            connectionCheck = null;
        }
    }

}
