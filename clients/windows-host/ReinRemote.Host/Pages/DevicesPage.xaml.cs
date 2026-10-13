using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using ReinRemote.Host.Core;

namespace ReinRemote.Host.Pages;

/// <summary>The paired devices, each with Unpair (asked first).</summary>
public sealed partial class DevicesPage : Page
{
    HostModel M => App.Model;

    public DevicesPage()
    {
        InitializeComponent();
        Loaded += (_, _) =>
        {
            M.DevicesChanged += Show;
            M.StatusChanged += Show;
            Show();
        };
        Unloaded += (_, _) =>
        {
            M.DevicesChanged -= Show;
            M.StatusChanged -= Show;
        };
    }

    void Show()
    {
        var list = M.IsConnected ? M.Devices.Devices : new List<Device>();
        DeviceList.ItemsSource = list;
        NoDevices.Text = M.IsConnected ? "No devices paired. Pair one from Sharing with its code." : "Not connected to rein-remote host.";
        NoDevices.Visibility = list.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    async void Unpair_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: Device d })
            return;
        var (yes, _) = await Ui.Confirm(XamlRoot, $"Unpair {d.Name}?",
            $"{d.Name} won't connect again without a new pairing code, and its open sessions end now.", "Unpair");
        if (!yes)
            return;
        var (list, error) = await M.Call(c => c.Unpair(d.Id));
        M.Take(list);
        Ui.Result(DevicesResult, error, error is null ? $"{d.Name} was unpaired." : null);
    }
}
