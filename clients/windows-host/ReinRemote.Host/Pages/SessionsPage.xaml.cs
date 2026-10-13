using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using ReinRemote.Host.Core;

namespace ReinRemote.Host.Pages;

/// <summary>The admin's view of the sessions (<c>/control/admin/sessions</c>), fetched again as viewers change.</summary>
public sealed partial class SessionsPage : Page
{
    HostModel M => App.Model;
    bool loading, again;

    public SessionsPage()
    {
        InitializeComponent();
        Loaded += (_, _) =>
        {
            M.ViewersChanged += Load;
            M.Connected += Load;
            Load();
        };
        Unloaded += (_, _) =>
        {
            M.ViewersChanged -= Load;
            M.Connected -= Load;
        };
    }

    async void Load()
    {
        if (loading)
        {
            again = true;
            return;
        }
        loading = true;
        do
        {
            again = false;
            var (s, error) = await M.Call(c => c.GetSessions());
            var list = s?.Sessions ?? new List<Viewer>();
            SessionList.ItemsSource = list;
            NoSessions.Text = error ?? "No sessions.";
            NoSessions.Visibility = list.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        } while (again);
        loading = false;
    }

    async void Kick_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: Viewer v })
            return;
        var (yes, reason) = await Ui.Confirm(XamlRoot, $"Kick {v.Name}?", $"{v.Name} ({v.User}) is told the host ended the session, and it closes.", "Kick", reason: true);
        if (!yes)
            return;
        var (list, error) = await M.Call(c => c.Kick(v.Id, reason));
        M.Take(list);
        Ui.Result(SessionsResult, error, error is null ? $"{v.Name} was kicked." : null);
        Load();
    }
}
