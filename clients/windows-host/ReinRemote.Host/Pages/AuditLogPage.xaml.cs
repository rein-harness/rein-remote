using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using ReinRemote.Host.Core;

namespace ReinRemote.Host.Pages;

/// <summary>The audit log (<c>/control/admin/audit</c>), newest first: filters, and a page at a time.</summary>
public sealed partial class AuditLogPage : Page
{
    HostModel M => App.Model;
    static readonly string[] EventTypes =
    {
        "session_start", "session_end", "session_refused", "session_kicked", "signin", "signin_denied", "signout",
        "pair", "pair_failed", "device_signin", "device_signin_failed", "unpair", "server_start", "sharing_start",
        "sharing_stop", "settings_changed", "policy_changed", "signin_changed",
    };
    /// <summary>The <c>before</c> of each page shown so far (null: the newest).</summary>
    readonly List<string?> pages = new() { null };
    string? next;
    bool loaded;

    public AuditLogPage()
    {
        InitializeComponent();
        foreach (var t in EventTypes)
        {
            var item = new MenuFlyoutItem { Text = t };
            item.Click += (_, _) =>
            {
                var have = EventFilter.Text.Split(',', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries).ToList();
                if (!have.Contains(t))
                    have.Add(t);
                EventFilter.Text = string.Join(", ", have);
            };
            EventMenu.Items.Add(item);
        }
        Loaded += (_, _) =>
        {
            if (!loaded)
                _ = Search();
            loaded = true;
        };
    }

    AuditQuery Query(string? before) => new()
    {
        Events = EventFilter.Text.Split(',', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries),
        User = UserFilter.Text,
        Since = SinceFilter.Text,
        Limit = int.TryParse((LimitBox.SelectedItem as ComboBoxItem)?.Content as string, out var l) ? l : 50,
        Before = before,
    };

    async Task Search()
    {
        pages.Clear();
        pages.Add(null);
        await Show();
    }

    async Task Show()
    {
        var before = pages[^1];
        SearchAudit.IsEnabled = false;
        var (page, error) = await M.Call(c => c.GetAudit(Query(before)));
        SearchAudit.IsEnabled = true;
        if (page is null)
        {
            Ui.Result(AuditResult, error);
            return;
        }
        Ui.Result(AuditResult, null);
        var rows = page.Rows();
        AuditList.ItemsSource = rows;
        next = page.Next;
        OlderAudit.IsEnabled = next is not null;
        NewerAudit.IsEnabled = pages.Count > 1;
        AuditPageInfo.Text = page.Path is null
            ? "The audit log is off (Settings, Audit log)."
            : $"Page {pages.Count} · {rows.Count} entr{(rows.Count == 1 ? "y" : "ies")}{(rows.Count == 0 ? " match" : "")} · {page.Path}";
    }

    async void Search_Click(object sender, RoutedEventArgs e) => await Search();

    async void Older_Click(object sender, RoutedEventArgs e)
    {
        if (next is null)
            return;
        pages.Add(next);
        await Show();
    }

    async void Newer_Click(object sender, RoutedEventArgs e)
    {
        if (pages.Count <= 1)
            return;
        pages.RemoveAt(pages.Count - 1);
        await Show();
    }
}
