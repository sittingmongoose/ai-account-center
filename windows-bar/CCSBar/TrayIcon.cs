using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.IO;
using System.Runtime.InteropServices;
using Forms = System.Windows.Forms;

namespace CCSBar;

/// <summary>
/// The notification-area icon (Apex Soft, light or dark taskbar art chosen by SystemUsesLightTheme), its tooltip
/// (usage %), and the restyled right-click menu: a ContextMenuStrip with a Daylight Atlas renderer, Lucide images and
/// Windows 11 rounded corners from DWM.
/// </summary>
public sealed class TrayIcon : IDisposable
{
    private readonly Forms.NotifyIcon notify;
    private readonly Forms.ContextMenuStrip menu;
    private Icon? icon;
    private bool lightTaskbar;
    private static Font? menuFont, menuBold, menuHead;

    public event Action? OpenRequested, ToggleRequested, DashboardRequested, RefreshRequested, SettingsRequested, QuitRequested;

    public TrayIcon()
    {
        menu = CreateStyledMenu();
        notify = new Forms.NotifyIcon { Text = "AI Account Center", Visible = false, ContextMenuStrip = menu };
        notify.MouseClick += (_, e) => { if (e.Button == Forms.MouseButtons.Left) ToggleRequested?.Invoke(); };
        menu.Opening += (_, _) => BuildMenu();
        menu.Opened += (_, _) => RoundCorners(menu.Handle);
        ApplyTheme();
        notify.Visible = true;
    }

    public static Icon LoadIcon(bool lightTaskbar, int size)
    {
        using var stream = IconStream(lightTaskbar);
        return new Icon(stream, new Size(size, size));
    }

    /// <summary>The plated Apex Soft app icon (scripts/build-app-icon.py): the exe, window and shortcut icon, legible on
    /// any background. The bare glyphs below are notification-area art only.</summary>
    public static Stream AppIconStream() => typeof(TrayIcon).Assembly.GetManifestResourceStream("CCSBar.Icons.AppIcon.ico") ?? throw new InvalidOperationException("App icon resource missing.");

    /// <summary>Apex Soft taskbar art: TrayLight.ico (dark ink) for a light taskbar, TrayDark.ico for a dark one.</summary>
    public static Stream IconStream(bool lightTaskbar)
    {
        var name = "CCSBar.Icons." + (lightTaskbar ? "TrayLight.ico" : "TrayDark.ico");
        return typeof(TrayIcon).Assembly.GetManifestResourceStream(name) ?? throw new InvalidOperationException("Tray icon resource missing.");
    }

    /// <summary>Picks taskbar art by the system (taskbar) mode and re-themes the menu for the app mode.</summary>
    public void ApplyTheme()
    {
        lightTaskbar = SystemTheme.SystemUsesLightTheme();
        var size = Forms.SystemInformation.SmallIconSize.Width;
        var next = LoadIcon(lightTaskbar, size);
        var previous = icon; icon = next; notify.Icon = next; previous?.Dispose();
        menu.Renderer = new AtlasMenuRenderer();
        menu.BackColor = ToDrawing("Card");
    }

    internal static bool MenuFontIsInstrumentSans { get { EnsureFonts(); return menuFont?.FontFamily.Name == "Instrument Sans"; } }

    public void SetTooltip(string text)
    {
        var next = text.Length <= 127 ? text : text[..127];
        if (notify.Text == next) return; // an unchanged tooltip never reaches the notification area (N6)
        notify.Text = next;
    }

    private void BuildMenu() => Populate(menu, OpenRequested, DashboardRequested, RefreshRequested, SettingsRequested, QuitRequested);

    /// <summary>A styled, empty tray menu (also used by the render checks).</summary>
    internal static Forms.ContextMenuStrip CreateStyledMenu() => new()
    {
        ShowImageMargin = true, Padding = new Forms.Padding(4), DropShadowEnabled = false, AutoSize = true,
        Renderer = new AtlasMenuRenderer(), BackColor = ToDrawing("Card"),
    };

    internal static void Populate(Forms.ContextMenuStrip menu, Action? open, Action? dashboard, Action? refresh, Action? settings, Action? quit)
    {
        // A rebuild replaces every item: dispose the previous GDI+ bitmaps instead of leaving them to the finalizer,
        // so repeated menu opens never grow GDI objects or native memory (N6).
        foreach (Forms.ToolStripItem item in menu.Items) item.Image?.Dispose();
        menu.Items.Clear();
        EnsureFonts();
        menu.Renderer = new AtlasMenuRenderer(); menu.BackColor = ToDrawing("Card");
        var ink3 = ToMedia("Ink3");
        // The header is a disabled item, so the logo sits in the icon column like the menu icons.
        var head = new Forms.ToolStripMenuItem("AI Account Center", Icons.MenuBitmap("logo", 16, ToMedia("Ink"))) { Font = menuHead, Enabled = false, Padding = new Forms.Padding(2, 5, 2, 3) };
        menu.Items.Add(head);
        void Item(string text, string iconName, Action? action, bool bold = false)
        {
            var item = new Forms.ToolStripMenuItem(text, Icons.MenuBitmap(iconName, 16, ink3)) { Font = bold ? menuBold : menuFont, ForeColor = ToDrawing("Ink"), Padding = new Forms.Padding(2, 7, 2, 7) };
            item.Click += (_, _) => action?.Invoke();
            menu.Items.Add(item);
        }
        Item("Open accounts", "panel", open, bold: true);
        Item("Open dashboard", "external", dashboard);
        Item("Refresh now", "refresh", refresh);
        Item("Settings", "settings", settings);
        menu.Items.Add(new Forms.ToolStripSeparator());
        Item("Quit AI Account Center", "power", quit);
    }

    private static void EnsureFonts()
    {
        if (menuFont is not null) return;
        var family = PrivateFonts.Family;
        menuFont = new Font(family, 9.5f, FontStyle.Regular, GraphicsUnit.Point);
        menuBold = new Font(family, 9.5f, FontStyle.Bold, GraphicsUnit.Point);
        menuHead = new Font(family, 9f, FontStyle.Regular, GraphicsUnit.Point);
    }

    internal static Color ToDrawing(string key) { var c = Theme.Color(key, Theme.IsDark); return Color.FromArgb(c.A, c.R, c.G, c.B); }
    internal static System.Windows.Media.Color ToMedia(string key) => Theme.Color(key, Theme.IsDark);

    private static void RoundCorners(IntPtr handle)
    {
        try { int preference = 2; DwmSetWindowAttribute(handle, 33, ref preference, sizeof(int)); } catch { /* Windows 10: square corners */ }
    }

    [DllImport("dwmapi.dll")] private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attribute, ref int value, int size);

    public void Dispose()
    {
        notify.Visible = false; notify.Dispose(); menu.Dispose(); icon?.Dispose();
    }

    private sealed class AtlasMenuRenderer : Forms.ToolStripProfessionalRenderer
    {
        public AtlasMenuRenderer() : base(new AtlasColors()) { RoundedEdges = false; }

        protected override void OnRenderToolStripBackground(Forms.ToolStripRenderEventArgs e)
        {
            using var brush = new SolidBrush(ToDrawing("Card"));
            e.Graphics.FillRectangle(brush, e.AffectedBounds);
        }

        protected override void OnRenderToolStripBorder(Forms.ToolStripRenderEventArgs e)
        {
            using var pen = new Pen(ToDrawing("Rule"));
            e.Graphics.DrawRectangle(pen, 0, 0, e.ToolStrip.Width - 1, e.ToolStrip.Height - 1);
        }

        protected override void OnRenderImageMargin(Forms.ToolStripRenderEventArgs e) { }

        protected override void OnRenderMenuItemBackground(Forms.ToolStripItemRenderEventArgs e)
        {
            if (!e.Item.Selected || !e.Item.Enabled) return;
            var bounds = new Rectangle(2, 1, e.Item.Width - 4, e.Item.Height - 2);
            e.Graphics.SmoothingMode = SmoothingMode.AntiAlias;
            using var path = Rounded(bounds, 4);
            using var brush = new SolidBrush(ToDrawing("Card3"));
            e.Graphics.FillPath(brush, path);
        }

        protected override void OnRenderSeparator(Forms.ToolStripSeparatorRenderEventArgs e)
        {
            using var pen = new Pen(ToDrawing("Rule2"));
            var y = e.Item.Height / 2;
            e.Graphics.DrawLine(pen, 6, y, e.Item.Width - 6, y);
        }

        // Text and image share one vertical centre: both are placed against the full item height.
        protected override void OnRenderItemText(Forms.ToolStripItemTextRenderEventArgs e)
        {
            var bounds = new Rectangle(e.TextRectangle.X, 0, e.Item.Width - e.TextRectangle.X, e.Item.Height);
            Forms.TextRenderer.DrawText(e.Graphics, e.Text, e.TextFont, bounds, e.Item.Enabled ? ToDrawing("Ink") : ToDrawing("Ink3"),
                Forms.TextFormatFlags.VerticalCenter | Forms.TextFormatFlags.Left | Forms.TextFormatFlags.SingleLine | Forms.TextFormatFlags.NoPrefix | Forms.TextFormatFlags.NoPadding);
        }

        protected override void OnRenderItemImage(Forms.ToolStripItemImageRenderEventArgs e)
        {
            if (e.Image is null) return;
            var size = e.ImageRectangle.Size;
            e.Graphics.DrawImage(e.Image, new Rectangle(e.ImageRectangle.X, (e.Item.Height - size.Height) / 2, size.Width, size.Height));
        }

        private static GraphicsPath Rounded(Rectangle r, int radius)
        {
            var path = new GraphicsPath(); var d = radius * 2;
            path.AddArc(r.X, r.Y, d, d, 180, 90); path.AddArc(r.Right - d, r.Y, d, d, 270, 90);
            path.AddArc(r.Right - d, r.Bottom - d, d, d, 0, 90); path.AddArc(r.X, r.Bottom - d, d, d, 90, 90);
            path.CloseFigure(); return path;
        }
    }

    private sealed class AtlasColors : Forms.ProfessionalColorTable
    {
        public override Color ToolStripDropDownBackground => ToDrawing("Card");
        public override Color MenuBorder => ToDrawing("Rule");
        public override Color MenuItemBorder => Color.Transparent;
        public override Color MenuItemSelected => ToDrawing("Card3");
        public override Color ImageMarginGradientBegin => ToDrawing("Card");
        public override Color ImageMarginGradientMiddle => ToDrawing("Card");
        public override Color ImageMarginGradientEnd => ToDrawing("Card");
        public override Color SeparatorDark => ToDrawing("Rule2");
        public override Color SeparatorLight => ToDrawing("Rule2");
    }
}

/// <summary>Instrument Sans for GDI (WinForms menu): the embedded TTFs registered as private memory fonts.</summary>
public static class PrivateFonts
{
    private static System.Drawing.Text.PrivateFontCollection? collection;

    public static FontFamily Family
    {
        get
        {
            try
            {
                if (collection is null)
                {
                    collection = new System.Drawing.Text.PrivateFontCollection();
                    foreach (var face in new[] { "InstrumentSans-Regular.ttf", "InstrumentSans-SemiBold.ttf", "InstrumentSans-Bold.ttf" })
                    {
                        using var stream = System.Windows.Application.GetResourceStream(new Uri($"pack://application:,,,/CCSBar;component/Resources/Fonts/{face}"))!.Stream;
                        using var memory = new MemoryStream(); stream.CopyTo(memory);
                        var bytes = memory.ToArray();
                        var pointer = Marshal.AllocCoTaskMem(bytes.Length);
                        Marshal.Copy(bytes, 0, pointer, bytes.Length);
                        collection.AddMemoryFont(pointer, bytes.Length);
                        // GDI text (TextRenderer) needs the face registered with GDI as well.
                        uint installed = 0; AddFontMemResourceEx(pointer, (uint)bytes.Length, IntPtr.Zero, ref installed);
                    }
                }
                foreach (var family in collection.Families) if (family.Name == "Instrument Sans") return family;
            }
            catch { }
            return SystemFonts.MenuFont?.FontFamily ?? FontFamily.GenericSansSerif;
        }
    }

    [DllImport("gdi32.dll")] private static extern IntPtr AddFontMemResourceEx(IntPtr font, uint length, IntPtr reserved, ref uint fonts);
}
