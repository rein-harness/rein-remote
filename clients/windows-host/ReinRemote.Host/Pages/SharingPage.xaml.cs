using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using ReinRemote.Host.Core;

namespace ReinRemote.Host.Pages;

/// <summary>Sharing on or off, what's shared, the links with their QR code, and pairing a device.</summary>
public sealed partial class SharingPage : Page
{
    HostModel M => App.Model;
    readonly DispatcherTimer tick = new() { Interval = TimeSpan.FromSeconds(1) };
    List<SourceItem> sources = new();
    bool settingSource;
    bool busy;
    string linksShown = "";
    string? qrShown, pairQrShown;

    public SharingPage()
    {
        InitializeComponent();
        tick.Tick += (_, _) => ShowCountdown();
        Loaded += (_, _) =>
        {
            M.StateChanged += Show;
            M.StatusChanged += Show;
            M.Connected += OnConnected;
            tick.Start();
            Show();
            if (sources.Count == 0)
                _ = LoadSources();
        };
        Unloaded += (_, _) =>
        {
            M.StateChanged -= Show;
            M.StatusChanged -= Show;
            M.Connected -= OnConnected;
            tick.Stop();
        };
    }

    void OnConnected() => _ = LoadSources();

    void Show()
    {
        var s = M.State;
        var connected = M.IsConnected && s is not null;
        StartStopButton.IsEnabled = connected && !busy;
        NewCodeButton.IsEnabled = connected && s!.Sharing && s.Pairing.Enabled;
        CopyCodeButton.IsEnabled = connected && s!.Pairing.Code is not null;
        SourcePicker.IsEnabled = connected;
        if (!connected)
        {
            SharingStatus.Text = M.Status == LinkStatus.Connected ? "Connecting…" : "Not connected";
            SharingDetail.Text = M.StatusMessage ?? "";
            StatusDot.Fill = (Brush)Application.Current.Resources["ControlStrongFillColorDisabledBrush"];
            SetStartStop(false);
            return;
        }
        SharingStatus.Text = s!.Sharing ? "Sharing" : "Not sharing";
        StatusDot.Fill = (Brush)Application.Current.Resources[s.Sharing ? "SystemFillColorSuccessBrush" : "ControlStrongFillColorDisabledBrush"];
        SharingDetail.Text = s.Sharing
            ? $"{(s.Viewers == 1 ? "1 viewer" : $"{s.Viewers} viewers")} · viewers connect on {s.Listen}"
            : s.Error is { Length: > 0 } e ? $"Didn't start: {e}" : "No one can connect until you start sharing.";
        SetStartStop(s.Sharing);
        RestartButton.Visibility = s.Sharing && s.RestartNeeded ? Visibility.Visible : Visibility.Collapsed;

        // The source
        CurrentSource.Text = $"Now: {Describe(s.Source)}";
        SelectSource(s.Source);

        // The links
        var key = string.Join("\n", s.Links.Select(l => l.Url));
        if (key != linksShown)
        {
            linksShown = key;
            Links.ItemsSource = s.Links;
            if (s.Links.Count > 0)
                Links.SelectedIndex = 0;
        }
        NoLinks.Visibility = s.Links.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        Links.Visibility = s.Links.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
        if (s.Links.Count == 0)
            LinkQrPanel.Visibility = Visibility.Collapsed;

        // Pairing
        var p = s.Pairing;
        PairingCode.Text = p.Code ?? "–";
        AutomationProperties.SetName(PairingCode, p.Code is null ? "No pairing code" : $"Pairing code {p.Code}");
        PairingNote.Text = !s.Sharing ? "Pairing needs sharing on."
            : !p.Enabled ? "Pairing is off (Settings, Pairing)."
            : p.Code is null ? "No open code: make a new one." : "";
        Fingerprint.Text = string.IsNullOrEmpty(p.Fingerprint) ? "" : $"This machine: {p.Fingerprint}";
        _ = ShowPairingQr(p.Code is null ? null : p.Qr);
        ShowCountdown();
    }

    void SetStartStop(bool sharing)
    {
        StartStopButton.Content = sharing ? "Stop sharing" : "Start sharing";
        AutomationProperties.SetName(StartStopButton, sharing ? "Stop sharing" : "Start sharing");
        StartStopButton.Style = (Style)Application.Current.Resources[sharing ? "DefaultButtonStyle" : "AccentButtonStyle"];
    }

    void ShowCountdown()
    {
        var p = M.State?.Pairing;
        var left = p?.Code is null ? "" : Format.Countdown(p.ExpiresAt, DateTimeOffset.UtcNow);
        PairingCountdown.Text = left switch { "" => "", "expired" => "expired", _ => $"{left} left" };
    }

    async Task ShowPairingQr(string? text)
    {
        if (text == pairQrShown)
            return;
        pairQrShown = text;
        PairingQr.Source = await Ui.Qr(text);
        PairingQrPanel.Visibility = text is null ? Visibility.Collapsed : Visibility.Visible;
    }

    string Describe(string? id)
    {
        if (string.IsNullOrEmpty(id))
            return "the main display";
        var known = sources.FirstOrDefault(x => x.Id == id);
        return known is not null ? known.Display : id switch
        {
            "display" => "the main display",
            _ when id.StartsWith("test-pattern") => $"a test pattern ({id})",
            _ => id,
        };
    }

    void SelectSource(string? id)
    {
        settingSource = true;
        try
        {
            SourcePicker.SelectedItem = sources.FirstOrDefault(x => x.Id == id);
        }
        finally
        {
            settingSource = false;
        }
    }

    async Task LoadSources()
    {
        var (list, error) = await M.Call(c => c.GetSources());
        if (list is null)
        {
            SourcesError.Text = error ?? "";
            SourcesError.Visibility = error is null ? Visibility.Collapsed : Visibility.Visible;
            return;
        }
        var all = list.All();
        // What's shared now stays in the list even when the listing doesn't have it (a test pattern's size, a closed window).
        var now = M.State?.Source;
        if (!string.IsNullOrEmpty(now) && all.All(x => x.Id != now))
            all.Insert(0, new SourceItem { Id = now, Name = Describe(now), Group = "Now" });
        sources = all;
        settingSource = true;
        SourcePicker.ItemsSource = sources;
        settingSource = false;
        SourcesError.Text = list.Error ?? "";
        SourcesError.Visibility = list.Error is null ? Visibility.Collapsed : Visibility.Visible;
        Show();
    }

    async void StartStop_Click(object sender, RoutedEventArgs e)
    {
        var sharing = M.State?.Sharing == true;
        busy = true;
        StartStopButton.IsEnabled = false;
        var (state, error) = await M.Call(c => sharing ? c.StopSharing() : c.StartSharing());
        busy = false;
        M.Take(state);
        Ui.Result(SharingResult, error);
        Show();
    }

    async void Restart_Click(object sender, RoutedEventArgs e)
    {
        var (state, error) = await M.Call(c => c.RestartSharing());
        M.Take(state);
        Ui.Result(SharingResult, error, error is null ? "Sharing restarted with the new settings." : null);
    }

    async void SourcePicker_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (settingSource || SourcePicker.SelectedItem is not SourceItem item || item.Id == M.State?.Source)
            return;
        SourcePicker.IsEnabled = false;
        var (state, error) = await M.Call(c => c.SetSource(item.Id));
        SourcePicker.IsEnabled = true;
        M.Take(state);
        Ui.Result(SharingResult, error, error is null ? $"Now sharing {item.Display}." : null);
        if (error is not null)
            SelectSource(M.State?.Source);
    }

    void SourcePicker_DropDownOpened(object sender, object e)
    {
        // Listing windows takes a moment: when the picker opens, not on a timer.
        if (sources.Count == 0)
            _ = LoadSources();
    }

    void RefreshSources_Click(object sender, RoutedEventArgs e) => _ = LoadSources();

    async void Links_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (Links.SelectedItem is not Link l)
            return;
        LinkQrCaption.Text = $"Scan to open: {l.Label}";
        var text = l.Qr ?? l.Url;
        if (text == qrShown)
            return;
        qrShown = text;
        LinkQr.Source = await Ui.Qr(text);
        AutomationProperties.SetName(LinkQr, $"QR code of the link for {l.Label}");
        LinkQrPanel.Visibility = Visibility.Visible;
    }

    void CopyLink_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string url })
        {
            Ui.Copy(url);
            Ui.Result(SharingResult, null, "Link copied.");
        }
    }

    void CopyCode_Click(object sender, RoutedEventArgs e)
    {
        if (M.State?.Pairing.Code is { } code)
            Ui.Copy(code);
    }

    async void NewCode_Click(object sender, RoutedEventArgs e)
    {
        NewCodeButton.IsEnabled = false;
        var (p, error) = await M.Call(c => c.NewCode());
        if (p is not null && M.State is { } s)
        {
            s.Pairing = p;
            M.Take(s);
        }
        Ui.Result(SharingResult, error);
        Show();
    }
}
