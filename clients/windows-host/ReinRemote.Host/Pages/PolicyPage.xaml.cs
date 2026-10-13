using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using ReinRemote.Host.Core;

namespace ReinRemote.Host.Pages;

/// <summary>The policy's text (<c>/control/admin/policy</c>): checked by the server's parser, its message shown as it is.</summary>
public sealed partial class PolicyPage : Page
{
    HostModel M => App.Model;

    public PolicyPage()
    {
        InitializeComponent();
        Loaded += (_, _) =>
        {
            M.Connected += Load;
            Load();
        };
        Unloaded += (_, _) => M.Connected -= Load;
    }

    async void Load()
    {
        var (p, error) = await M.Call(c => c.GetPolicy());
        if (p is null)
        {
            Ui.Result(PolicyResult, error);
            return;
        }
        Show(p);
        PolicyText.Text = p.Text ?? "";
    }

    void Show(Policy p)
    {
        PolicyStatus.Text = p.Enabled
            ? $"On: {p.Rules} rule{(p.Rules == 1 ? "" : "s")}, from {p.Path}"
            : "Off: everyone who gets in (a link, a pairing, a sign-in) has full control.";
        PolicyFormat.SelectedIndex = p.Format == "json" ? 1 : 0;
        PolicyOff.IsEnabled = p.Enabled;
    }

    void Reload_Click(object sender, RoutedEventArgs e)
    {
        Ui.Result(PolicyResult, null);
        Load();
    }

    async void Save_Click(object sender, RoutedEventArgs e)
    {
        var text = PolicyText.Text.Replace("\r\n", "\n").Replace('\r', '\n');
        if (string.IsNullOrWhiteSpace(text))
        {
            Ui.Result(PolicyResult, "The policy is empty. To have none, turn it off.");
            return;
        }
        var format = (PolicyFormat.SelectedItem as ComboBoxItem)?.Tag as string ?? "toml";
        SavePolicy.IsEnabled = false;
        var (p, error) = await M.Call(c => c.PutPolicy(text, format));
        SavePolicy.IsEnabled = true;
        if (p is null)
        {
            // The parser's message, as it is (it says where).
            Ui.Result(PolicyResult, error);
            PolicyResult.Title = "The policy wasn't saved";
            return;
        }
        Show(p);
        Ui.Result(PolicyResult, null, $"Checked and saved. {SettingsEdit.Applied(p.Applied)}");
    }

    async void Off_Click(object sender, RoutedEventArgs e)
    {
        var (yes, _) = await Ui.Confirm(XamlRoot, "Turn the policy off?", "Everyone who gets in will have full control. The policy's file is kept.", "Turn off");
        if (!yes)
            return;
        var (p, error) = await M.Call(c => c.PutPolicy(null, null));
        if (p is not null)
            Show(p);
        Ui.Result(PolicyResult, error, error is null ? $"The policy is off. {SettingsEdit.Applied(p!.Applied)}" : null);
    }
}
