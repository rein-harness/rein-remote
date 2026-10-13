using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using ReinRemote.Host.Core;

namespace ReinRemote.Host.Pages;

/// <summary>The viewers connected now, each with Kick (asked first).</summary>
public sealed partial class ViewersPage : Page
{
    HostModel M => App.Model;

    public ViewersPage()
    {
        InitializeComponent();
        Loaded += (_, _) =>
        {
            M.ViewersChanged += Show;
            M.StatusChanged += Show;
            Show();
        };
        Unloaded += (_, _) =>
        {
            M.ViewersChanged -= Show;
            M.StatusChanged -= Show;
        };
    }

    void Show()
    {
        var list = M.IsConnected ? M.Viewers.Viewers : new List<Viewer>();
        ViewerList.ItemsSource = list;
        NoViewers.Text = !M.IsConnected ? "Not connected to rein-remote host." : M.State?.Sharing == false ? "Not sharing: no one can watch." : "No one is watching.";
        NoViewers.Visibility = list.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    async void Kick_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: Viewer v })
            return;
        var (yes, reason) = await Ui.Confirm(XamlRoot, $"Kick {v.Name}?",
            $"{v.Name} ({v.User}, from {v.Addr}) is told the host ended the session, and it closes. They can reconnect with the link or pairing they have.", "Kick", reason: true);
        if (!yes)
            return;
        var (list, error) = await M.Call(c => c.Kick(v.Id, reason));
        M.Take(list);
        Ui.Result(ViewersResult, error, error is null ? $"{v.Name} was kicked." : null);
    }
}
