using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using ReinRemote.Host.Core;
using ReinRemote.Host.Pages;

namespace ReinRemote.Host;

public sealed partial class MainWindow : Window
{
    bool closing;

    static readonly Dictionary<string, Type> pages = new()
    {
        ["Sharing"] = typeof(SharingPage),
        ["Viewers"] = typeof(ViewersPage),
        ["Devices"] = typeof(DevicesPage),
        ["Settings"] = typeof(SettingsPage),
        ["SignIn"] = typeof(SignInPage),
        ["Policy"] = typeof(PolicyPage),
        ["Sessions"] = typeof(SessionsPage),
        ["Audit"] = typeof(AuditLogPage),
        ["Metrics"] = typeof(MetricsPage),
    };

    public MainWindow()
    {
        InitializeComponent();
        // The system's light or dark theme, unless REIN_REMOTE_THEME says (for screenshots).
        Root.RequestedTheme = Environment.GetEnvironmentVariable("REIN_REMOTE_THEME") switch
        {
            "light" => ElementTheme.Light,
            "dark" => ElementTheme.Dark,
            _ => ElementTheme.Default,
        };
        try
        {
            AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "rein-remote.ico"));
        }
        catch (Exception e)
        {
            App.Log($"icon: {e.Message}");
        }
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);
        ThemeTitleBar();
        Root.ActualThemeChanged += (_, _) => ThemeTitleBar();
        FitToScreen();
        // Closing the window keeps the app in the notification area; Quit (its menu) ends it.
        AppWindow.Closing += (_, e) =>
        {
            if (!closing && App.HasTray)
            {
                e.Cancel = true;
                AppWindow.Hide();
            }
        };
        App.Model.StatusChanged += ShowStatus;
        App.Model.StateChanged += ShowStatus;
        App.Model.ViewersChanged += ShowViewers;
        ShowStatus();
        Nav.SelectedItem = Nav.MenuItems[0];
    }

    /// <summary>The caption buttons drawn in the theme's colors.</summary>
    void ThemeTitleBar()
    {
        var dark = Root.ActualTheme == ElementTheme.Dark;
        var bar = AppWindow.TitleBar;
        bar.ButtonBackgroundColor = Colors.Transparent;
        bar.ButtonInactiveBackgroundColor = Colors.Transparent;
        bar.ButtonForegroundColor = dark ? Colors.White : Colors.Black;
        bar.ButtonHoverForegroundColor = dark ? Colors.White : Colors.Black;
        bar.ButtonHoverBackgroundColor = dark ? ColorHelper.FromArgb(0x20, 0xff, 0xff, 0xff) : ColorHelper.FromArgb(0x20, 0, 0, 0);
        bar.ButtonInactiveForegroundColor = Colors.Gray;
    }

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    static extern uint GetDpiForWindow(IntPtr hwnd);

    /// <summary>1100 × 780 (at this display's scale), smaller when the screen is, centered.</summary>
    void FitToScreen()
    {
        var area = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Primary).WorkArea;
        var dpi = GetDpiForWindow(WinRT.Interop.WindowNative.GetWindowHandle(this));
        var scale = dpi == 0 ? 1.0 : dpi / 96.0;
        int w = Math.Min((int)(1100 * scale), area.Width), h = Math.Min((int)(780 * scale), area.Height);
        AppWindow.MoveAndResize(new Windows.Graphics.RectInt32(area.X + (area.Width - w) / 2, area.Y + (area.Height - h) / 2, w, h));
    }

    public void ShowAndFocus()
    {
        AppWindow.Show();
        Activate();
    }

    public void CloseForGood()
    {
        closing = true;
        Close();
    }

    public void Navigate(string tag)
    {
        foreach (var item in Nav.MenuItems.OfType<NavigationViewItem>())
            if (item.Tag as string == tag)
                Nav.SelectedItem = item;
    }

    void Nav_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        if (args.SelectedItem is NavigationViewItem { Tag: string tag } && pages.TryGetValue(tag, out var type) && ContentFrame.CurrentSourcePageType != type)
            ContentFrame.Navigate(type);
    }

    void ShowStatus()
    {
        var m = App.Model;
        switch (m.Status)
        {
            case LinkStatus.Connected:
                if (m.State?.Error is { Length: > 0 } error && m.State.Sharing == false)
                {
                    ConnectionBar.Severity = InfoBarSeverity.Error;
                    ConnectionBar.Title = "Sharing didn't start";
                    ConnectionBar.Message = error;
                    ConnectionBar.IsOpen = true;
                }
                else
                {
                    ConnectionBar.IsOpen = false;
                }
                break;
            case LinkStatus.Starting:
                ConnectionBar.Severity = InfoBarSeverity.Informational;
                ConnectionBar.Title = "Starting";
                ConnectionBar.Message = m.StatusMessage ?? "Starting rein-remote host…";
                ConnectionBar.IsOpen = true;
                break;
            case LinkStatus.Connecting:
                ConnectionBar.Severity = InfoBarSeverity.Informational;
                ConnectionBar.Title = "Connecting";
                ConnectionBar.Message = m.StatusMessage ?? "Looking for rein-remote host…";
                ConnectionBar.IsOpen = true;
                break;
            default:
                ConnectionBar.Severity = InfoBarSeverity.Error;
                ConnectionBar.Title = "Not connected";
                ConnectionBar.Message = m.StatusMessage ?? "rein-remote host isn't running.";
                ConnectionBar.IsOpen = true;
                break;
        }
        Title = "Rein Remote: " + TrayIcon.StatusText(m);
        TitleText.Text = Title;
    }

    void ShowViewers()
    {
        var n = App.Model.Viewers.Viewers.Count;
        ViewersBadge.Value = n;
        ViewersBadge.Visibility = n > 0 ? Visibility.Visible : Visibility.Collapsed;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(NavViewers, n > 0 ? $"Viewers ({n})" : "Viewers");
    }
}
