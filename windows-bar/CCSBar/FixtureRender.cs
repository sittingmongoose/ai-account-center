using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Text.Json;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace CCSBar;

/// <summary>
/// Offline render checks (--render-fixture DIR): the real panel drawn from the bundled sanitized fixture
/// (example.com identities, captured 2026-10-01) in Light and Dark, with Settings, an expanded row and a switch.
/// Measures the selected-row alignment rule: every check and "Active" must land on the Activate button's left edge
/// and label x within 0.5 px, before and after a switch. Never connects anywhere and never changes an account.
/// </summary>
public static class FixtureRender
{
    private sealed class FixtureFile { public string CapturedAt { get; set; } = ""; public AccountDashboard Dashboard { get; set; } = new(); }

    public static AccountDashboard LoadFixture(out DateTimeOffset capturedAt)
    {
        using var stream = typeof(FixtureRender).Assembly.GetManifestResourceStream("CCSBar.Fixtures.dashboard.json") ?? throw new InvalidOperationException("Fixture missing.");
        var file = JsonSerializer.Deserialize<FixtureFile>(stream, Formatting.Json) ?? throw new InvalidOperationException("Fixture unreadable.");
        capturedAt = DateTimeOffset.Parse(file.CapturedAt, CultureInfo.InvariantCulture);
        return file.Dashboard;
    }

    private static AccountDashboard Clone(AccountDashboard dashboard) =>
        JsonSerializer.Deserialize<AccountDashboard>(JsonSerializer.Serialize(dashboard, Formatting.Json), Formatting.Json)!;

    /// <summary>One theme per process: RenderTargetBitmap keeps a brush's first colour on its own channel, so a
    /// Light and a Dark capture in one process would not show the retinted brushes (the live window does).</summary>
    public static async Task<CheckReport> Run(App app, string directory, string? only = null)
    {
        var report = new CheckReport();
        var measures = new Dictionary<string, double>();
        Directory.CreateDirectory(directory);
        Motion.Enabled = false;
        var fixture = LoadFixture(out var capturedAt);
        Formatting.Now = () => capturedAt.AddSeconds(5);
        foreach (var mode in new[] { ThemeMode.Light, ThemeMode.Dark }.Where(mode => only is null || mode.ToString().Equals(only, StringComparison.OrdinalIgnoreCase)))
        {
            var name = mode == ThemeMode.Light ? "light" : "dark";
            Theme.Apply(mode, animate: false);
            var window = new MainWindow(new Preferences { Theme = name, Hotkey = false }, loadConnection: false) { ShowActivated = false, Left = 40, Top = 40, Width = 760, Height = 850 };
            window.UseFixtureConnection();
            window.Show();
            measures[name + "_theme_is_dark"] = Theme.IsDark ? 1 : 0;
            window.ApplyDashboardSample(Clone(fixture));
            await Settle(window);
            SavePng(window, Path.Combine(directory, $"panel-{name}.png"));
            report.Checks[$"{name}_codex_active_aligned_before_switch"] = Aligned(window, "codex", measures, name + "_before");
            report.Checks[$"{name}_antigravity_slot_on_codex_slot_line"] = SlotLine(window, measures, name);
            report.Checks[$"{name}_platter_on_active_row"] = PlatterOn(window, "codex", "row:codex:example-2", measures, name + "_before");
            report.Checks[$"{name}_fable_on_max_only"] = Fable(window);
            report.Checks[$"{name}_provider_order_today"] = Order(window, new[] { "section:claude", "section:codex", "section:antigravity", "provider:cursor", "provider:muse", "provider:kimi-code", "provider:qwen", "provider:zai", "provider:opencode-go" });
            report.Checks[$"{name}_tabular_instrument_sans_resolves"] = FontResolves();
            report.Checks[$"{name}_section_columns_and_names_aligned"] = Columns(window, measures, name);
            var codexSection = FindUid(window.ContentPanel, "section:codex");
            report.Checks[$"{name}_codex_header_has_no_active_meta"] = codexSection is not null && FindUid(codexSection, "section-meta") is null;
            // The Codex auto-switch moved from the footer into the Codex section header: its toggle and
            // threshold sit above the first Codex account and inside the Codex section, and the footer
            // keeps no auto-switch control.
            var codexTools = codexSection is null ? null : FindUid(codexSection, "mutation:toggle");
            var codexDrop = codexSection is null ? null : FindUid(codexSection, "mutation:drop");
            var codexFirstRow = FindUid(window.ContentPanel, "row:codex:example-1");
            report.Checks[$"{name}_codex_auto_in_section_header"] = codexSection is not null && codexTools is ToggleSwitch && codexDrop is Button && codexFirstRow is not null
                && Y(codexTools, window) + codexTools.ActualHeight <= Y(codexFirstRow, window) + 0.5
                && Y(codexTools, window) >= Y(codexSection, window) - 0.5
                && Y(codexDrop, window) + codexDrop.ActualHeight <= Y(codexFirstRow, window) + 0.5;
            report.Checks[$"{name}_footer_has_no_auto_switch"] = window.AutoSwitchPanel.Content is null
                && FindUid(window.AutoSwitchPanel, "mutation:toggle") is null
                && FindUid(window.AutoSwitchPanel, "mutation:drop") is null;

            // Switch: codex-3 becomes active; the platter moves to it and the alignment still holds.
            var switched = Clone(fixture);
            foreach (var account in switched.Accounts.Where(account => account.Provider == "codex")) account.IsActive = account.Id == "codex:example-3";
            window.ApplyDashboardSample(switched);
            await Settle(window);
            SavePng(window, Path.Combine(directory, $"panel-{name}-switched.png"));
            report.Checks[$"{name}_codex_active_aligned_after_switch"] = Aligned(window, "codex", measures, name + "_after");
            report.Checks[$"{name}_platter_follows_switch"] = PlatterOn(window, "codex", "row:codex:example-3", measures, name + "_after");

            // Two signed-in Antigravity accounts, the second one active: Antigravity uses the same slot and indicator.
            var twoAg = Clone(fixture);
            var first = twoAg.Accounts.First(account => account.Provider == "antigravity");
            first.Capabilities.AntigravityProfileId = "example-1"; first.Capabilities.AntigravityCanActivate = true; first.Capabilities.AntigravityHostIds = new() { "ubuntu" };
            var second = Clone(new AccountDashboard { Accounts = { first } }).Accounts[0];
            second.Id = "antigravity:example-2"; second.Email = "antigravity-2@example.com"; second.Label = "Antigravity account 2"; second.IsActive = true; second.Capabilities.AntigravityProfileId = "example-2";
            // A same-day reset beside a two-decimal value: the tightest compact cell (live data showed "5:1...").
            second.Windows.First(window => window.Key == "gemini-weekly").ResetAt = capturedAt.AddHours(5).AddMinutes(15).ToString("O");
            twoAg.Accounts.Insert(twoAg.Accounts.IndexOf(first) + 1, second);
            twoAg.AntigravityAutoSwitch = new AntigravityAutoStatus { Enabled = true, ThresholdUsedPercent = 95, PollIntervalSeconds = 60 };
            window.ApplyDashboardSample(twoAg);
            await Settle(window);
            SavePng(window, Path.Combine(directory, $"panel-{name}-antigravity-two.png"));
            report.Checks[$"{name}_antigravity_active_aligned"] = Aligned(window, "antigravity", measures, name + "_ag") && SameLine(measures, name + "_ag_button", name + "_before_button");
            report.Checks[$"{name}_antigravity_platter_on_active_row"] = PlatterOn(window, "antigravity", "row:antigravity:example-2", measures, name + "_ag");
            report.Checks[$"{name}_antigravity_two_columns_and_names_aligned"] = Columns(window, measures, name + "_ag2");
            var agTwoSection = FindUid(window.ContentPanel, "section:antigravity");
            report.Checks[$"{name}_antigravity_header_keeps_active_meta"] = agTwoSection is not null && FindUid(agTwoSection, "section-meta") is not null;
            var codexToggleTwo = FindUid(window.ContentPanel, "section:codex") is FrameworkElement codexTwo ? FindUid(codexTwo, "mutation:toggle") : null;
            var agToggleTwo = agTwoSection is null ? null : FindUid(agTwoSection, "mutation:toggle");
            report.Checks[$"{name}_codex_auto_matches_antigravity_tools"] = codexToggleTwo is ToggleSwitch codexSwitch && agToggleTwo is ToggleSwitch agSwitch
                && Math.Abs(codexSwitch.ActualHeight - agSwitch.ActualHeight) <= 0.5;
            var compact = All(window.ContentPanel).OfType<Meter>().Where(meter => meter.Kind == MeterKind.Compact && meter.IsVisible).ToArray();
            var tight = compact.FirstOrDefault(meter => meter.Key == "antigravity:example-2|gemini-weekly");
            report.Notes[$"{name}_tight_reset_shown"] = tight?.ResetShown ?? "missing";
            report.Checks[$"{name}_compact_resets_shown_whole"] = tight is not null && compact.All(meter => !meter.ResetClipped);
            // The active one of the two hidden in the tray: the other still offers Activate and the Auto-switch toggle
            // stays live, because "Show in tray" never changes what the user can operate.
            var agHidden = Clone(twoAg);
            agHidden.Accounts.First(account => account.Id == "antigravity:example-2").TrayHidden = true;
            window.ApplyDashboardSample(agHidden);
            await Settle(window);
            var agRow = FindUid(window.ContentPanel, "row:antigravity:example-1");
            var agSection = FindUid(window.ContentPanel, "section:antigravity");
            report.Checks[$"{name}_antigravity_switching_survives_tray_hiding"] = FindUid(window.ContentPanel, "row:antigravity:example-2") is null
                && agRow is not null && FindUid(agRow, "mutation:activate") is not null && agSection is not null && FindUid(agSection, "mutation:toggle") is not null;

            // Antigravity plan display (ANTIGRAVITY-SPEC): the fixture's three plans on the four columns — the Pro
            // account shows all four meters, the Free account's missing 5-hour cells read "Weekly only" and "Not on
            // plan", and the Workspace row says it has no quota. Then the expanded details: pools ordered Gemini
            // 5-hour first, whole pool titles, the AI credits (overage) row with its hint.
            window.ApplyDashboardSample(Clone(fixture));
            await Settle(window);
            SavePng(window, Path.Combine(directory, $"panel-{name}-antigravity-plans.png"));
            var planSection = FindUid(window.ContentPanel, "section:antigravity");
            report.Checks[$"{name}_antigravity_four_columns_and_plans"] = planSection is not null
                && FindUid(planSection, "row:antigravity:example-3") is not null && FindUid(planSection, "row:antigravity:example-4") is not null;
            var planMeters = All(window.ContentPanel).OfType<Meter>().Where(meter => meter.IsVisible).ToArray();
            var weeklyOnly = planMeters.FirstOrDefault(meter => meter.Key == "antigravity:example-3|gemini-5h");
            var notOnPlan = planMeters.FirstOrDefault(meter => meter.Key == "antigravity:example-3|3p-5h");
            report.Checks[$"{name}_antigravity_muted_cells_are_honest"] = weeklyOnly is { DrawnUnavailable: true } && weeklyOnly.ValueShown == "Weekly only"
                && notOnPlan is { DrawnUnavailable: true } && notOnPlan.ValueShown == "Not on plan"
                && planMeters.Any(meter => meter.Key == "antigravity:example-3|gemini-weekly" && meter.Target is 22.5);
            report.Checks[$"{name}_antigravity_no_quota_row_line"] = planSection is not null
                && All(planSection).OfType<TextBlock>().Any(text => text.Text == "No Antigravity quota on this plan");
            window.ToggleDetailsForCheck("antigravity:example-1");
            await Settle(window);
            var planDetails = FindUid(window.ContentPanel, "details");
            var detailMeters = All(window.ContentPanel).OfType<Meter>().Where(meter => meter.Kind == MeterKind.Detail && meter.IsVisible).Select(meter => meter.Key).ToArray();
            report.Checks[$"{name}_antigravity_details_pool_order"] = detailMeters.SequenceEqual(new[]
            {
                "detail:antigravity:example-1|gemini-5h", "detail:antigravity:example-1|gemini-weekly", "detail:antigravity:example-1|3p-5h", "detail:antigravity:example-1|3p-weekly",
            });
            var planBlocks = All(window.ContentPanel).OfType<TextBlock>().Select(text => text.Text).ToArray();
            report.Checks[$"{name}_antigravity_details_plan_notes"] = planBlocks.Contains("Refreshes every 5 hours up to a weekly limit.")
                && planBlocks.Any(text => text.StartsWith("Models: Gemini 3.8 Flash", StringComparison.Ordinal))
                && planBlocks.Contains("Family members sharing this plan may share one quota pool.")
                && planBlocks.Contains("AI credits (overage)")
                && planBlocks.Contains("Used only after the plan quota runs out, when AI Credit Overages is on.");
            report.Checks[$"{name}_antigravity_details_not_truncated"] = planDetails is not null && NoClippedText(planDetails);
            SavePng(window, Path.Combine(directory, $"panel-{name}-antigravity-details.png"));
            window.ToggleDetailsForCheck("antigravity:example-1");

            // Hidden providers are honoured; an unknown provider still renders, with the neutral mark.
            var hidden = Clone(fixture);
            // "Show in tray" off for Kimi Code hides it; "Show on dashboard" off for Muse does not; an account hidden in the
            // tray is left out.
            hidden.Settings ??= new AccountRefreshSettings();
            hidden.Settings.TrayHiddenProviders = new() { "kimi-code" };
            hidden.Providers = new() { new DashboardProvider { Id = "muse", Visible = false, TrayVisible = true } };
            hidden.Accounts.First(account => account.Id == "claude:example-4").TrayHidden = true;
            hidden.Accounts.Add(new DashboardAccount { Id = "newcode:example-1", Provider = "newcode", ProviderLabel = "New Code", Label = "New Code account", Email = "newcode-1@example.com", Platform = "ubuntu", Status = "cached", SampledAt = hidden.UpdatedAt, Windows = new() { new QuotaWindow { Key = "weekly", Label = "Weekly", UsedPercent = 42, WindowMinutes = 10080, Kind = "rate_limit" } } });
            window.ApplyDashboardSample(hidden);
            await Settle(window);
            report.Checks[$"{name}_hidden_provider_closes_up"] = FindUid(window.ContentPanel, "provider:kimi-code") is null && FindUid(window.ContentPanel, "provider:muse") is not null;
            report.Checks[$"{name}_hidden_account_is_left_out"] = FindUid(window.ContentPanel, "row:claude:example-4") is null && FindUid(window.ContentPanel, "row:claude:example-1") is not null;
            report.Checks[$"{name}_unknown_provider_renders"] = FindUid(window.ContentPanel, "provider:newcode") is not null;
            report.Checks[$"{name}_status_counts_visible_providers"] = window.StatusText.Text.StartsWith("9 of 9", StringComparison.Ordinal);

            // A provider card's details list only the accounts shown in the tray, on the first click as on a redraw.
            var detailHidden = Clone(fixture);
            var zaiShown = detailHidden.Accounts.First(account => account.Provider == "zai");
            var zaiHidden = Clone(new AccountDashboard { Accounts = { zaiShown } }).Accounts[0];
            zaiHidden.Id = "zai:example-hidden"; zaiHidden.Email = "zai-hidden@example.com"; zaiHidden.Label = "Z.ai hidden account"; zaiHidden.TrayHidden = true;
            detailHidden.Accounts.Insert(detailHidden.Accounts.IndexOf(zaiShown) + 1, zaiHidden);
            window.ApplyDashboardSample(detailHidden);
            await Settle(window);
            window.ToggleDetailsForCheck("prov:zai");
            await Settle(window);
            bool LeaksHidden() => All(window.ContentPanel).OfType<TextBlock>().Any(text => text.Text.Contains("zai-hidden@example.com", StringComparison.Ordinal) || text.Text.Contains("Z.ai hidden account", StringComparison.Ordinal));
            var firstClick = FindUid(window.ContentPanel, "details") is not null && !LeaksHidden();
            window.ApplyDashboardSample(Clone(detailHidden));
            await Settle(window);
            report.Checks[$"{name}_provider_details_leave_out_tray_hidden_accounts"] = firstClick && FindUid(window.ContentPanel, "details") is not null && !LeaksHidden();
            window.ToggleDetailsForCheck("prov:zai");
            await Settle(window);

            // Details: full-row expansion under Claude 1.
            window.ApplyDashboardSample(Clone(fixture));
            window.ToggleDetailsForCheck("claude:example-1");
            await Settle(window);
            SavePng(window, Path.Combine(directory, $"panel-{name}-details.png"));
            report.Checks[$"{name}_row_click_opens_details"] = FindUid(window.ContentPanel, "details") is not null;
            window.ToggleDetailsForCheck("claude:example-1");

            // Settings: opens in the panel with an X; the gear is pressed; Escape closes it, a second Escape hides.
            window.OpenSettings();
            await Settle(window);
            SavePng(window, Path.Combine(directory, $"settings-{name}.png"));
            report.Checks[$"{name}_settings_in_panel_with_x"] = window.SettingsOpen && window.SettingsButton.IsChecked == true && FindUid(window.SettingsPanel, "settings-close") is not null;
            PressEscape(window);
            await Settle(window);
            report.Checks[$"{name}_escape_closes_settings"] = !window.SettingsOpen && window.SettingsButton.IsChecked == false;
            window.OpenSettings(); window.CloseSettings();
            report.Checks[$"{name}_gear_toggles_settings"] = !window.SettingsOpen;
            // The real controls: a gear click opens Settings (pressed), a second click closes it; the X closes it.
            Click(window.SettingsButton); await Settle(window);
            var gearOpened = window.SettingsOpen && window.SettingsButton.IsChecked == true;
            Click(window.SettingsButton); await Settle(window);
            report.Checks[$"{name}_gear_click_opens_then_closes"] = gearOpened && !window.SettingsOpen && window.SettingsButton.IsChecked == false;
            Click(window.SettingsButton); await Settle(window);
            if (FindUid(window.SettingsPanel, "settings-close") is Button close) Click(close);
            await Settle(window);
            report.Checks[$"{name}_x_click_closes_settings"] = !window.SettingsOpen && window.SettingsButton.IsChecked == false && FindUid(window.SettingsPanel, "settings-close") is not null;
            PressEscape(window);
            report.Checks[$"{name}_escape_then_hides_panel"] = !window.IsVisible;

            await PairedSettings(report, fixture, directory, name);

            // Animated Settings (the 2026-10-02 crash: the Appearance segmented thumb animated Width from
            // NaN, which throws only with real motion on): a real gear click opens Settings, the theme
            // segment switches (the thumb springs), the panel renders, and Escape closes it. Signed in and
            // with two Antigravity accounts.
            window.ApplyDashboardSample(Clone(fixture));
            await SettingsAnimated(report, window, directory, name + "_settings_opens_animated_signed_in");
            window.ApplyDashboardSample(Clone(twoAg));
            await SettingsAnimated(report, window, directory, name + "_settings_opens_animated_two_antigravity");

            // The restyled notification-area menu (WinForms ContextMenuStrip with the Atlas renderer).
            report.Checks[$"{name}_tray_menu_styled"] = TrayMenu(Path.Combine(directory, $"tray-menu-{name}.png"));

            // A sample that arrives while the panel is hidden (background start) still gets its platter on open.
            var hiddenFirst = new MainWindow(new Preferences { Theme = name, Hotkey = false }, loadConnection: false) { ShowActivated = false, Width = 760, Height = 850 };
            hiddenFirst.UseFixtureConnection();
            hiddenFirst.ApplyDashboardSample(Clone(fixture));
            hiddenFirst.ShowPanel();
            await Settle(hiddenFirst);
            report.Checks[$"{name}_platter_placed_after_hidden_sample"] = PlatterOn(hiddenFirst, "codex", "row:codex:example-2", measures, name + "_hidden");
            hiddenFirst.AllowClose = true; hiddenFirst.Close();

            await SignInStates(report, measures, directory, name);

            // First run: the sign-in screen.
            var signIn = new MainWindow(new Preferences { Theme = name, Hotkey = false }, loadConnection: false) { ShowActivated = false, Left = 40, Top = 40, Width = 760, Height = 850 };
            signIn.Show(); signIn.ShowSignIn(firstRun: true);
            await Settle(signIn);
            SavePng(signIn, Path.Combine(directory, $"signin-{name}.png"));
            // The gear works on the sign-in screen: Settings slides over it (its form rests), Escape returns to it.
            Click(signIn.SettingsButton); await Settle(signIn);
            var overSignIn = signIn.SettingsOpen && signIn.Body.Children.IndexOf(signIn.SettingsLayer) > signIn.Body.Children.IndexOf(signIn.SignInLayer) && !signIn.SignInLayer.IsEnabled;
            PressEscape(signIn); await Settle(signIn);
            report.Checks[$"{name}_settings_opens_over_sign_in_and_returns"] = overSignIn && !signIn.SettingsOpen && signIn.SignInLayer.IsVisible && signIn.SignInLayer.IsEnabled && signIn.IsVisible;
            await SettingsAnimated(report, signIn, directory, name + "_settings_opens_animated_sign_in");
            signIn.AllowClose = true; signIn.Close();
            window.AllowClose = true; window.Close();
        }
        report.Checks["tray_tooltip_reports_active_codex_weekly_left"] = Formatting.TrayTooltip(fixture) == "AI Account Center · Codex: codex-2, 91% weekly left";
        if (only is null || only.Equals("light", StringComparison.OrdinalIgnoreCase)) { await MotionChecks(report, fixture, measures); await RuntimeChecks(report, fixture, measures); await ResetPendingRender(report, fixture, capturedAt, directory); }
        foreach (var (key, value) in measures) report.Measurements[key] = Math.Round(value, 3);
        report.Passed = report.Checks.Values.All(value => value);
        return report;
    }

    /// <summary>Runs real animations and samples them every frame: a meter moving 9% to 63% and the platter gliding
    /// to a new row must ease out and never pass their targets.</summary>
    private static async Task MotionChecks(CheckReport report, AccountDashboard fixture, Dictionary<string, double> measures)
    {
        Motion.Enabled = true;
        Theme.Apply(ThemeMode.Light, animate: false);
        var window = new MainWindow(new Preferences { Theme = "light", Hotkey = false }, loadConnection: false) { ShowActivated = false, Left = 40, Top = 40, Width = 760, Height = 850 };
        try
        {
            window.UseFixtureConnection();
            window.Show();
            window.ApplyDashboardSample(Clone(fixture));
            await Settle(window);
            await Task.Delay(1200);
            var meter = All(window.ContentPanel).OfType<Meter>().First(candidate => candidate.Key == "codex:example-2|week");
            var startShown = meter.Shown;
            var changed = Clone(fixture);
            foreach (var account in changed.Accounts.Where(account => account.Provider == "codex")) account.IsActive = account.Id == "codex:example-3";
            changed.Accounts.First(account => account.Id == "codex:example-2").Windows.First(window => window.Key == "seven_day").UsedPercent = 63;
            var platter = window.PlatterFor("codex")!;
            var shift = (TranslateTransform)platter.RenderTransform;
            var startY = shift.Y;
            window.ApplyDashboardSample(changed);
            await Settle(window);
            var target = FindUid(window.ContentPanel, "row:codex:example-3")!.TranslatePoint(new Point(0, 0), (UIElement)platter.Parent).Y;
            var shownSamples = new List<double>(); var ySamples = new List<double>();
            var started = DateTime.UtcNow;
            while ((DateTime.UtcNow - started).TotalMilliseconds < 1400)
            {
                shownSamples.Add(meter.Shown); ySamples.Add(shift.Y);
                await Task.Delay(16);
                await window.Dispatcher.InvokeAsync(() => { }, System.Windows.Threading.DispatcherPriority.Render);
            }
            bool monotonic(List<double> values, bool rising) { for (int i = 1; i < values.Count; i++) if (rising ? values[i] + 1e-6 < values[i - 1] : values[i] - 1e-6 > values[i - 1]) return false; return true; }
            measures["motion_meter_from"] = startShown; measures["motion_meter_max"] = shownSamples.Max(); measures["motion_meter_final"] = meter.Shown; measures["motion_meter_samples"] = shownSamples.Count;
            measures["motion_meter_distinct_frames"] = shownSamples.Distinct().Count();
            measures["motion_platter_from"] = startY; measures["motion_platter_target"] = target; measures["motion_platter_max"] = ySamples.Max(); measures["motion_platter_final"] = shift.Y;
            report.Checks["meter_eases_to_value_without_overshoot"] = Math.Abs(startShown - 9) < 0.01 && shownSamples.Max() <= 63 + 1e-6 && Math.Abs(meter.Shown - 63) < 0.01 && monotonic(shownSamples, true) && shownSamples.Distinct().Count() > 3;
            report.Checks["platter_glides_without_overshoot"] = target > startY && ySamples.Max() <= target + 1e-6 && Math.Abs(shift.Y - target) < 0.5 && monotonic(ySamples, true) && ySamples.Distinct().Count() > 3;
        }
        finally { window.AllowClose = true; window.Close(); Motion.Enabled = false; }
    }

    /// <summary>Settings with real motion on, as on an interactive desktop (2026-10-02: opening Settings
    /// killed the tray because the Appearance segmented thumb animated Width from NaN, which only throws when the
    /// animation clock ticks). A real gear click opens Settings, the theme segment switches (the thumb springs),
    /// the panel renders, and Escape closes it. Any dispatcher exception fails the check and is recorded by type
    /// with the first line of its message (WPF animation errors name properties only, never data).</summary>
    private static async Task SettingsAnimated(CheckReport report, MainWindow window, string directory, string key)
    {
        Motion.Enabled = true;
        Exception? dispatchError = null;
        void OnUnhandled(object sender, System.Windows.Threading.DispatcherUnhandledExceptionEventArgs args)
        { dispatchError ??= args.Exception; args.Handled = true; }
        window.Dispatcher.UnhandledException += OnUnhandled;
        try
        {
            try
            {
                window.ShowPanel();
                await Settle(window);
                Click(window.SettingsButton);
                await Settle(window);
                await Task.Delay(500);
                await Settle(window);
                var opened = window.SettingsOpen && window.SettingsButton.IsChecked == true;
                var segmented = All(window.SettingsPanel).OfType<Segmented>().FirstOrDefault();
                var previous = segmented?.Value;
                if (segmented is not null) segmented.Select(segmented.Value == "dark" ? "light" : "dark", true);
                await Settle(window);
                await Task.Delay(600);
                await Settle(window);
                var switched = segmented is not null && previous is not null && segmented.Value != previous;
                SavePng(window, Path.Combine(directory, $"settings-animated-{key}.png"));
                var thumbPlaced = ThumbOnSelected(segmented, window);
                PressEscape(window);
                await Settle(window);
                var closed = !window.SettingsOpen;
                report.Checks[key] = dispatchError is null && opened && switched && thumbPlaced && closed;
                report.Notes[key + "_parts"] = $"opened={opened} switched={switched} thumb={thumbPlaced} closed={closed} error={dispatchError is not null}";
            }
            catch (Exception error) { dispatchError ??= error; report.Checks[key] = false; }
            if (dispatchError is not null)
            {
                var message = dispatchError.Message.Split('\n', '\r').FirstOrDefault(line => line.Length > 0) ?? "";
                if (message.Length > 220) message = message[..220];
                var inner = dispatchError.InnerException is null ? "" : " < " + dispatchError.InnerException.GetType().Name;
                report.Notes[key + "_error"] = dispatchError.GetType().FullName + inner + ": " + message;
            }
        }
        finally { window.Dispatcher.UnhandledException -= OnUnhandled; Motion.Enabled = false; }
    }

    /// <summary>The segmented thumb sits on a segment: as wide as its button, starting on its x.</summary>
    private static bool ThumbOnSelected(Segmented? segmented, MainWindow window)
    {
        if (segmented is null) return false;
        var thumb = All(segmented).OfType<Border>().FirstOrDefault();
        if (thumb is null || thumb.ActualWidth <= 0) return false;
        var thumbX = thumb.TranslatePoint(new Point(0, 0), window).X;
        return All(segmented).OfType<Button>().Where(button => button.IsVisible)
            .Any(button => Math.Abs(button.ActualWidth - thumb.ActualWidth) <= 1
                && Math.Abs(button.TranslatePoint(new Point(0, 0), window).X - thumbX) <= 1);
    }

    /// <summary>Refreshes must not pile up theme handlers (hidden or shown), and an idle panel must not use the CPU:
    /// nothing animates or polls between samples, shown or hidden.</summary>
    private static async Task RuntimeChecks(CheckReport report, AccountDashboard fixture, Dictionary<string, double> measures)
    {
        Motion.Enabled = true;
        Theme.Apply(ThemeMode.Light, animate: false);
        var window = new MainWindow(new Preferences { Theme = "light", Hotkey = false }, loadConnection: false) { ShowActivated = false, Width = 760, Height = 850 };
        try
        {
            window.UseFixtureConnection();
            window.ApplyDashboardSample(Clone(fixture));
            await Settle(window);
            GC.Collect(); GC.WaitForPendingFinalizers();
            var hiddenBefore = Theme.SubscriberCount;
            for (int i = 0; i < 20; i++) { window.ApplyDashboardSample(Clone(fixture)); await Settle(window); }
            GC.Collect(); GC.WaitForPendingFinalizers();
            var hiddenAfter = Theme.SubscriberCount;
            window.ShowPanel();
            await Settle(window); await Task.Delay(1500);
            var shownBefore = Theme.SubscriberCount;
            for (int i = 0; i < 20; i++) { window.ApplyDashboardSample(Clone(fixture)); await Settle(window); }
            await Task.Delay(1500);
            GC.Collect(); GC.WaitForPendingFinalizers();
            var shownAfter = Theme.SubscriberCount;
            measures["theme_handlers_hidden_before"] = hiddenBefore; measures["theme_handlers_hidden_after_20"] = hiddenAfter;
            measures["theme_handlers_shown_before"] = shownBefore; measures["theme_handlers_shown_after_20"] = shownAfter;
            report.Checks["theme_handlers_bounded_across_refreshes"] = hiddenAfter <= hiddenBefore && shownAfter <= shownBefore;

            async Task<double> IdlePercent()
            {
                var process = System.Diagnostics.Process.GetCurrentProcess();
                process.Refresh(); var cpu = process.TotalProcessorTime; var clock = System.Diagnostics.Stopwatch.StartNew();
                await Task.Delay(4000);
                process.Refresh();
                return (process.TotalProcessorTime - cpu).TotalMilliseconds / clock.Elapsed.TotalMilliseconds * 100;
            }
            // A refresh while hidden (the 60 s background sample) runs no spin; a visible panel spins.
            window.HidePopup();
            window.SetRefreshingForCheck(true); var spunHidden = window.RefreshSpinning; window.SetRefreshingForCheck(false);
            window.ShowPanel(); await Settle(window);
            window.SetRefreshingForCheck(true); var spunShown = window.RefreshSpinning; window.SetRefreshingForCheck(false);
            report.Checks["refresh_spins_only_while_visible"] = !spunHidden && spunShown;
            // Let the last motion and the runtime's own warm-up (tiered JIT of the 40 renders above) finish first.
            await Task.Delay(3000);
            var shownA = await IdlePercent(); var shownB = await IdlePercent();
            window.HidePopup();
            await Task.Delay(1000);
            var hiddenA = await IdlePercent(); var hiddenB = await IdlePercent();
            var shownIdle = Math.Min(shownA, shownB); var hiddenIdle = Math.Min(hiddenA, hiddenB);
            measures["idle_cpu_percent_shown_4s_a"] = shownA; measures["idle_cpu_percent_shown_4s_b"] = shownB;
            measures["idle_cpu_percent_hidden_4s_a"] = hiddenA; measures["idle_cpu_percent_hidden_4s_b"] = hiddenB;
            report.Checks["idle_panel_uses_no_cpu"] = shownIdle < 1.5 && hiddenIdle < 1.5;
        }
        finally { window.AllowClose = true; window.Close(); Motion.Enabled = false; }
    }

    /// <summary>F6 in the real panel. A Claude window whose reset passed after its sample shows "Reset at ... · new
    /// reading pending" with the unavailable (dashed) track and no number; a reading taken after its reset and a reset
    /// still ahead show normally; a provider card and Details say "Pending" with the reset; and the refresh timer's
    /// first step flips a window whose reset passes while the panel is open.</summary>
    private static async Task ResetPendingRender(CheckReport report, AccountDashboard fixture, DateTimeOffset capturedAt, string directory)
    {
        var saved = Formatting.Now;
        Motion.Enabled = false;
        Theme.Apply(ThemeMode.Light, animate: false);
        var window = new MainWindow(new Preferences { Theme = "light", Hotkey = false }, loadConnection: false) { ShowActivated = false, Left = 40, Top = 40, Width = 760, Height = 850 };
        try
        {
            window.UseFixtureConnection();
            window.Show();
            var sample = Clone(fixture);
            var one = sample.Accounts.First(account => account.Id == "claude:example-1");
            one.SampledAt = capturedAt.AddHours(-2).ToString("O");
            var week = one.Windows.First(item => item.Key == "seven_day");
            week.ResetAt = capturedAt.AddHours(-1).ToString("O");            // passed after the sample: pending
            var fable = one.Windows.First(item => item.Key == "seven_day_fable");
            fable.ResetAt = capturedAt.AddHours(-3).ToString("O");           // passed before the sample: the reading stands
            var two = sample.Accounts.First(account => account.Id == "claude:example-2");
            var twoWeek = two.Windows.First(item => item.Key == "seven_day"); // still ahead: the reading stands
            var codexTwo = sample.Accounts.First(account => account.Id == "codex:example-2");
            codexTwo.SampledAt = capturedAt.AddHours(-2).ToString("O");
            codexTwo.Windows.First(item => item.Key == "seven_day").ResetAt = capturedAt.AddHours(-1).ToString("O");
            var kimi = sample.Accounts.First(account => account.Provider == "kimi-code");
            kimi.SampledAt = capturedAt.AddMinutes(-30).ToString("O");
            var kimiWindow = kimi.Windows.First(item => item.Key == "5h");
            kimiWindow.ResetAt = capturedAt.AddMinutes(-10).ToString("O");
            window.ApplyDashboardSample(sample);
            await Settle(window);
            Meter Find(string key) => All(window.ContentPanel).OfType<Meter>().First(meter => meter.Key == key);
            var pending = Find("claude:example-1|week");
            report.Notes["reset_pending_compact_text"] = pending.ValueShown;
            report.Checks["reset_pending_meter_has_no_number_no_fill_never_zero"] = pending.Target is null && pending.DrawnUnavailable && pending.ValueShown.StartsWith("Reset ", StringComparison.Ordinal) && !pending.ValueClipped
                && !pending.ValueShown.Contains('%') && pending.ResetShown.Length == 0 && pending.TooltipText.Contains("new reading pending", StringComparison.Ordinal);
            var codexPending = Find("codex:example-2|week");
            report.Notes["reset_pending_codex_text"] = codexPending.ValueShown;
            report.Checks["reset_pending_codex_cell_whole_without_number_or_notch"] = codexPending.Target is null && codexPending.DrawnUnavailable && codexPending.ValueShown.StartsWith("Reset ", StringComparison.Ordinal) && !codexPending.ValueClipped
                && !All(codexPending).Any(element => element is Border { Width: 2, Height: 12 } notch && notch.IsVisible);
            var before = Find("claude:example-1|fable");
            report.Checks["reset_passed_before_the_sample_shows_the_reading"] = before.Target == fable.UsedPercent && !before.DrawnUnavailable && before.ValueShown.EndsWith("%", StringComparison.Ordinal);
            var ahead = Find("claude:example-2|week");
            report.Checks["reset_still_ahead_shows_the_reading"] = ahead.Target == twoWeek.UsedPercent && !ahead.DrawnUnavailable;
            var card = Find(kimi.Id + "|5h");
            report.Notes["reset_pending_card_reset"] = card.ResetShown;
            report.Checks["reset_pending_provider_card_says_pending_with_the_reset"] = card.Target is null && card.DrawnUnavailable && card.ValueShown == "Pending" && card.ResetShown.StartsWith("Reset at ", StringComparison.Ordinal);
            window.ToggleDetailsForCheck("claude:example-1");
            await Settle(window);
            var detail = Find("detail:claude:example-1|seven_day");
            report.Notes["reset_pending_detail_text"] = detail.LongResetShown;
            report.Checks["reset_pending_details_say_pending_with_the_reset"] = detail.Target is null && detail.DrawnUnavailable && detail.ValueShown == "Pending" && detail.LongResetShown.EndsWith(" · new reading pending", StringComparison.Ordinal);
            SavePng(window, Path.Combine(directory, "reset-pending-light.png"));
            window.ToggleDetailsForCheck("claude:example-1");

            // The panel stays open while a reset passes: the timer's first step redraws only that window.
            Formatting.Now = () => capturedAt.AddSeconds(5);
            var soon = Clone(fixture);
            soon.Accounts.First(account => account.Id == "claude:example-2").Windows.First(item => item.Key == "seven_day").ResetAt = capturedAt.AddSeconds(35).ToString("O");
            window.ApplyDashboardSample(soon);
            await Settle(window);
            var shownBefore = Find("claude:example-2|week").Target is double;
            window.ReevaluateResets();
            var keptReading = Find("claude:example-2|week").Target is double;
            Formatting.Now = () => capturedAt.AddSeconds(65);
            window.ReevaluateResets();
            await Settle(window);
            var flipped = Find("claude:example-2|week");
            report.Notes["reset_flip_text"] = flipped.ValueShown;
            report.Checks["refresh_timer_flips_a_window_when_its_reset_passes"] = shownBefore && keptReading && flipped.Target is null && flipped.DrawnUnavailable && flipped.ValueShown.StartsWith("Reset ", StringComparison.Ordinal) && !flipped.ValueClipped;
        }
        finally { Formatting.Now = saved; window.AllowClose = true; window.Close(); }
    }

    /// <summary>The fifteen sign-in states (TSIGN-C), each shown directly in the real panel: the expected title, the
    /// card inside the sign-in area and clear of the footer, no text cut off, the primary's label centred, and the regions
    /// each state opens. One PNG per state for review.</summary>
    private static async Task SignInStates(CheckReport report, Dictionary<string, double> measures, string directory, string name)
    {
        var expected = new (SignInState State, string Title, string[] Open, string? Variant)[]
        {
            (SignInState.FirstRun, "Connect this PC", new[] { "addr", "acts" }, null),
            (SignInState.Password, "Sign in to pair", new[] { "creds", "device", "acts" }, null),
            (SignInState.SetupCode, "Set up sign-in", new[] { "creds", "setup", "acts" }, null),
            (SignInState.Pairing, "Pairing this PC", new[] { "steps", "acts" }, null),
            (SignInState.NotLocal, "This address isn't on your local network", new[] { "addr", "guide", "acts" }, null),
            (SignInState.PairingOff, "Pairing is turned off for remote computers", new[] { "acts" }, null),
            (SignInState.WrongPassword, "Sign in to pair", new[] { "creds", "device", "acts" }, null),
            (SignInState.RateLimited, "Sign in to pair", new[] { "banner", "creds", "device", "acts" }, null),
            (SignInState.Unreachable, "Can't reach that address", new[] { "addr", "acts" }, null),
            (SignInState.WrongAddress, "That isn't a dashboard address", new[] { "addr", "acts" }, null),
            (SignInState.Securing, "Securing this tray", new[] { "steps" }, null),
            (SignInState.SignedOut, "This tray was signed out", new[] { "creds", "device", "acts" }, null),
            (SignInState.SignedOutAll, "This tray was signed out", new[] { "creds", "device", "acts" }, null),
            (SignInState.Expired, "This tray was signed out", new[] { "creds", "device", "acts" }, null),
            (SignInState.Success, "Paired", new[] { "steps", "acts" }, null),
            (SignInState.Password, "Sign in to re-pair", new[] { "banner", "creds", "device", "acts" }, "repair"),
            (SignInState.Unreachable, "Can't reach that address", new[] { "banner", "addr", "acts" }, "repair"),
            (SignInState.Password, "Sign in to pair this tray", new[] { "banner", "creds", "device", "acts" }, "upgrade"),
            (SignInState.Password, "Sign in with your password", new[] { "creds", "acts" }, "legacy"),
            (SignInState.PairingOff, "Pairing is turned off for remote computers", new[] { "acts" }, "fallback"),
            (SignInState.Unreachable, "Can't reach that address", new[] { "banner", "addr", "acts" }, "uncertain"),
        };
        var regions = new[] { "banner", "addr", "guide", "creds", "setup", "device", "steps", "acts" };
        foreach (var (state, title, open, variant) in expected)
        {
            var tag = $"{name}_signin_{state.ToString().ToLowerInvariant()}{(variant is null ? "" : "_" + variant)}";
            var window = new MainWindow(new Preferences { Theme = name, Hotkey = false }, loadConnection: false) { ShowActivated = false, Left = 40, Top = 40, Width = 760, Height = 850 };
            try
            {
                window.Show();
                window.PresetSignInForCheck(state, variant == "repair", variant);
                await Settle(window);
                var view = window.SignIn;
                SavePng(window, Path.Combine(directory, tag + ".png"));
                var area = view.TranslatePoint(new Point(0, 0), window);
                var card = view.Card.TranslatePoint(new Point(0, 0), window);
                var cardBottom = card.Y + view.Card.ActualHeight;
                var areaBottom = area.Y + view.ActualHeight;
                measures[tag + "_card_top"] = card.Y - area.Y; measures[tag + "_card_room_below"] = areaBottom - cardBottom;
                report.Checks[tag + "_title"] = view.TitleText == title;
                report.Checks[tag + "_card_inside_area_and_clear_of_footer"] = card.Y >= area.Y && cardBottom <= areaBottom && cardBottom <= window.Footer.TranslatePoint(new Point(0, 0), window).Y;
                report.Checks[tag + "_regions"] = regions.All(region => view.RegionOpen(region) == open.Contains(region));
                report.Checks[tag + "_no_text_cut_off"] = NoClippedText(view.Card);
                var label = All(view.PrimaryButton).OfType<TextBlock>().FirstOrDefault(block => block.IsVisible && block.Opacity > 0.5 && block.Text.Length > 0);
                if (view.RegionOpen("acts") && label is not null && label.ActualWidth > 0)
                {
                    var buttonMid = view.PrimaryButton.TranslatePoint(new Point(view.PrimaryButton.ActualWidth / 2, 0), window).X;
                    var parent = (FrameworkElement)VisualTreeHelper.GetParent(label);
                    var labelMid = parent.TranslatePoint(new Point(parent.ActualWidth / 2, 0), window).X;
                    report.Checks[tag + "_primary_label_centred"] = Math.Abs(buttonMid - labelMid) <= 1;
                }
                report.Notes[tag + "_status"] = window.StatusText.Text;
                if (variant is "fallback") report.Checks[tag + "_offers_the_password"] = FindUid(view, "si-use-password") is Button { IsVisible: true };
                if (variant is "uncertain") report.Checks[tag + "_never_says_unchanged"] = view.BannerText.StartsWith("The current key may have been replaced", StringComparison.Ordinal) && view.AltButton.Content as string == "Back to usage";
                if (variant is "upgrade") report.Checks[tag + "_speaks_of_the_saved_password"] = view.BannerText.StartsWith("Pairing replaces the saved password", StringComparison.Ordinal) && window.StatusText.Text == "Pairing · the saved password still works";
            }
            catch (Exception error) { report.Checks[tag + "_rendered"] = false; report.Notes[tag] = error.GetType().Name + ": " + error.Message; }
            finally { window.AllowClose = true; window.Close(); }
        }
    }

    /// <summary>Settings › Connection on a paired tray: the paired lines, Re-pair and Disconnect, then Disconnect's inline
    /// confirm. The dashboard's view of this computer is given, so the render sends nothing.</summary>
    private static async Task PairedSettings(CheckReport report, AccountDashboard fixture, string directory, string name)
    {
        var window = new MainWindow(new Preferences { Theme = name, Hotkey = false }, loadConnection: false) { ShowActivated = false, Left = 40, Top = 40, Width = 760, Height = 850 };
        try
        {
            window.UsePairedFixtureConnection();
            window.ConnectionLineForRender = new AuthCheck { Connection = new AuthConnection { Peer = "192.168.1.31", Trusted = true } };
            window.Show();
            window.ApplyDashboardSample(Clone(fixture));
            window.OpenSettings();
            await Settle(window);
            SavePng(window, Path.Combine(directory, $"settings-paired-{name}.png"));
            var card = FindUid(window.SettingsPanel, "settings-connection");
            var who = FindUid(window.SettingsPanel, "settings-connection-who") as TextBlock;
            var via = FindUid(window.SettingsPanel, "settings-this-connection") as TextBlock;
            var whoText = who is null ? "" : new System.Windows.Documents.TextRange(who.ContentStart, who.ContentEnd).Text;
            report.Notes[$"{name}_settings_paired_who"] = whoText;
            report.Checks[$"{name}_settings_paired_connection_card"] = card is not null && whoText.StartsWith("Paired as Windows tray", StringComparison.Ordinal)
                && via?.Text == "This connection: 192.168.1.31, trusted local network" && FindUid(window.SettingsPanel, "settings-repair") is Button { IsVisible: true, IsEnabled: true }
                && FindUid(window.SettingsPanel, "settings-disconnect") is Button { IsVisible: true, IsEnabled: true } && NoClippedText(card!);
            if (FindUid(window.SettingsPanel, "settings-disconnect") is Button disconnect) Click(disconnect);
            await Settle(window);
            SavePng(window, Path.Combine(directory, $"settings-disconnect-{name}.png"));
            var line = FindUid(window.SettingsPanel, "settings-disconnect-line");
            var text = FindUid(window.SettingsPanel, "settings-disconnect-text") as TextBlock;
            report.Checks[$"{name}_settings_disconnect_confirm"] = line is { IsVisible: true, ActualHeight: > 0 } && text?.Text.StartsWith("Disconnect this Windows tray?", StringComparison.Ordinal) == true
                && FindUid(window.SettingsPanel, "settings-disconnect-yes") is Button { IsVisible: true } && FindUid(window.SettingsPanel, "settings-disconnect-cancel") is Button { IsVisible: true }
                && NoClippedText(card!) && window.ClientForCheck is { Paired: true };
        }
        catch (Exception error) { report.Checks[$"{name}_settings_paired_rendered"] = false; report.Notes[$"{name}_settings_paired"] = error.GetType().Name + ": " + error.Message; }
        finally { window.AllowClose = true; window.Close(); }
    }

    /// <summary>No single-line text in the element is wider than its own box (wrapped text is laid out to fit).</summary>
    private static bool NoClippedText(FrameworkElement root)
    {
        foreach (var block in All(root).OfType<TextBlock>().Where(block => block.IsVisible && block.ActualWidth > 0 && block.TextWrapping == TextWrapping.NoWrap && block.Text.Length > 0))
        {
            var typeface = new Typeface(block.FontFamily, block.FontStyle, block.FontWeight, block.FontStretch);
            var width = new FormattedText(block.Text, CultureInfo.CurrentCulture, FlowDirection.LeftToRight, typeface, block.FontSize, Brushes.Black, VisualTreeHelper.GetDpi(block).PixelsPerDip).WidthIncludingTrailingWhitespace;
            if (block.TextTrimming == TextTrimming.None && width > block.ActualWidth + 0.75) return false;
        }
        return true;
    }

    private static bool TrayMenu(string path)
    {
        using var menu = TrayIcon.CreateStyledMenu();
        TrayIcon.Populate(menu, null, null, null, null, null);
        menu.Show(new System.Drawing.Point(-4000, -4000));
        try
        {
            var size = menu.Size;
            using var bitmap = new System.Drawing.Bitmap(size.Width, size.Height);
            menu.DrawToBitmap(bitmap, new System.Drawing.Rectangle(0, 0, size.Width, size.Height));
            bitmap.Save(path, System.Drawing.Imaging.ImageFormat.Png);
            var labels = menu.Items.OfType<System.Windows.Forms.ToolStripItem>().Select(item => item.Text).ToArray();
            return labels.SequenceEqual(new[] { "AI Account Center", "Open accounts", "Open dashboard", "Refresh now", "Settings", "", "Quit AI Account Center" })
                && menu.Items.OfType<System.Windows.Forms.ToolStripMenuItem>().All(item => item.Image is not null && item.Font.FontFamily.Name == "Instrument Sans")
                && menu.Items.OfType<System.Windows.Forms.ToolStripMenuItem>().Count(item => !item.Enabled) == 1
                && menu.Renderer is System.Windows.Forms.ToolStripProfessionalRenderer && menu.BackColor.ToArgb() == TrayIcon.ToDrawing("Card").ToArgb();
        }
        finally { menu.Close(); }
    }

    private static async Task Settle(Window window)
    {
        window.UpdateLayout();
        await window.Dispatcher.InvokeAsync(() => { }, System.Windows.Threading.DispatcherPriority.Loaded);
        await window.Dispatcher.InvokeAsync(() => { }, System.Windows.Threading.DispatcherPriority.Render);
        window.UpdateLayout();
        await window.Dispatcher.InvokeAsync(() => { }, System.Windows.Threading.DispatcherPriority.ApplicationIdle);
    }

    public static void SavePng(FrameworkElement element, string path, double scale = 2)
    {
        var width = (int)Math.Ceiling(element.ActualWidth * scale);
        var height = (int)Math.Ceiling(element.ActualHeight * scale);
        var bitmap = new RenderTargetBitmap(width, height, 96 * scale, 96 * scale, PixelFormats.Pbgra32);
        var visual = new DrawingVisual();
        using (var context = visual.RenderOpen())
        {
            context.DrawRectangle(new SolidColorBrush(Color.FromRgb(0xD9, 0xDF, 0xE5)), null, new Rect(0, 0, element.ActualWidth, element.ActualHeight));
            context.DrawRectangle(new VisualBrush(element), null, new Rect(0, 0, element.ActualWidth, element.ActualHeight));
        }
        bitmap.Render(visual);
        var encoder = new PngBitmapEncoder(); encoder.Frames.Add(BitmapFrame.Create(bitmap));
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(path))!);
        using var output = File.Create(path);
        encoder.Save(output);
    }

    /// <summary>A real click: ButtonBase.OnClick (a ToggleButton toggles first, then raises Click).</summary>
    private static void Click(System.Windows.Controls.Primitives.ButtonBase button) =>
        typeof(System.Windows.Controls.Primitives.ButtonBase).GetMethod("OnClick", System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance)!.Invoke(button, null);

    private static void PressEscape(Window window)
    {
        var source = PresentationSource.FromVisual(window);
        if (source is null) return;
        window.RaiseEvent(new KeyEventArgs(Keyboard.PrimaryDevice, source, 0, Key.Escape) { RoutedEvent = Keyboard.PreviewKeyDownEvent });
    }

    public static FrameworkElement? FindUid(DependencyObject parent, string uid)
    {
        foreach (var child in LogicalTreeHelper.GetChildren(parent).OfType<FrameworkElement>())
        {
            if (child.Uid == uid) return child;
            var nested = FindUid(child, uid);
            if (nested is not null) return nested;
        }
        return null;
    }

    private static IEnumerable<FrameworkElement> All(DependencyObject parent)
    {
        foreach (var child in LogicalTreeHelper.GetChildren(parent).OfType<FrameworkElement>())
        {
            yield return child;
            foreach (var nested in All(child)) yield return nested;
        }
    }

    private static double X(FrameworkElement element, Window window) => element.TranslatePoint(new Point(0, 0), window).X;

    private static double Y(FrameworkElement element, Window window) => element.TranslatePoint(new Point(0, 0), window).Y;

    /// <summary>Check-circle left = Activate left, "Active" left = "Activate" label left, within 0.5 px.</summary>
    private static bool Aligned(MainWindow window, string provider, Dictionary<string, double> measures, string tag)
    {
        var section = FindUid(window.ContentPanel, "section:" + provider);
        if (section is null) return false;
        var buttons = All(section).OfType<Button>().Where(button => button.Uid == "mutation:activate" && button.IsVisible).ToArray();
        var actives = All(section).Where(element => element.Uid == "active-label" && element.IsVisible).ToArray();
        if (buttons.Length == 0 || actives.Length != 1) return false;
        var active = actives[0];
        var check = All(active).First(element => element.Uid == "active-check");
        var text = All(active).OfType<TextBlock>().First(element => element.Uid == "active-text");
        bool ok = true;
        foreach (var button in buttons)
        {
            var label = (TextBlock)button.Content;
            var buttonLeft = X(button, window); var labelLeft = X(label, window);
            ok &= Math.Abs(X(check, window) - buttonLeft) <= 0.5 && Math.Abs(X(text, window) - labelLeft) <= 0.5;
            measures[tag + "_button"] = buttonLeft; measures[tag + "_label"] = labelLeft;
        }
        measures[tag + "_check"] = X(check, window); measures[tag + "_active_text"] = X(text, window);
        return ok;
    }

    /// <summary>Every account section's name starts on one x, and the first meter (and its caption) starts on one x,
    /// whatever the number of meters.</summary>
    private static bool Columns(MainWindow window, Dictionary<string, double> measures, string tag)
    {
        var names = new List<double>(); var meters = new List<double>();
        foreach (var provider in new[] { "claude", "codex", "antigravity" })
        {
            var section = FindUid(window.ContentPanel, "section:" + provider);
            if (section is null) return false;
            var nameBlock = All(section).FirstOrDefault(element => element.Uid == "section-name");
            if (nameBlock is null) return false;
            names.Add(X(nameBlock, window));
            var first = All(section).OfType<Meter>().Where(meter => meter.IsVisible).Select(meter => X(meter, window)).DefaultIfEmpty(double.NaN).Min();
            meters.Add(first);
            measures[$"{tag}_{provider}_name_x"] = names[^1]; measures[$"{tag}_{provider}_first_meter_x"] = first;
        }
        return names.Max() - names.Min() <= 0.5 && meters.Max() - meters.Min() <= 0.5;
    }

    private static bool SameLine(Dictionary<string, double> measures, string a, string b) => measures.TryGetValue(a, out var x) && measures.TryGetValue(b, out var y) && Math.Abs(x - y) <= 0.5;

    /// <summary>Antigravity's slot (Not reported ring) starts on the Codex slot line.</summary>
    private static bool SlotLine(MainWindow window, Dictionary<string, double> measures, string name)
    {
        var section = FindUid(window.ContentPanel, "section:antigravity");
        var codexButton = All(FindUid(window.ContentPanel, "section:codex")!).OfType<Button>().FirstOrDefault(button => button.Uid == "mutation:activate");
        if (section is null || codexButton is null) return false;
        var ring = All(section).OfType<System.Windows.Shapes.Ellipse>().FirstOrDefault();
        if (ring is null) return false;
        measures[name + "_ag_ring"] = X(ring, window);
        return Math.Abs(X(ring, window) - X(codexButton, window)) <= 0.5;
    }

    private static bool PlatterOn(MainWindow window, string provider, string rowUid, Dictionary<string, double> measures, string tag)
    {
        var row = FindUid(window.ContentPanel, rowUid);
        var platter = window.PlatterFor(provider);
        if (row is null || platter is null || platter.Opacity < 0.99) return false;
        var rowY = row.TranslatePoint(new Point(0, 0), window).Y;
        var platterY = platter.TranslatePoint(new Point(0, 0), window).Y;
        measures[tag + "_row_y"] = rowY; measures[tag + "_platter_y"] = platterY;
        return Math.Abs(rowY - platterY) <= 0.5 && Math.Abs(row.ActualHeight - platter.ActualHeight) <= 0.5;
    }

    private static bool Fable(MainWindow window)
    {
        var max = new[] { "claude:example-1", "claude:example-3", "claude:example-4" };
        var meters = All(window.ContentPanel).OfType<Meter>().ToArray();
        return max.All(id => meters.Any(meter => meter.Key == id + "|fable" && meter.Target == 0))
            && meters.All(meter => meter.Key != "claude:example-2|fable");
    }

    private static bool Order(MainWindow window, string[] expected)
    {
        var uids = window.ContentPanel.Children.OfType<FrameworkElement>().Select(element => element.Uid).Where(uid => uid.Length > 0).ToArray();
        return uids.SequenceEqual(expected);
    }

    public static bool FontResolves()
    {
        bool ok = true;
        foreach (var (family, weight, stretch) in new[] { (Theme.Sans, FontWeights.Normal, FontStretches.Normal), (Theme.Sans, FontWeights.Medium, FontStretches.Normal), (Theme.Sans, FontWeights.SemiBold, FontStretches.Normal), (Theme.Sans, FontWeights.Bold, FontStretches.Normal), (Theme.Numerals, FontWeights.SemiBold, FontStretches.Normal) })
        {
            var typeface = new Typeface(family, FontStyles.Normal, weight, stretch);
            if (!typeface.TryGetGlyphTypeface(out var glyphs)) { ok = false; continue; }
            var name = glyphs.Win32FamilyNames.Values.FirstOrDefault() ?? "";
            ok &= name.StartsWith("Instrument Sans", StringComparison.Ordinal) && glyphs.Weight == weight;
            var advances = "0123456789%".Select(c => glyphs.AdvanceWidths[glyphs.CharacterToGlyphMap[c]]).Distinct().Count();
            ok &= advances == 1;
        }
        return ok;
    }
}
