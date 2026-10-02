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

        // Connection: today's password login (device pairing waits for the dashboard), stored with DPAPI.
        var connectionCard = new StackPanel();
        connectionCard.Children.Add(Ui.Text("Connection", 13, "Ink", FontWeights.SemiBold));
        var device = new Grid { Margin = new Thickness(0, 9, 0, 0) };
        device.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(34) });
        device.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        device.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var deviceIcon = new Border { Width = 34, Height = 34, CornerRadius = new CornerRadius(8), BorderThickness = new Thickness(1), BorderBrush = Theme.Brush("Rule"), Child = Icons.Icon("monitor", 18, Theme.Brush("Ink2")) };
        device.Children.Add(deviceIcon);
        var words = new StackPanel { Margin = new Thickness(12, 0, 12, 0), VerticalAlignment = VerticalAlignment.Center };
        var who = new TextBlock { FontSize = 13, Foreground = Theme.Brush("Ink"), TextTrimming = TextTrimming.CharacterEllipsis };
        var where = new TextBlock { FontSize = 12, Foreground = Theme.Brush("Ink3"), Margin = new Thickness(0, 1, 0, 0), TextTrimming = TextTrimming.CharacterEllipsis };
        if (client is not null && connection is not null)
        {
            who.Inlines.Add(new Run("Signed in as ")); who.Inlines.Add(new Run(connection.Username) { FontWeight = FontWeights.SemiBold });
            where.Inlines.Add(new Run(dashboard is null ? "Connecting" : staleSample ? "Last refresh failed" : "Connected") { Foreground = Theme.Brush(staleSample ? "WarnText" : "GoodText"), FontWeight = FontWeights.SemiBold });
            where.Inlines.Add(new Run(" · " + client.BaseURL.GetLeftPart(UriPartial.Authority) + (dashboard is null ? "" : " · last synced " + Formatting.Relative(dashboard.UpdatedAt))));
        }
        else { who.Inlines.Add(new Run("Not connected") { FontWeight = FontWeights.SemiBold }); where.Text = "Sign in with your dashboard username and password to see usage."; }
        words.Children.Add(who); words.Children.Add(where);
        Grid.SetColumn(words, 1); device.Children.Add(words);
        var change = Ui.Button(client is null ? "Sign in" : "Change", client is null ? "AtlasPrimaryButton" : "AtlasButton");
        change.VerticalAlignment = VerticalAlignment.Center;
        change.Click += (_, _) => { CloseSettings(animate: false); ShowSignIn(firstRun: client is null); };
        Grid.SetColumn(change, 2); device.Children.Add(change);
        connectionCard.Children.Add(device);
        connectionCard.Children.Add(Note("shield", "Your dashboard sign-in is stored with Windows data protection for your user. After a dashboard password change, sign in here again. Provider credentials stay on the AI Account Center server."));
        SettingsPanel.Children.Add(SettingCard(connectionCard));

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
            var hidden = dashboard.Hidden.Select(provider => Formatting.ProviderName(provider)).ToArray();
            var (hiddenRow, hiddenValue) = Fact("Hidden on the dashboard", (!dashboard.ReportsHidden ? "Not reported by this server" : hidden.Length > 0 ? string.Join(", ", hidden) : "None") + " · ");
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
            if (File.Exists(file)) Process.Start(new ProcessStartInfo(file) { UseShellExecute = true });
        };
        aboutButtons.Children.Add(notices);
        var quit = Ui.Button("Quit", "GhostButton", Icons.Icon("power", 14, Theme.Brush("Ink3")));
        quit.Click += (_, _) => QuitRequested?.Invoke();
        aboutButtons.Children.Add(quit);
        Grid.SetColumn(aboutButtons, 2); about.Children.Add(aboutButtons);
        SettingsPanel.Children.Add(SettingCard(about));
    }

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

    // ------------------------------------------------------------------ sign-in

    /// <summary>First run (or Change): the dashboard address, username and password. Connect checks them against
    /// the dashboard first and saves them with DPAPI only once they work (<see cref="SubmitConnection"/>).</summary>
    public void ShowSignIn(bool firstRun)
    {
        // A Change opened again over a running check (Settings slides over the sign-in screen) starts over.
        CancelConnectionCheck();
        signInVisible = true;
        SignInPanel.Children.Clear();
        var top = new StackPanel { HorizontalAlignment = HorizontalAlignment.Center, Margin = new Thickness(0, 0, 0, 14) };
        var logo = Icons.Logo(44, Theme.Brush("Ink")); logo.HorizontalAlignment = HorizontalAlignment.Center; top.Children.Add(logo);
        var title = Ui.Text(firstRun ? "Connect this Windows PC to AI Account Center" : "Change the dashboard connection", 18, "Ink", FontWeights.SemiBold);
        title.HorizontalAlignment = HorizontalAlignment.Center; title.Margin = new Thickness(0, 12, 0, 0); top.Children.Add(title);
        var intro = Ui.Text("Sign in with your dashboard username and password.", 12.5, "Ink3", wrap: true);
        intro.HorizontalAlignment = HorizontalAlignment.Center; intro.TextAlignment = TextAlignment.Center; intro.Margin = new Thickness(0, 6, 0, 0); top.Children.Add(intro);
        SignInPanel.Children.Add(top);
        var url = Field("Dashboard address", new TextBox { Text = connection?.BaseURL ?? new ConnectionSettings().BaseURL });
        var user = Field("Username", new TextBox { Text = connection?.Username ?? "" });
        var password = Field("Password", new PasswordBox());
        if (connection is not null) password.Hint.Text = "Leave blank to keep the saved password.";
        var http = Note("info", "This address uses plain HTTP on your network, so the password crosses it unencrypted when the tray signs in. Use HTTPS or an SSH tunnel where you can.");
        http.Uid = "http-connection-warning";
        void UpdateHttp() => http.Visibility = Uri.TryCreate(((TextBox)url.Input).Text.Trim(), UriKind.Absolute, out var origin) && origin.Scheme == Uri.UriSchemeHttp ? Visibility.Visible : Visibility.Collapsed;
        ((TextBox)url.Input).TextChanged += (_, _) => UpdateHttp(); UpdateHttp();
        SignInPanel.Children.Add(url.Element); SignInPanel.Children.Add(user.Element); SignInPanel.Children.Add(password.Element);
        SignInPanel.Children.Add(http);
        SignInPanel.Children.Add(Note("shield", "Saved with Windows data protection for your user once the dashboard accepts it. Provider credentials stay on the AI Account Center server."));
        var error = Ui.Text("", 12, "CritText", wrap: true); error.Margin = new Thickness(0, 8, 0, 0); error.MinHeight = 15; error.Uid = "sign-in-error";
        SignInPanel.Children.Add(error);
        var actions = new Grid { Margin = new Thickness(0, 10, 0, 0) };
        actions.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        actions.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(8) });
        actions.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1.6, GridUnitType.Star) });
        var connectLabel = firstRun ? "Connect" : "Save and connect";
        var connect = Ui.Button(connectLabel, "AtlasPrimaryButton"); connect.Height = 34; connect.IsDefault = true; connect.Uid = "sign-in-connect";
        // Cancel stops a running check (the saved connection stays as it was); otherwise it closes a Change.
        // On first run there is nothing to go back to, so it shows only while a check runs.
        var cancel = Ui.Button("Cancel"); cancel.Height = 34; cancel.Uid = "sign-in-cancel";
        cancel.Click += (_, _) => { if (CheckingConnection) CancelConnectionCheck(); else if (client is not null) CloseSignIn(); };
        actions.Children.Add(cancel);
        actions.Children.Add(connect);
        void Layout(bool checking)
        {
            var showCancel = client is not null || checking;
            cancel.Visibility = showCancel ? Visibility.Visible : Visibility.Collapsed;
            Grid.SetColumn(connect, showCancel ? 2 : 0); Grid.SetColumnSpan(connect, showCancel ? 1 : 3);
            connect.Content = checking ? "Checking the connection" : connectLabel;
            connect.IsEnabled = !checking;
            url.Input.IsEnabled = user.Input.IsEnabled = password.Input.IsEnabled = !checking;
        }
        Layout(false);
        SignInPanel.Children.Add(actions);
        connect.Click += async (_, _) =>
        {
            if (CheckingConnection) return;
            error.Text = "";
            Layout(true);
            var failure = await SubmitConnection(((TextBox)url.Input).Text, ((TextBox)user.Input).Text, ((PasswordBox)password.Input).Password);
            Layout(false);
            if (failure is not null) { error.Text = failure; return; }
            ((PasswordBox)password.Input).Clear();
            CloseSignIn();
            RenderFooter(); UpdateStatus(); SampleChanged?.Invoke();
            await Refresh(true);
        };
        SignInLayer.Visibility = Visibility.Visible;
        SignInLayer.Opacity = 0; Motion.To(SignInLayer, OpacityProperty, 1, 260);
        RenderFooter(); UpdateStatus();
        Dispatcher.BeginInvoke(new Action(() => (string.IsNullOrEmpty(((TextBox)url.Input).Text) ? url.Input : string.IsNullOrEmpty(((TextBox)user.Input).Text) ? user.Input : password.Input).Focus()), System.Windows.Threading.DispatcherPriority.Input);
    }

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
            // A blank password keeps the saved one: it is checked again with the new address and username.
            candidate = new ConnectionSettings { BaseURL = address.Trim(), Username = username.Trim(), Password = password.Length > 0 ? password : connection?.Password ?? "", Extra = connection?.Extra };
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

    private void CloseSignIn()
    {
        signInVisible = false;
        Motion.To(SignInLayer, OpacityProperty, 0, 200, completed: (_, _) => { if (!signInVisible) SignInLayer.Visibility = Visibility.Collapsed; });
    }

    private sealed record FieldParts(FrameworkElement Element, Control Input, TextBlock Hint);

    private static FieldParts Field(string label, Control input)
    {
        var panel = new StackPanel { Margin = new Thickness(0, 0, 0, 10) };
        var caption = Ui.Text(label, 12, "Ink2", FontWeights.Medium); caption.Margin = new Thickness(0, 0, 0, 5); panel.Children.Add(caption);
        panel.Children.Add(input);
        var hint = Ui.Text("", 11.5, "Ink3"); hint.Margin = new Thickness(0, 3, 0, 0); panel.Children.Add(hint);
        return new FieldParts(panel, input, hint);
    }
}
