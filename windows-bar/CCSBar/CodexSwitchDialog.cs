using System;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Threading;

namespace CCSBar;

public sealed class CodexSwitchDialog : Window
{
    public CodexSwitchDialog(CodexSwitchConfirmation confirmation, string targetIdentity)
    {
        Title = "Switch Codex account";
        Width = 570; SizeToContent = SizeToContent.Height;
        MaxHeight = Math.Max(300, SystemParameters.WorkArea.Height - 48);
        WindowStartupLocation = WindowStartupLocation.CenterOwner;
        ResizeMode = ResizeMode.NoResize; ShowInTaskbar = false;
        Background = new SolidColorBrush(Color.FromRgb(11, 23, 40));
        Foreground = Brushes.WhiteSmoke;
        var grid = new Grid { Margin = new Thickness(20) };
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        grid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        var heading = new StackPanel();
        heading.Children.Add(Label("Switch to " + targetIdentity, 17, true));
        heading.Children.Add(Label("This will stop the programs below, switch the Codex account, then restart them. Active work may be interrupted and may need to be resumed manually.", 12));
        if (!string.IsNullOrWhiteSpace(confirmation.Warning)) heading.Children.Add(Label(confirmation.Warning, 12));
        grid.Children.Add(heading);
        var programs = new StackPanel { Margin = new Thickness(0, 12, 0, 12) };
        foreach (var process in confirmation.Processes)
            programs.Children.Add(Label($"{process.Label}  ·  PID {process.Pid}" + (string.IsNullOrWhiteSpace(process.Role) ? "" : "  ·  " + process.Role), 12));
        var scroll = new ScrollViewer { Content = programs, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, MaxHeight = Math.Max(100, SystemParameters.WorkArea.Height - 330) };
        Grid.SetRow(scroll, 1); grid.Children.Add(scroll);
        var footer = new StackPanel();
        var expiration = Label("", 11); footer.Children.Add(expiration);
        var actions = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right, Margin = new Thickness(0, 12, 0, 0) };
        var cancel = new Button { Content = "Cancel", IsCancel = true, Margin = new Thickness(0, 0, 10, 0) };
        var yes = new Button { Content = "Yes — Stop, Switch, Restart", Background = new SolidColorBrush(Color.FromRgb(25, 121, 210)) };
        cancel.Click += (_, _) => { DialogResult = false; };
        yes.Click += (_, _) => { DialogResult = true; };
        actions.Children.Add(cancel); actions.Children.Add(yes); footer.Children.Add(actions);
        Grid.SetRow(footer, 2); grid.Children.Add(footer); Content = grid;
        void UpdateExpiration()
        {
            expiration.Text = confirmation.Expired ? "Confirmation expired. Cancel, then Activate again to review the current programs." : "Only the listed programs are covered by this confirmation.";
            yes.Content = confirmation.Expired ? "Expired — Activate again" : "Yes — Stop, Switch, Restart";
            yes.IsEnabled = !confirmation.Expired;
        }
        var timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(1) };
        timer.Tick += (_, _) => UpdateExpiration(); UpdateExpiration(); timer.Start();
        Loaded += (_, _) => cancel.Focus(); Closed += (_, _) => timer.Stop();
    }

    private static TextBlock Label(string text, double size, bool bold = false) => new()
    {
        Text = text, FontSize = size, TextWrapping = TextWrapping.Wrap,
        FontWeight = bold ? FontWeights.SemiBold : FontWeights.Normal, Margin = new Thickness(0, 0, 0, 8)
    };
}
