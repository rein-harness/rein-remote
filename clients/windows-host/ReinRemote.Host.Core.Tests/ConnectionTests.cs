using System.Collections.Concurrent;
using System.Net;
using System.Text;
using Xunit;

namespace ReinRemote.Host.Core.Tests;

/// <summary>
/// A stand-in for <c>rein-remote host</c>'s control API: control.json written to a home folder, the
/// token checked, <c>GET /control/state</c>, and <c>/control/events</c> with one of each event (as the
/// doc says a new subscriber gets) held open until <see cref="Dispose"/>, as a host that exits ends it.
/// </summary>
sealed class FakeHost : IDisposable
{
    readonly HttpListener listener = new();
    readonly CancellationTokenSource gone = new();
    public readonly string Token = Guid.NewGuid().ToString("N");
    public readonly int Pid;
    public readonly ConcurrentQueue<string> Seen = new();

    public FakeHost(string home, int pid)
    {
        Pid = pid;
        var port = FreePort();
        listener.Prefixes.Add($"http://127.0.0.1:{port}/");
        listener.Start();
        _ = Task.Run(Serve);
        File.WriteAllText(HostHome.ControlJson(home), $"{{\"url\":\"http://127.0.0.1:{port}\",\"token\":\"{Token}\",\"pid\":{pid},\"version\":\"0.1.1\",\"api\":1}}");
    }

    static int FreePort()
    {
        var l = new System.Net.Sockets.TcpListener(IPAddress.Loopback, 0);
        l.Start();
        var p = ((IPEndPoint)l.LocalEndpoint).Port;
        l.Stop();
        return p;
    }

    string State() => Doc.Json("\"links\": [").Replace("41237", Pid.ToString());

    async Task Serve()
    {
        while (!gone.IsCancellationRequested)
        {
            HttpListenerContext ctx;
            try
            {
                ctx = await listener.GetContextAsync();
            }
            catch (Exception)
            {
                return;
            }
            _ = Task.Run(() => Answer(ctx));
        }
    }

    async Task Answer(HttpListenerContext ctx)
    {
        var path = ctx.Request.Url!.AbsolutePath;
        Seen.Enqueue($"{ctx.Request.HttpMethod} {path} {ctx.Request.Headers["Authorization"]}");
        var res = ctx.Response;
        try
        {
            if (ctx.Request.Headers["Authorization"] != $"Bearer {Token}")
            {
                await Write(res, 401, "{\"error\":\"the control API needs its token\"}");
                return;
            }
            switch (path)
            {
                case "/control/state":
                    await Write(res, 200, State());
                    return;
                case "/control/events":
                    res.ContentType = "text/event-stream";
                    res.SendChunked = true;
                    var w = new StreamWriter(res.OutputStream, new UTF8Encoding(false)) { AutoFlush = true };
                    await w.WriteAsync($"event: state\ndata: {State().ReplaceLineEndings("")}\n\n");
                    await w.WriteAsync($"event: viewers\ndata: {Doc.Json("{\"viewers\"").ReplaceLineEndings("")}\n\n");
                    await w.WriteAsync($"event: devices\ndata: {Doc.Json("{\"devices\"").ReplaceLineEndings("")}\n\n");
                    await w.WriteAsync($"event: settings\ndata: {Doc.Json("\"overridden\"").ReplaceLineEndings("")}\n\n: keepalive\n\n");
                    try
                    {
                        await Task.Delay(Timeout.Infinite, gone.Token);
                    }
                    catch (OperationCanceledException)
                    {
                    }
                    res.Abort();
                    return;
                default:
                    await Write(res, 404, "{\"error\":\"no such endpoint\"}");
                    return;
            }
        }
        catch (Exception)
        {
        }
    }

    static async Task Write(HttpListenerResponse res, int status, string body)
    {
        res.StatusCode = status;
        res.ContentType = "application/json";
        var b = Encoding.UTF8.GetBytes(body);
        res.ContentLength64 = b.Length;
        await res.OutputStream.WriteAsync(b);
        res.Close();
    }

    public void Dispose()
    {
        gone.Cancel();
        try
        {
            listener.Stop();
            listener.Close();
        }
        catch (Exception)
        {
        }
    }
}

public class ConnectionTests : IDisposable
{
    readonly string home = Path.Combine(Path.GetTempPath(), "rr-win-conn-" + Guid.NewGuid());

    public ConnectionTests() => Directory.CreateDirectory(home);

    public void Dispose()
    {
        try
        {
            Directory.Delete(home, true);
        }
        catch (IOException)
        {
        }
    }

    static async Task Until(Func<bool> f, string what, int seconds = 20)
    {
        var end = DateTime.UtcNow.AddSeconds(seconds);
        while (DateTime.UtcNow < end)
        {
            if (f())
                return;
            await Task.Delay(50);
        }
        Assert.Fail($"timed out waiting for {what}");
    }

    [Fact]
    public async Task Follows_the_events_then_reconnects_to_the_next_host()
    {
        var states = new ConcurrentQueue<HostState>();
        var viewers = new ConcurrentQueue<ViewerList>();
        var settings = new ConcurrentQueue<SettingsResponse>();
        var statuses = new ConcurrentQueue<(LinkStatus, string?)>();
        var first = new FakeHost(home, 1001);
        await using var c = new HostConnection(home, null, retry: TimeSpan.FromMilliseconds(100));
        c.State += states.Enqueue;
        c.Viewers += viewers.Enqueue;
        c.Settings += settings.Enqueue;
        c.StatusChanged += (s, m) => statuses.Enqueue((s, m));
        c.Start();

        await Until(() => c.Status == LinkStatus.Connected && viewers.Count > 0 && settings.Count > 0, "the first host's events");
        Assert.All(states, s => Assert.Equal(1001, s.Pid));
        Assert.Equal("5f1c2a9b0e7d4c31", viewers.Last().Viewers[0].Id);
        Assert.NotNull(c.Client);
        await Until(() => first.Seen.Contains($"GET /control/events Bearer {first.Token}"), "the first host's events followed");

        // The host exits: its stream ends, the app says so and keeps trying.
        first.Dispose();
        await Until(() => c.Status == LinkStatus.Disconnected, "the link lost");
        Assert.Null(c.Client);
        Assert.Contains("Reconnecting", c.StatusMessage);

        // A new host (a new port, a new token) writes its control.json: the app finds it.
        using var second = new FakeHost(home, 1002);
        await Until(() => c.Status == LinkStatus.Connected && states.Any(s => s.Pid == 1002), "the second host");
        await Until(() => second.Seen.Contains($"GET /control/events Bearer {second.Token}"), "the second host's events followed");
        Assert.Equal(c.Client!.Info.Token, second.Token);
    }

    [Fact]
    public async Task A_stale_control_json_is_not_a_live_host()
    {
        File.WriteAllText(HostHome.ControlJson(home), "{\"url\":\"http://127.0.0.1:9\",\"token\":\"old\",\"pid\":5,\"api\":1}");
        await using var c = new HostConnection(home, null, retry: TimeSpan.FromMilliseconds(100));
        c.Start();
        await Until(() => c.Status == LinkStatus.Disconnected, "not connected");
        Assert.Equal("rein-remote host isn't running", c.StatusMessage);
    }

    [Fact]
    public async Task Starts_the_host_when_none_answers_and_says_why_it_stopped()
    {
        // A stand-in binary: it notes how it was started, says why on stderr, and exits 3.
        var dir = Path.Combine(home, "app");
        Directory.CreateDirectory(dir);
        string exe;
        if (OperatingSystem.IsWindows())
        {
            exe = Path.Combine(dir, "stub.cmd");
            File.WriteAllText(exe, "@echo off\r\necho %* > \"%~dp0launched.txt\"\r\necho rein-remote: can't open the display 1>&2\r\nexit /b 3\r\n");
        }
        else
        {
            exe = Path.Combine(dir, "stub.sh");
            File.WriteAllText(exe, "#!/bin/sh\necho \"$@\" > \"$(dirname \"$0\")/launched.txt\"\necho \"rein-remote: can't open the display\" >&2\nexit 3\n");
            File.SetUnixFileMode(exe, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
        }
        using var launcher = new HostLauncher(exe);
        await using var c = new HostConnection(home, launcher, retry: TimeSpan.FromMilliseconds(100));
        var statuses = new ConcurrentQueue<LinkStatus>();
        c.StatusChanged += (s, _) => statuses.Enqueue(s);
        c.Start();
        await Until(() => c.StatusMessage?.Contains("exit code 3") == true, "the reason it stopped");
        Assert.Contains(LinkStatus.Starting, statuses);
        Assert.Equal("host", File.ReadAllText(Path.Combine(dir, "launched.txt")).Trim());
        Assert.EndsWith("can't open the display", c.StatusMessage);
        // (Opened as a viewer of a log being written would: it may still be open for writing.)
        using var log = new StreamReader(new FileStream(Path.Combine(home, "host.log"), FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete));
        Assert.Contains("can't open the display", log.ReadToEnd());
    }
}
