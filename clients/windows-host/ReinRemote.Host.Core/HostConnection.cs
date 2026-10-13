using System.Text.Json;

namespace ReinRemote.Host.Core;

public enum LinkStatus
{
    /// <summary>Looking for the host (control.json, then GET /control/state).</summary>
    Connecting,
    /// <summary>Starting <c>rein-remote host</c> (none was running).</summary>
    Starting,
    /// <summary>Following its events.</summary>
    Connected,
    /// <summary>No host answers; trying again.</summary>
    Disconnected,
}

/// <summary>
/// The app's link to <c>rein-remote host</c>: finds the live one (its <c>control.json</c> and a
/// <c>GET /control/state</c> that answers, docs/control-api.md), starts one when none answers (with a
/// <see cref="HostLauncher"/>), follows <c>/control/events</c>, and when the stream ends (the host
/// exited or restarted) reads <c>control.json</c> again and reconnects. Events are raised on a
/// background thread.
/// </summary>
public sealed class HostConnection : IAsyncDisposable
{
    public string Home { get; }
    public HostLauncher? Launcher { get; }
    readonly TimeSpan retry;
    readonly TimeSpan launchWait;
    readonly TimeSpan idle;
    readonly CancellationTokenSource stop = new();
    Task? loop;
    DateTimeOffset lastLaunch = DateTimeOffset.MinValue;

    public event Action<LinkStatus, string?>? StatusChanged;
    public event Action<HostState>? State;
    public event Action<ViewerList>? Viewers;
    public event Action<DeviceList>? Devices;
    public event Action<SettingsResponse>? Settings;
    /// <summary>A client for the host just connected to (null when the link is lost).</summary>
    public event Action<ControlClient?>? ClientChanged;

    public LinkStatus Status { get; private set; } = LinkStatus.Connecting;
    public string? StatusMessage { get; private set; }
    public ControlClient? Client { get; private set; }

    public HostConnection(string home, HostLauncher? launcher, TimeSpan? retry = null, TimeSpan? launchWait = null, TimeSpan? idle = null)
    {
        Home = home;
        Launcher = launcher;
        this.retry = retry ?? TimeSpan.FromSeconds(1);
        this.launchWait = launchWait ?? TimeSpan.FromSeconds(20);
        this.idle = idle ?? TimeSpan.FromSeconds(40);
    }

    public void Start() => loop ??= Task.Run(() => Run(stop.Token));

    void SetStatus(LinkStatus s, string? message)
    {
        if (Status == s && StatusMessage == message)
            return;
        Status = s;
        StatusMessage = message;
        StatusChanged?.Invoke(s, message);
    }

    /// <summary>A client for the host in control.json when it answers, else null.</summary>
    async Task<(ControlClient?, HostState?, string?)> Find(CancellationToken ct)
    {
        var info = ControlInfo.TryRead(Home);
        if (info is null)
            return (null, null, "rein-remote host isn't running (no control.json in " + Home + ")");
        var c = new ControlClient(info);
        try
        {
            using var quick = CancellationTokenSource.CreateLinkedTokenSource(ct);
            quick.CancelAfter(TimeSpan.FromSeconds(5));
            HostState s;
            try
            {
                s = await c.GetState(quick.Token).ConfigureAwait(false);
            }
            catch (OperationCanceledException) when (!ct.IsCancellationRequested)
            {
                throw new ControlException(0, "no answer");
            }
            return (c, s, null);
        }
        catch (ControlException e)
        {
            c.Dispose();
            return (null, null, e.Unreachable ? "rein-remote host isn't running" : $"rein-remote host refused this app: {e.Message}");
        }
    }

    async Task Run(CancellationToken ct)
    {
        var wasConnected = false;
        while (!ct.IsCancellationRequested)
        {
            try
            {
                if (!wasConnected)
                    SetStatus(Status == LinkStatus.Disconnected ? LinkStatus.Disconnected : LinkStatus.Connecting, StatusMessage);
                var (client, state, why) = await Find(ct).ConfigureAwait(false);
                var launched = false;
                if (client is null && Launcher is not null && Launcher.Child is null && DateTimeOffset.UtcNow - lastLaunch > TimeSpan.FromSeconds(10))
                {
                    (client, state, why) = await LaunchAndFind(ct).ConfigureAwait(false);
                    launched = true;
                }
                if (client is null)
                {
                    // Why a start failed says more than "reconnecting"; it stays until the next attempt says otherwise.
                    if (launched || !wasConnected || Status != LinkStatus.Disconnected || StatusMessage is null)
                        SetStatus(LinkStatus.Disconnected, wasConnected && !launched ? "Lost the connection to rein-remote host. Reconnecting…" : why);
                    await Task.Delay(retry, ct).ConfigureAwait(false);
                    continue;
                }
                await Follow(client, state!, ct).ConfigureAwait(false);
                wasConnected = true;
                SetStatus(LinkStatus.Disconnected, "Lost the connection to rein-remote host. Reconnecting…");
                await Task.Delay(retry, ct).ConfigureAwait(false);
            }
            catch (OperationCanceledException) when (ct.IsCancellationRequested)
            {
                return;
            }
            catch (Exception e)
            {
                // Nothing the host does (or doesn't) ends this loop.
                SetStatus(LinkStatus.Disconnected, $"rein-remote host: {e.Message}");
                try
                {
                    await Task.Delay(retry, ct).ConfigureAwait(false);
                }
                catch (OperationCanceledException)
                {
                    return;
                }
            }
        }
    }

    async Task<(ControlClient?, HostState?, string?)> LaunchAndFind(CancellationToken ct)
    {
        lastLaunch = DateTimeOffset.UtcNow;
        SetStatus(LinkStatus.Starting, "Starting rein-remote host…");
        System.Diagnostics.Process p;
        try
        {
            p = Launcher!.Launch(Home);
        }
        catch (Exception e)
        {
            return (null, null, $"Couldn't start {Launcher!.ExePath}: {e.Message}");
        }
        var end = DateTimeOffset.UtcNow + launchWait;
        while (DateTimeOffset.UtcNow < end)
        {
            await Task.Delay(200, ct).ConfigureAwait(false);
            if (ControlInfo.TryRead(Home) is { } info && info.Pid == p.Id)
            {
                var found = await Find(ct).ConfigureAwait(false);
                if (found.Item1 is not null)
                    return found;
            }
            if (p.HasExited)
            {
                // Another host may have won the race (one host per user): look for it.
                var other = await Find(ct).ConfigureAwait(false);
                if (other.Item1 is not null)
                    return other;
                await Task.Delay(200, ct).ConfigureAwait(false);
                var tail = Launcher.Tail.Trim();
                return (null, null, $"rein-remote host stopped (exit code {Launcher.ExitCode}){(tail.Length > 0 ? ": " + tail.Split('\n')[^1] : "")}");
            }
        }
        return (null, null, "rein-remote host was started but its control API didn't answer");
    }

    async Task Follow(ControlClient client, HostState first, CancellationToken ct)
    {
        Client = client;
        ClientChanged?.Invoke(client);
        SetStatus(LinkStatus.Connected, null);
        State?.Invoke(first);
        try
        {
            await foreach (var e in client.Events(idle, ct).ConfigureAwait(false))
            {
                try
                {
                    switch (e.Event)
                    {
                        case "state":
                            State?.Invoke(ControlClient.Parse<HostState>(e.Data));
                            break;
                        case "viewers":
                            Viewers?.Invoke(ControlClient.Parse<ViewerList>(e.Data));
                            break;
                        case "devices":
                            Devices?.Invoke(ControlClient.Parse<DeviceList>(e.Data));
                            break;
                        case "settings":
                            Settings?.Invoke(ControlClient.Parse<SettingsResponse>(e.Data));
                            break;
                    }
                }
                catch (JsonException)
                {
                    // An event this app can't read: the next one is the full object again.
                }
            }
        }
        catch (OperationCanceledException) when (ct.IsCancellationRequested)
        {
            throw;
        }
        catch (Exception)
        {
            // The stream broke (the host exited, or went quiet): reconnect.
        }
        finally
        {
            Client = null;
            ClientChanged?.Invoke(null);
            client.Dispose();
        }
    }

    public async ValueTask DisposeAsync()
    {
        stop.Cancel();
        if (loop is not null)
        {
            try
            {
                await loop.ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
            }
        }
        stop.Dispose();
    }
}
