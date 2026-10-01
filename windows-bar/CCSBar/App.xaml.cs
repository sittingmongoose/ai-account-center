using System;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Reflection;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using Forms = System.Windows.Forms;

namespace CCSBar;

public partial class App : System.Windows.Application
{
    private Forms.NotifyIcon? tray;
    private Mutex? instance;
    private EventWaitHandle? showRequest;
    private RegisteredWaitHandle? showListener;
    private bool ownsInstance;
    private Icon? icon;

    protected override async void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        try
        {
            if (e.Args.Length > 0 && e.Args[0] == "--configure-stdin")
            {
                var input = await Console.In.ReadToEndAsync();
                if (input.Length > 16_384) throw new ArgumentException("Connection input is too large.");
                var settings = JsonSerializer.Deserialize<ConnectionSettings>(input, Formatting.Json) ?? throw new ArgumentException("Invalid connection settings.");
                SecureStore.Save(settings);
                Console.WriteLine("Dashboard connection stored with current-user DPAPI.");
                Shutdown(0); return;
            }
            if (e.Args.Length > 1 && e.Args[0] is "--check" or "--check-live")
            {
                var live = e.Args[0] == "--check-live";
                var report = live ? await Checks.Live() : await Checks.Run();
                WriteReport(e.Args[1], report);
                Shutdown(report.Passed ? 0 : 1); return;
            }
            if (e.Args.Length > 1 && e.Args[0] == "--render-proof")
            {
                var proof = new MainWindow();
                MainWindow = proof;
                // Render the actual live WPF tree without creating a second tray instance.
                proof.Show();
                await proof.Refresh(false);
                proof.UpdateLayout();
                await Dispatcher.InvokeAsync(() => { }, System.Windows.Threading.DispatcherPriority.Render);
                var bitmap = new RenderTargetBitmap((int)Math.Ceiling(proof.ActualWidth), (int)Math.Ceiling(proof.ActualHeight), 96, 96, PixelFormats.Pbgra32);
                bitmap.Render(proof);
                var encoder = new PngBitmapEncoder(); encoder.Frames.Add(BitmapFrame.Create(bitmap));
                Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(e.Args[1]))!);
                using (var output = File.Create(e.Args[1])) encoder.Save(output);
                proof.AllowClose = true;
                Shutdown(0); return;
            }

            instance = new Mutex(true, "Local\\CCSBar-Windows-v1", out ownsInstance);
            showRequest = new EventWaitHandle(false, EventResetMode.AutoReset, "Local\\CCSBar-Windows-show-v1");
            if (!ownsInstance) { showRequest.Set(); Shutdown(0); return; }
            var window = new MainWindow(); MainWindow = window;
            CreateTray(window);
            showListener = ThreadPool.RegisterWaitForSingleObject(showRequest,
                (_, _) => Dispatcher.BeginInvoke(new Action(async () => await window.OpenPopup())), null, Timeout.Infinite, false);
            if (window.IsConfigured) await window.Refresh(false);
            else await window.OpenPopup();
        }
        catch
        {
            // Never log exception bodies: connection credentials and account identities stay private.
            Directory.CreateDirectory(SecureStore.StateDirectory);
            File.WriteAllText(Path.Combine(SecureStore.StateDirectory, "startup-status.txt"), "AI Account Center startup failed. Check the connection or reinstall AI Account Center.");
            Shutdown(1);
        }
    }

    private void CreateTray(MainWindow window)
    {
        using var bitmap = new Bitmap(32, 32, System.Drawing.Imaging.PixelFormat.Format32bppArgb);
        using (var drawing = Graphics.FromImage(bitmap))
        {
            drawing.SmoothingMode = System.Drawing.Drawing2D.SmoothingMode.AntiAlias;
            using var lower = new SolidBrush(System.Drawing.Color.FromArgb(52, 152, 255));
            using var middle = new SolidBrush(System.Drawing.Color.FromArgb(66, 168, 255));
            using var upper = new SolidBrush(System.Drawing.Color.FromArgb(97, 184, 255));
            drawing.FillPolygon(lower, new[] { new System.Drawing.Point(1, 20), new(16, 28), new(31, 20), new(31, 24), new(16, 32), new(1, 24) });
            drawing.FillPolygon(middle, new[] { new System.Drawing.Point(1, 11), new(16, 19), new(31, 11), new(31, 16), new(16, 24), new(1, 16) });
            drawing.FillPolygon(upper, new[] { new System.Drawing.Point(1, 8), new(16, 0), new(31, 8), new(16, 16) });
        }
        var handle = bitmap.GetHicon();
        try { using var borrowed = Icon.FromHandle(handle); icon = (Icon)borrowed.Clone(); }
        finally { NativeIcon.DestroyIcon(handle); }
        tray = new Forms.NotifyIcon { Text = "AI Account Center · accounts", Icon = icon, Visible = true };
        tray.MouseClick += async (_, e) =>
        {
            if (e.Button == Forms.MouseButtons.Left)
                await Dispatcher.InvokeAsync(async () => await window.OpenPopup()).Task.Unwrap();
        };
        var menu = new Forms.ContextMenuStrip();
        menu.Items.Add("Open accounts", null, async (_, _) => await Dispatcher.InvokeAsync(async () => await window.OpenPopup()).Task.Unwrap());
        menu.Items.Add(new Forms.ToolStripSeparator());
        menu.Items.Add("Quit AI Account Center", null, (_, _) => Dispatcher.Invoke(Quit));
        tray.ContextMenuStrip = menu;
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
        if (tray is not null) { tray.Visible = false; tray.Dispose(); }
        icon?.Dispose(); showListener?.Unregister(null); showRequest?.Dispose();
        if (ownsInstance) instance?.ReleaseMutex(); instance?.Dispose();
        base.OnExit(e);
    }

    private static class NativeIcon
    {
        [System.Runtime.InteropServices.DllImport("user32.dll")] [return: System.Runtime.InteropServices.MarshalAs(System.Runtime.InteropServices.UnmanagedType.Bool)]
        public static extern bool DestroyIcon(IntPtr icon);
    }
}
