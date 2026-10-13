using Microsoft.UI.Dispatching;
using ReinRemote.Host.Core;

namespace ReinRemote.Host;

/// <summary>
/// What the pages show, from the host's events, raised on the UI thread; and the one way pages call
/// the API (<see cref="Call{T}"/>), which turns every failure into the host's message.
/// </summary>
public sealed class HostModel
{
    readonly DispatcherQueue ui;
    public HostConnection Connection { get; }

    public HostState? State { get; private set; }
    public ViewerList Viewers { get; private set; } = new();
    public DeviceList Devices { get; private set; } = new();
    public SettingsResponse? Settings { get; private set; }
    public LinkStatus Status { get; private set; } = LinkStatus.Connecting;
    public string? StatusMessage { get; private set; }

    public event Action? StateChanged;
    public event Action? ViewersChanged;
    public event Action? DevicesChanged;
    public event Action? SettingsChanged;
    public event Action? StatusChanged;
    /// <summary>A new host was connected to (after a start or a restart): pages reload what they fetched.</summary>
    public event Action? Connected;

    public HostModel(DispatcherQueue ui, HostConnection connection)
    {
        this.ui = ui;
        Connection = connection;
        connection.State += s => Post(() => { State = s; StateChanged?.Invoke(); });
        connection.Viewers += v => Post(() => { Viewers = v; ViewersChanged?.Invoke(); });
        connection.Devices += d => Post(() => { Devices = d; DevicesChanged?.Invoke(); });
        connection.Settings += s => Post(() => { Settings = s; SettingsChanged?.Invoke(); });
        connection.StatusChanged += (s, m) => Post(() =>
        {
            var was = Status;
            Status = s;
            StatusMessage = m;
            StatusChanged?.Invoke();
            if (s == LinkStatus.Connected && was != LinkStatus.Connected)
                Connected?.Invoke();
        });
    }

    public bool IsConnected => Status == LinkStatus.Connected && Connection.Client is not null;

    void Post(Action a) => ui.TryEnqueue(() =>
    {
        try
        {
            a();
        }
        catch (Exception e)
        {
            App.Log($"event handler: {e}");
        }
    });

    public void Start() => Connection.Start();

    /// <summary>
    /// Calls the API: the result, or null and <paramref name="error"/> set to what to show (the host's
    /// own message, or that it isn't running). Never throws.
    /// </summary>
    public async Task<(T? Result, string? Error)> Call<T>(Func<ControlClient, Task<T>> f) where T : class
    {
        var c = Connection.Client;
        if (c is null)
            return (null, StatusMessage ?? "Not connected to rein-remote host.");
        try
        {
            return (await f(c), null);
        }
        catch (ControlException e)
        {
            return (null, e.Message);
        }
        catch (ObjectDisposedException)
        {
            return (null, "The connection to rein-remote host was lost. Try again in a moment.");
        }
        catch (Exception e)
        {
            App.Log($"call: {e}");
            return (null, e.Message);
        }
    }

    /// <summary>Takes a state an action answered with (a POST's answer) before its event arrives.</summary>
    public void Take(HostState? s)
    {
        if (s is null)
            return;
        State = s;
        StateChanged?.Invoke();
    }

    public void Take(ViewerList? v)
    {
        if (v is null)
            return;
        Viewers = v;
        ViewersChanged?.Invoke();
    }

    public void Take(DeviceList? d)
    {
        if (d is null)
            return;
        Devices = d;
        DevicesChanged?.Invoke();
    }

    public void Take(SettingsResponse? s)
    {
        if (s is null)
            return;
        Settings = s;
        SettingsChanged?.Invoke();
    }
}
