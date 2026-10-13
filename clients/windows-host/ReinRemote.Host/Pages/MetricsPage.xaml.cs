using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using ReinRemote.Host.Core;

namespace ReinRemote.Host.Pages;

/// <summary>The host's counters (<c>/control/admin/metrics</c>), every 2 seconds while the page shows.</summary>
public sealed partial class MetricsPage : Page
{
    HostModel M => App.Model;
    readonly DispatcherTimer timer = new() { Interval = TimeSpan.FromSeconds(2) };
    bool loading;

    public MetricsPage()
    {
        InitializeComponent();
        timer.Tick += (_, _) => Load();
        Loaded += (_, _) =>
        {
            timer.Start();
            Load();
        };
        Unloaded += (_, _) => timer.Stop();
    }

    async void Load()
    {
        if (loading)
            return;
        loading = true;
        var (m, error) = await M.Call(c => c.GetMetrics());
        loading = false;
        if (m is null)
        {
            MetricsInfo.Text = error ?? "";
            return;
        }
        MetricsList.ItemsSource = Format.MetricsRows(m);
        MetricsInfo.Text = $"Since rein-remote host started; updated {DateTime.Now:T}.";
    }

    void Refresh_Click(object sender, RoutedEventArgs e) => Load();
}
