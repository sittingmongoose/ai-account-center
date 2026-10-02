using System;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using Microsoft.Win32;

namespace CCSBar;

public partial class App : System.Windows.Application
{
    private TrayIcon? tray;
    private Hotkey? hotkey;
    private Mutex? instance;
    private EventWaitHandle? showRequest;
    private RegisteredWaitHandle? showListener;
    private bool ownsInstance;
    private Preferences preferences = new();

    /// <summary>Registers the shared theme brushes under their token names so XAML can use DynamicResource.</summary>
    internal void RegisterTheme()
    {
        // Application resources are sealed (frozen), so styles get frozen copies at the target colours, replaced on
        // every theme change. Elements built in code hold the shared animated brushes and cross-fade instead.
        foreach (var key in Theme.Tokens.Keys)
        {
            var brush = new SolidColorBrush(Theme.Color(key, Theme.IsDark)); brush.Freeze();
            Resources[key] = brush;
        }
        if (!themeHooked) { Theme.Changed += RegisterTheme; themeHooked = true; }
    }
    private bool themeHooked;

    protected override async void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        try
        {
            var args = e.Args;
            if (args.Length > 0 && args[0] == "--configure-stdin")
            {
                var input = await Console.In.ReadToEndAsync();
                if (input.Length > 16_384) throw new ArgumentException("Connection input is too large.");
                var settings = JsonSerializer.Deserialize<ConnectionSettings>(input, Formatting.Json) ?? throw new ArgumentException("Invalid connection settings.");
                SecureStore.Save(settings);
                Console.WriteLine("Dashboard connection stored with current-user DPAPI.");
                Shutdown(0); return;
            }
            preferences = Preferences.Load();
            Theme.Apply(preferences.Mode, animate: false);
            RegisterTheme();
            if (args.Length > 1 && args[0] is "--check" or "--check-live")
            {
                var report = args[0] == "--check-live" ? await Checks.Live() : await Checks.Run();
                WriteReport(args[1], report);
                Shutdown(report.Passed ? 0 : 1); return;
            }
            if (args.Length > 1 && args[0] == "--render-fixture")
            {
                // Offline: the bundled sanitized fixture only. No connection, provider request or account change.
                // Failures are reported in the output folder, never in the installed app's state folder.
                CheckReport report;
                try { report = await FixtureRender.Run(this, args[1], args.Length > 2 ? args[2] : null); }
                catch (Exception error)
                {
                    Directory.CreateDirectory(args[1]);
                    File.WriteAllText(Path.Combine(args[1], "render-error.txt"), error.ToString());
                    report = new CheckReport();
                }
                WriteReport(Path.Combine(args[1], "render-checks" + (args.Length > 2 ? "-" + args[2] : "") + ".json"), report);
                Shutdown(report.Passed ? 0 : 1); return;
            }
            if (args.Length > 1 && args[0] == "--render-proof")
            {
                Motion.Enabled = false;
                var proof = new MainWindow(preferences);
                MainWindow = proof;
                // Render the actual live WPF tree without creating a second tray instance.
                proof.Show();
                await proof.Refresh(false);
                proof.UpdateLayout();
                await Dispatcher.InvokeAsync(() => { }, System.Windows.Threading.DispatcherPriority.Render);
                FixtureRender.SavePng(proof, args[1]);
                proof.AllowClose = true;
                Shutdown(0); return;
            }

            instance = new Mutex(true, "Local\\CCSBar-Windows-v1", out ownsInstance);
            showRequest = new EventWaitHandle(false, EventResetMode.AutoReset, "Local\\CCSBar-Windows-show-v1");
            if (!ownsInstance)
            {
                // Launching again (Start menu, desktop shortcut) shows the running tray's panel. Let it take the
                // foreground: this process was started by the user, so it may pass that right on.
                AllowSetForegroundWindow(-1);
                showRequest.Set(); Shutdown(0); return;
            }
            var background = args.Contains("--background");
            var window = new MainWindow(preferences); MainWindow = window;
            window.QuitRequested = Quit;
            CreateTray(window);
            CreateHotkey(window);
            SystemEvents.UserPreferenceChanged += OnUserPreferenceChanged;
            showListener = ThreadPool.RegisterWaitForSingleObject(showRequest,
                (_, _) => Dispatcher.BeginInvoke(new Action(async () => await window.OpenPopup())), null, Timeout.Infinite, false);
            if (window.IsConfigured)
            {
                if (background) await window.Refresh(false);
                else await window.OpenPopup();
            }
            else await window.OpenPopup();
        }
        catch
        {
            // Never log exception bodies: connection credentials and account identities stay private.
            if (e.Args.Length > 0 && e.Args[0].StartsWith("--", StringComparison.Ordinal) && e.Args[0] != "--background") { Shutdown(1); return; }
            Directory.CreateDirectory(SecureStore.StateDirectory);
            File.WriteAllText(Path.Combine(SecureStore.StateDirectory, "startup-status.txt"), "AI Account Center startup failed. Check the connection or reinstall AI Account Center.");
            Shutdown(1);
        }
    }

    private void CreateTray(MainWindow window)
    {
        tray = new TrayIcon();
        tray.ToggleRequested += () => Dispatcher.BeginInvoke(new Action(async () => await window.TogglePopup()));
        tray.OpenRequested += () => Dispatcher.BeginInvoke(new Action(async () => await window.OpenPopup()));
        tray.DashboardRequested += () => Dispatcher.BeginInvoke(new Action(window.OpenDashboard));
        tray.RefreshRequested += () => Dispatcher.BeginInvoke(new Action(async () => await window.OpenPopup()));
        tray.SettingsRequested += () => Dispatcher.BeginInvoke(new Action(async () => await window.OpenPopup(settings: true)));
        tray.QuitRequested += () => Dispatcher.BeginInvoke(new Action(Quit));
        window.SampleChanged += () => tray?.SetTooltip(Formatting.TrayTooltip(window.Dashboard, window.IsStale, window.IsConfigured));
        tray.SetTooltip(Formatting.TrayTooltip(window.Dashboard, false, window.IsConfigured));
        SetWindowIcon(window);
    }

    private void CreateHotkey(MainWindow window)
    {
        hotkey = new Hotkey();
        hotkey.Pressed += () => Dispatcher.BeginInvoke(new Action(async () => await window.TogglePopup()));
        if (preferences.Hotkey) hotkey.Register();
        window.HotkeyState = () => preferences.Hotkey ? hotkey.Registered : null;
        window.SetHotkey = on =>
        {
            if (on) return hotkey.Register();
            hotkey.Unregister(); return false;
        };
    }

    private static void SetWindowIcon(Window window)
    {
        try
        {
            using var stream = TrayIcon.IconStream(SystemTheme.SystemUsesLightTheme());
            window.Icon = BitmapFrame.Create(stream, BitmapCreateOptions.None, BitmapCacheOption.OnLoad);
        }
        catch { /* The window keeps the application icon. */ }
    }

    private void OnUserPreferenceChanged(object sender, UserPreferenceChangedEventArgs e)
    {
        if (e.Category is not (UserPreferenceCategory.General or UserPreferenceCategory.Color or UserPreferenceCategory.VisualStyle)) return;
        Dispatcher.BeginInvoke(new Action(() =>
        {
            if (Theme.Mode == ThemeMode.Auto) Theme.Apply(ThemeMode.Auto, animate: true);
            tray?.ApplyTheme();
            if (MainWindow is not null) SetWindowIcon(MainWindow);
        }));
    }

    private static void WriteReport(string path, CheckReport report)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(path))!);
        File.WriteAllText(path, JsonSerializer.Serialize(report, new JsonSerializerOptions(Formatting.Json) { WriteIndented = true }));
        Console.WriteLine(report.Passed ? "AI Account Center checks passed." : "AI Account Center checks failed.");
    }

    public void Quit()
    {
        if (MainWindow is MainWindow window) window.AllowClose = true;
        Shutdown();
    }

    protected override void OnExit(ExitEventArgs e)
    {
        SystemEvents.UserPreferenceChanged -= OnUserPreferenceChanged;
        tray?.Dispose(); hotkey?.Dispose();
        showListener?.Unregister(null); showRequest?.Dispose();
        if (ownsInstance) instance?.ReleaseMutex(); instance?.Dispose();
        base.OnExit(e);
    }

    [DllImport("user32.dll")] private static extern bool AllowSetForegroundWindow(int processId);
}
