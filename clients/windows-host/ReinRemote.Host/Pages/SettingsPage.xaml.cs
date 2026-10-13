using System.Text.Json.Nodes;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using ReinRemote.Host.Core;

namespace ReinRemote.Host.Pages;

/// <summary>
/// host.toml's settings through <c>PUT /control/settings</c>: only what changed is sent, the host checks
/// it, and its message is shown when it refuses. Secrets are write-only. Start at login applies at once.
/// </summary>
public sealed partial class SettingsPage : Page
{
    HostModel M => App.Model;
    JsonObject loaded = new();
    bool filling;

    public SettingsPage()
    {
        InitializeComponent();
        Loaded += (_, _) =>
        {
            M.SettingsChanged += OnSettings;
            M.Connected += OnConnected;
            M.StatusChanged += Enable;
            OnSettings();
            _ = LoadStartAtLogin();
            Enable();
        };
        Unloaded += (_, _) =>
        {
            M.SettingsChanged -= OnSettings;
            M.Connected -= OnConnected;
            M.StatusChanged -= Enable;
        };
    }

    void Enable()
    {
        SaveSettings.IsEnabled = M.IsConnected;
        StartAtLoginToggle.IsEnabled = M.IsConnected;
    }

    void OnConnected() => _ = LoadStartAtLogin();

    /// <summary>New settings from the host: shown, unless the form has changes not saved yet.</summary>
    void OnSettings()
    {
        if (M.Settings is not { } s)
            return;
        if (loaded.Count > 0 && Changes().Count > 0)
            return;
        Fill(s);
    }

    void Fill(SettingsResponse r)
    {
        filling = true;
        var s = r.Settings;
        loaded = (JsonObject)s.DeepClone();
        var prefer = SettingsEdit.Text(s["prefer"]);
        PreferQuality.IsChecked = prefer == "quality";
        PreferFramerate.IsChecked = prefer != "quality";
        FpsBox.Text = SettingsEdit.Text(s["fps"]);
        BitrateBox.Text = SettingsEdit.Text(s["bitrate"]);
        var codec = SettingsEdit.Text(s["codec"]);
        CodecBox.SelectedItem = CodecBox.Items.OfType<ComboBoxItem>().FirstOrDefault(i => (string)i.Tag == codec) ?? CodecBox.Items[0];
        AudioToggle.IsOn = SettingsEdit.Flag(s["audio"], true);
        ListenBox.Text = SettingsEdit.Text(s["listen"]);
        MdnsToggle.IsOn = SettingsEdit.Flag(s["mdns"], true);
        PairToggle.IsOn = SettingsEdit.Flag(s["pair"], true);
        WebTransportToggle.IsOn = SettingsEdit.Flag(s["webtransport"], true);
        TailscaleToggle.IsOn = SettingsEdit.Flag(s["tailscale-cert"], true);
        RelaysBox.Text = SettingsEdit.LinesText(s["relay"]);
        RelaySecretBox.Password = "";
        RelaySecretState.Text = SettingsEdit.Flag(s["relay-secret-set"]) ? "A secret is set." : "No secret set.";
        RecordBox.Text = SettingsEdit.Text(s["record"]);
        AuditBox.Text = SettingsEdit.Text(s["audit"]);
        SettingsPath.Text = r.Path is null ? "" : $"Kept in {r.Path}";
        Overridden.Text = r.Overridden.Count == 0 ? "" : $"Set on rein-remote host's command line for this run: {string.Join(", ", r.Overridden)} (a change here wins from then on).";
        Overridden.Visibility = r.Overridden.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
        filling = false;
    }

    /// <summary>The form as the API takes it.</summary>
    JsonObject Edited()
    {
        JsonNode? OrNull(string text) => string.IsNullOrWhiteSpace(text) ? null : JsonValue.Create(text.Trim());
        return new JsonObject
        {
            ["prefer"] = PreferQuality.IsChecked == true ? "quality" : "framerate",
            ["fps"] = SettingsEdit.AutoOrNumber(FpsBox.Text),
            ["bitrate"] = SettingsEdit.AutoOrNumber(BitrateBox.Text),
            ["codec"] = (CodecBox.SelectedItem as ComboBoxItem)?.Tag as string ?? "h264",
            ["audio"] = AudioToggle.IsOn,
            ["listen"] = string.IsNullOrWhiteSpace(ListenBox.Text) ? "auto" : ListenBox.Text.Trim(),
            ["mdns"] = MdnsToggle.IsOn,
            ["pair"] = PairToggle.IsOn,
            ["webtransport"] = WebTransportToggle.IsOn,
            ["tailscale-cert"] = TailscaleToggle.IsOn,
            ["relay"] = SettingsEdit.Lines(RelaysBox.Text),
            ["record"] = OrNull(RecordBox.Text),
            ["audit"] = OrNull(AuditBox.Text),
        };
    }

    JsonObject Changes()
    {
        var changes = SettingsEdit.Diff(loaded, Edited());
        if (RelaySecretBox.Password.Length > 0)
            changes["relay-secret"] = RelaySecretBox.Password;
        return changes;
    }

    async void Save_Click(object sender, RoutedEventArgs e)
    {
        var changes = Changes();
        if (changes.Count == 0)
        {
            Ui.Result(SettingsResult, null, "Nothing changed.");
            return;
        }
        SaveSettings.IsEnabled = false;
        var (r, error) = await M.Call(c => c.PutSettings(changes));
        SaveSettings.IsEnabled = M.IsConnected;
        if (r is null)
        {
            Ui.Result(SettingsResult, error);
            RestartSharing.Visibility = Visibility.Collapsed;
            return;
        }
        Fill(r);
        M.Take(r);
        Ui.Result(SettingsResult, null, SettingsEdit.Applied(r.Applied));
        RestartSharing.Visibility = r.RestartNeeded ? Visibility.Visible : Visibility.Collapsed;
    }

    void Revert_Click(object sender, RoutedEventArgs e)
    {
        if (M.Settings is { } s)
            Fill(s);
        Ui.Result(SettingsResult, null);
    }

    async void Restart_Click(object sender, RoutedEventArgs e)
    {
        var (state, error) = await M.Call(c => c.RestartSharing());
        M.Take(state);
        Ui.Result(SettingsResult, error, error is null ? "Sharing restarted with the new settings." : null);
        RestartSharing.Visibility = Visibility.Collapsed;
    }

    async Task LoadStartAtLogin()
    {
        var (s, error) = await M.Call(c => c.GetStartAtLogin());
        ShowStartAtLogin(s, error);
    }

    void ShowStartAtLogin(StartAtLogin? s, string? error)
    {
        if (s is not null)
        {
            filling = true;
            StartAtLoginToggle.IsOn = s.Enabled;
            filling = false;
            StartAtLoginDetail.Text = s.Enabled
                ? $"On: a {(s.Kind == "task" ? "scheduled task at logon" : s.Kind)} named {s.Name} starts rein-remote host (from the next sign-in)."
                : "Off: rein-remote host starts when you open this app.";
        }
        if (error is not null)
            StartAtLoginDetail.Text = error;
    }

    async void StartAtLogin_Toggled(object sender, RoutedEventArgs e)
    {
        if (filling)
            return;
        var on = StartAtLoginToggle.IsOn;
        StartAtLoginToggle.IsEnabled = false;
        var (s, error) = await M.Call(c => c.SetStartAtLogin(on));
        StartAtLoginToggle.IsEnabled = M.IsConnected;
        if (s is null)
        {
            filling = true;
            StartAtLoginToggle.IsOn = !on;
            filling = false;
        }
        ShowStartAtLogin(s, error);
    }
}
