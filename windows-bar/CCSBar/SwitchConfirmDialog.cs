using System;
using System.Collections.Generic;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Threading;

namespace CCSBar;

/// <summary>
/// Native confirmation before a switch stops running programs (Codex and Antigravity). Daylight Atlas surface, the
/// tray font, and the same rule as before: only the confirm button sends the server's short-lived token.
/// </summary>
public sealed class SwitchConfirmDialog : Window
{
    public SwitchConfirmDialog(string provider, string targetIdentity, string warning, IReadOnlyList<CodexSwitchProcess> processes, Func<bool> expired)
    {
        Title = "Switch " + provider + " account";
        Width = 560; SizeToContent = SizeToContent.Height;
        MaxHeight = Math.Max(300, SystemParameters.WorkArea.Height - 48);
        WindowStartupLocation = WindowStartupLocation.CenterOwner;
        ResizeMode = ResizeMode.NoResize; ShowInTaskbar = false; WindowStyle = WindowStyle.ToolWindow;
        Background = Theme.Brush("Panel");
        FontFamily = Theme.Sans;
        var card = new Border { Background = Theme.Brush("Card"), BorderBrush = Theme.Brush("WarnLine"), BorderThickness = new Thickness(1), CornerRadius = new CornerRadius(8), Margin = new Thickness(14), Padding = new Thickness(16, 14, 16, 14) };
        var grid = new Grid();
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        grid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        var heading = new StackPanel();
        var title = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 0, 0, 8) };
        title.Children.Add(Icons.Icon("alertCircle", 16, Theme.Brush("WarnText")));
        title.Children.Add(Label("Switch to " + targetIdentity, 14, "Ink", true, new Thickness(8, 0, 0, 0)));
        heading.Children.Add(title);
        heading.Children.Add(Label("This stops the programs below, switches the " + provider + " account, then restarts them. Active work may be interrupted and may need to be resumed by hand.", 12.5, "Ink2"));
        if (!string.IsNullOrWhiteSpace(warning)) heading.Children.Add(Label(warning, 12.5, "Ink2"));
        grid.Children.Add(heading);
        var programs = new StackPanel();
        foreach (var process in processes)
        {
            var row = new Grid { Margin = new Thickness(0, 2, 0, 2) };
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(110) });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(90) });
            row.Children.Add(Label(process.Label, 12.5, "Ink", false, new Thickness(0)));
            var role = Label(process.Role, 12, "Ink3", false, new Thickness(0)); Grid.SetColumn(role, 1); row.Children.Add(role);
            var pid = Label("PID " + process.Pid, 12, "Ink3", false, new Thickness(0)); Grid.SetColumn(pid, 2); row.Children.Add(pid);
            programs.Children.Add(row);
        }
        var list = new Border { Background = Theme.Brush("Card2"), BorderBrush = Theme.Brush("Rule2"), BorderThickness = new Thickness(1), CornerRadius = new CornerRadius(6), Padding = new Thickness(10, 6, 10, 6), Margin = new Thickness(0, 4, 0, 10),
            Child = new ScrollViewer { Content = programs, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, MaxHeight = Math.Max(100, SystemParameters.WorkArea.Height - 330) } };
        Grid.SetRow(list, 1); grid.Children.Add(list);
        var footer = new StackPanel();
        var expiration = Label("", 11.5, "Ink3"); footer.Children.Add(expiration);
        var actions = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right, Margin = new Thickness(0, 6, 0, 0) };
        var cancel = new Button { Content = "Cancel", IsCancel = true, Margin = new Thickness(0, 0, 8, 0), Style = (Style)System.Windows.Application.Current.FindResource("AtlasButton") };
        var yes = new Button { Content = "Stop, switch, restart", Style = (Style)System.Windows.Application.Current.FindResource("AtlasPrimaryButton") };
        cancel.Click += (_, _) => { DialogResult = false; };
        yes.Click += (_, _) => { DialogResult = true; };
        actions.Children.Add(cancel); actions.Children.Add(yes); footer.Children.Add(actions);
        Grid.SetRow(footer, 2); grid.Children.Add(footer);
        card.Child = grid; Content = card;
        void UpdateExpiration()
        {
            var gone = expired();
            expiration.Text = gone ? "This confirmation expired. Cancel, then Activate again to review the current programs." : "Only the listed programs are covered by this confirmation.";
            yes.Content = gone ? "Expired: activate again" : "Stop, switch, restart";
            yes.IsEnabled = !gone;
        }
        var timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(1) };
        timer.Tick += (_, _) => UpdateExpiration(); UpdateExpiration(); timer.Start();
        Loaded += (_, _) => cancel.Focus(); Closed += (_, _) => timer.Stop();
    }

    private static TextBlock Label(string text, double size, string ink, bool bold = false, Thickness? margin = null) => new()
    {
        Text = text, FontSize = size, TextWrapping = TextWrapping.Wrap, Foreground = Theme.Brush(ink), FontFamily = Theme.Sans,
        FontWeight = bold ? FontWeights.SemiBold : FontWeights.Normal, Margin = margin ?? new Thickness(0, 0, 0, 8)
    };
}
