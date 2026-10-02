using System;
using System.Collections.Generic;
using System.Globalization;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Shapes;

namespace CCSBar;

/// <summary>
/// One UI icon family: Lucide geometry on a 24 grid, stroke 1.75, round caps and joins, drawn as vector paths.
/// Filled platform glyphs (Apple, Windows) and the Apex Soft logo are vector paths too. No glyph-font icons.
/// </summary>
public static class Icons
{
    private static string Circle(double cx, double cy, double r) => string.Create(CultureInfo.InvariantCulture,
        $"M{cx - r},{cy} A{r},{r} 0 1 0 {cx + r},{cy} A{r},{r} 0 1 0 {cx - r},{cy} Z ");
    private static string Rect(double x, double y, double w, double h, double r) => string.Create(CultureInfo.InvariantCulture,
        $"M{x + r},{y} H{x + w - r} A{r},{r} 0 0 1 {x + w},{y + r} V{y + h - r} A{r},{r} 0 0 1 {x + w - r},{y + h} H{x + r} A{r},{r} 0 0 1 {x},{y + h - r} V{y + r} A{r},{r} 0 0 1 {x + r},{y} Z ");

    public static readonly Dictionary<string, string> Lucide = new()
    {
        ["refresh"] = "M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8 M21 3v5h-5 M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16 M8 16H3v5",
        ["settings"] = "M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z " + Circle(12, 12, 3),
        ["x"] = "M18 6 6 18 M6 6l12 12",
        ["info"] = Circle(12, 12, 10) + "M12 16v-4 M12 8h.01",
        ["check"] = "M20 6 9 17l-5-5",
        ["chevDown"] = "M6 9 l6 6 6-6",
        ["chevRight"] = "M9 18 l6-6-6-6",
        ["clock"] = Circle(12, 12, 10) + "M12 6v6l4 2",
        ["external"] = "M15 3h6v6 M10 14 21 3 M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6",
        ["sun"] = Circle(12, 12, 4) + "M12 2v2 M12 20v2 M4.93 4.93 l1.41 1.41 M17.66 17.66 l1.41 1.41 M2 12h2 M20 12h2 M6.34 17.66 l-1.41 1.41 M19.07 4.93 l-1.41 1.41",
        ["moon"] = "M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z",
        ["monitor"] = Rect(2, 3, 20, 14, 2) + "M8 21h8 M12 17v4",
        ["panel"] = Rect(3, 3, 18, 18, 2) + "M3 9h18",
        ["pack"] = "M11 21.73a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73z M12 22V12 M3.3 7 l7.703 4.734a2 2 0 0 0 1.994 0L20.7 7",
        ["power"] = "M12 2v10 M18.4 6.6a9 9 0 1 1-12.77.04",
        ["fileText"] = "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z M14 2v4a2 2 0 0 0 2 2h4 M10 9H8 M16 13H8 M16 17H8",
        ["shield"] = "M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z",
        ["keyboard"] = "M10 8h.01 M12 12h.01 M14 8h.01 M16 12h.01 M18 8h.01 M6 8h.01 M7 16h10 M8 12h.01 " + Rect(2, 4, 20, 16, 2),
        ["alertCircle"] = Circle(12, 12, 10) + "M12 8v4 M12 16h.01",
        ["link"] = "M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71 M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71",
        ["globe"] = Circle(12, 12, 10) + "M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20 M2 12h20",
        ["wallet"] = "M19 7V4a1 1 0 0 0-1-1H5a2 2 0 0 0 0 4h15a1 1 0 0 1 1 1v4h-3a2 2 0 0 0 0 4h3a1 1 0 0 0 1-1v-2a1 1 0 0 0-1-1 M3 5v14a2 2 0 0 0 2 2h15a1 1 0 0 0 1-1v-4",
        ["loader"] = "M21 12a9 9 0 1 1-6.219-8.56",
        ["eye"] = "M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0 " + Circle(12, 12, 3),
    };

    /// <summary>Filled platform glyphs (Simple Icons, CC0) from marks-v2, with their optical boxes.</summary>
    public static readonly Dictionary<string, (string Path, Rect Box)> Platform = new()
    {
        ["mac"] = ("M12.152 6.896c-.948 0-2.415-1.078-3.96-1.04-2.04.027-3.91 1.183-4.961 3.014-2.117 3.675-.546 9.103 1.519 12.09 1.013 1.454 2.208 3.09 3.792 3.039 1.52-.065 2.09-.987 3.935-.987 1.831 0 2.35.987 3.96.948 1.637-.026 2.676-1.48 3.676-2.948 1.156-1.688 1.636-3.325 1.662-3.415-.039-.013-3.182-1.221-3.22-4.857-.026-3.04 2.48-4.494 2.597-4.559-1.429-2.09-3.623-2.324-4.39-2.376-2-.156-3.675 1.09-4.61 1.09zM15.53 3.83c.843-1.012 1.4-2.427 1.245-3.83-1.207.052-2.662.805-3.532 1.818-.78.896-1.454 2.338-1.273 3.714 1.338.104 2.715-.688 3.559-1.701", new Rect(2.04, -0.18, 19.92, 24.36)),
        ["windows"] = ("M0,0H11.377V11.372H0ZM12.623,0H24V11.372H12.623ZM0,12.623H11.377V24H0Zm12.623,0H24V24H12.623", new Rect(-0.18, -0.18, 24.36, 24.36)),
    };

    // Apex Soft (logos/svg/apex-soft-color-24.svg): ink body plus the blue meter end.
    public const string ApexInk = "M2.617 22A1.008 1.008 0 0 1 1.713 20.548L10.474 2.701A1.7 1.7 0 0 1 13.526 2.701L22.287 20.548A1.008 1.008 0 0 1 21.383 22L20.074 22A0.6 0.6 0 0 1 19.535 21.664L12 6.314L4.465 21.664A0.6 0.6 0 0 1 3.926 22Z";
    public const string ApexMeter = "M6.577 14L11.5 14A1.5 1.5 0 0 1 11.5 17L5.104 17Z";

    private static readonly Dictionary<string, Geometry> cache = new();

    public static Geometry Geometry(string data)
    {
        if (cache.TryGetValue(data, out var geometry)) return geometry;
        geometry = System.Windows.Media.Geometry.Parse(data);
        geometry.Freeze();
        cache[data] = geometry;
        return geometry;
    }

    /// <summary>A Lucide icon at the given size, stroked with the given brush.</summary>
    public static FrameworkElement Icon(string name, double size, Brush stroke, double strokeWidth = 1.75)
    {
        var path = new Path
        {
            Data = Geometry(Lucide.TryGetValue(name, out var data) ? data : Lucide["info"]), Stroke = stroke, StrokeThickness = strokeWidth,
            StrokeStartLineCap = PenLineCap.Round, StrokeEndLineCap = PenLineCap.Round, StrokeLineJoin = PenLineJoin.Round, Width = 24, Height = 24,
        };
        return new Viewbox { Width = size, Height = size, Child = new Canvas { Width = 24, Height = 24, Children = { path } }, SnapsToDevicePixels = false };
    }

    public static FrameworkElement PlatformGlyph(string platform, double size, Brush fill)
    {
        var (data, box) = Platform[platform];
        var path = new Path { Data = Geometry(data), Fill = fill };
        var canvas = new Canvas { Width = box.Width, Height = box.Height, Children = { path } };
        Canvas.SetLeft(path, -box.X); Canvas.SetTop(path, -box.Y);
        var scale = platform == "mac" ? 0.9 : 0.82;
        return new Viewbox { Width = size * scale * Math.Min(1, box.Width / box.Height), Height = size * scale, Child = canvas, Stretch = Stretch.Uniform };
    }

    /// <summary>The Apex Soft colour mark; the body takes the ink brush, the meter end keeps its blue gradient.</summary>
    public static FrameworkElement Logo(double size, Brush ink)
    {
        var gradient = new LinearGradientBrush { MappingMode = BrushMappingMode.Absolute, StartPoint = new Point(5.104, 15.5), EndPoint = new Point(13, 15.5) };
        gradient.GradientStops.Add(new GradientStop((Color)ColorConverter.ConvertFromString("#3466FA"), 0));
        gradient.GradientStops.Add(new GradientStop((Color)ColorConverter.ConvertFromString("#3466FA"), 0.302));
        gradient.GradientStops.Add(new GradientStop((Color)ColorConverter.ConvertFromString("#2C96FF"), 1));
        gradient.Freeze();
        var canvas = new Canvas { Width = 24, Height = 24 };
        canvas.Children.Add(new Path { Data = Geometry(ApexMeter), Fill = gradient });
        canvas.Children.Add(new Path { Data = Geometry(ApexInk), Fill = ink });
        return new Viewbox { Width = size, Height = size, Child = canvas };
    }

    /// <summary>Renders an icon into a System.Drawing bitmap for the WinForms tray menu.</summary>
    public static System.Drawing.Bitmap MenuBitmap(string name, int pixels, Color stroke)
    {
        var brush = new SolidColorBrush(stroke); brush.Freeze();
        FrameworkElement element = name == "logo" ? Logo(pixels, brush) : Icon(name, pixels, brush, 1.75);
        element.Measure(new Size(pixels, pixels)); element.Arrange(new Rect(0, 0, pixels, pixels)); element.UpdateLayout();
        var bitmap = new RenderTargetBitmap(pixels, pixels, 96, 96, PixelFormats.Pbgra32);
        bitmap.Render(element);
        var encoder = new PngBitmapEncoder(); encoder.Frames.Add(BitmapFrame.Create(bitmap));
        using var stream = new System.IO.MemoryStream(); encoder.Save(stream); stream.Position = 0;
        using var decoded = new System.Drawing.Bitmap(stream);
        return new System.Drawing.Bitmap(decoded);
    }
}
