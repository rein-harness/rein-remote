using System.Net;
using System.Text;
using System.Text.Json.Nodes;
using Xunit;

namespace ReinRemote.Host.Core.Tests;

public class ClientTests
{
    static (ControlClient, Stub) Client()
    {
        var stub = new Stub();
        var c = new ControlClient(new ControlInfo { Url = "http://127.0.0.1:52817", Token = "tok", Pid = 1, Api = 1 }, stub);
        return (c, stub);
    }

    [Fact]
    public async Task Every_request_has_the_token_as_a_bearer_and_never_in_the_url()
    {
        var (c, stub) = Client();
        stub.Answer = _ => (HttpStatusCode.OK, Doc.Json("\"links\": ["));
        var s = await c.GetState();
        Assert.True(s.Sharing);
        var r = Assert.Single(stub.Requests);
        Assert.Equal(HttpMethod.Get, r.Method);
        Assert.Equal("/control/state", r.Path);
        Assert.Equal("Bearer tok", r.Auth);
        Assert.DoesNotContain("tok", r.Path);
    }

    [Fact]
    public async Task Each_endpoint_its_method_path_and_body()
    {
        var (c, stub) = Client();
        stub.Answer = req => (HttpStatusCode.OK, req.RequestUri!.AbsolutePath switch
        {
            "/control/devices" or "/control/devices/wxyz-1234" => Doc.Json("{\"devices\""),
            "/control/viewers/5f1c 2a/kick" or "/control/viewers/5f1c%202a/kick" => Doc.Json("{\"viewers\""),
            "/control/settings" => Doc.Json("\"overridden\""),
            "/control/admin/policy" => Doc.Json("\"rules\": 2"),
            "/control/admin/audit" => Doc.Json("\"entries\""),
            "/control/pairing/code" => Doc.Json("\"expires_in_s\""),
            "/control/start-at-login" => Doc.Json("\"launchd\""),
            _ => Doc.Json("\"links\": ["),
        });
        await c.StartSharing();
        await c.StopSharing("lunch");
        await c.StopSharing();
        await c.RestartSharing();
        await c.SetSource("window:4242");
        await c.NewCode();
        await c.Unpair("wxyz-1234");
        await c.Kick("5f1c 2a", "bye");
        await c.PutSettings(new JsonObject { ["fps"] = 30 });
        await c.SetStartAtLogin(true);
        await c.PutPolicy("[[rule]]\n", "toml");
        await c.PutPolicy(null, null);
        await c.GetAudit(new AuditQuery { Events = new[] { "session_kicked" }, Limit = 50 });
        var got = stub.Requests.Select(r => $"{r.Method} {r.Path} {r.Body}").ToList();
        Assert.Equal(new[]
        {
            "POST /control/sharing/start ",
            "POST /control/sharing/stop {\"reason\":\"lunch\"}",
            "POST /control/sharing/stop ",
            "POST /control/sharing/restart ",
            "PUT /control/source {\"source\":\"window:4242\"}",
            "POST /control/pairing/code ",
            "DELETE /control/devices/wxyz-1234 ",
            "POST /control/viewers/5f1c%202a/kick {\"reason\":\"bye\"}",
            "PUT /control/settings {\"fps\":30}",
            "PUT /control/start-at-login {\"enabled\":true}",
            "PUT /control/admin/policy {\"text\":\"[[rule]]\\n\",\"format\":\"toml\"}",
            "PUT /control/admin/policy {\"text\":null,\"format\":null}",
            "GET /control/admin/audit?event=session_kicked&limit=50 ",
        }, got);
    }

    [Theory]
    [InlineData(HttpStatusCode.Conflict, "{\"error\":\"already sharing\"}", "already sharing")]
    [InlineData(HttpStatusCode.Unauthorized, "{\"error\":\"the control API needs its token\"}", "the control API needs its token")]
    [InlineData(HttpStatusCode.InternalServerError, "{\"error\":\"can't listen on 0.0.0.0:7979: Address already in use\"}", "can't listen on 0.0.0.0:7979: Address already in use")]
    public async Task A_refusal_is_the_hosts_message(HttpStatusCode status, string body, string message)
    {
        var (c, stub) = Client();
        stub.Answer = _ => (status, body);
        var e = await Assert.ThrowsAsync<ControlException>(() => c.StartSharing());
        Assert.Equal((int)status, e.Status);
        Assert.Equal(message, e.Message);
        Assert.False(e.Unreachable);
    }

    [Fact]
    public async Task A_host_that_is_gone_is_unreachable_not_a_crash()
    {
        var port = new System.Net.Sockets.TcpListener(IPAddress.Loopback, 0);
        port.Start();
        var free = ((IPEndPoint)port.LocalEndpoint).Port;
        port.Stop();
        using var c = new ControlClient(new ControlInfo { Url = $"http://127.0.0.1:{free}", Token = "t" });
        var e = await Assert.ThrowsAsync<ControlException>(() => c.GetState());
        Assert.True(e.Unreachable);
        Assert.StartsWith("can't reach rein-remote host", e.Message);
    }

    [Fact]
    public void Audit_query_strings()
    {
        Assert.Equal("", new AuditQuery().ToQueryString());
        Assert.Equal("?event=session_start,session_end&user=alice&since=2026-10-12T00%3A00%3A00Z&limit=50&before=1841",
            new AuditQuery { Events = new[] { "session_start", " session_end ", "" }, User = "alice", Since = "2026-10-12T00:00:00Z", Limit = 50, Before = "1841" }.ToQueryString());
        Assert.Equal("?user=a%20b", new AuditQuery { User = " a b " }.ToQueryString());
    }
}

public class SseTests
{
    static async Task<List<SseEvent>> ReadAll(string text, TimeSpan? idle = null)
    {
        var list = new List<SseEvent>();
        await foreach (var e in Sse.Read(new MemoryStream(Encoding.UTF8.GetBytes(text)), idle ?? TimeSpan.FromSeconds(5)))
            list.Add(e);
        return list;
    }

    [Fact]
    public async Task Events_as_the_doc_shows_them()
    {
        var events = await ReadAll("event: state\ndata: {\"api\":1,\"sharing\":true}\n\nevent: viewers\ndata: {\"viewers\":[]}\n\n: keepalive\n\nevent: devices\r\ndata: {\"devices\":[]}\r\n\r\n");
        Assert.Equal(new[] { "state", "viewers", "devices" }, events.Select(e => e.Event));
        Assert.Equal("{\"api\":1,\"sharing\":true}", events[0].Data);
        Assert.True(ControlClient.Parse<HostState>(events[0].Data).Sharing);
    }

    [Fact]
    public async Task Several_data_lines_join_and_an_unfinished_event_is_dropped()
    {
        var events = await ReadAll("data: a\ndata:b\n\nevent: state\ndata: {");
        var e = Assert.Single(events);
        Assert.Equal("message", e.Event);
        Assert.Equal("a\nb", e.Data);
    }

    [Fact]
    public async Task A_quiet_stream_times_out()
    {
        var pipe = new System.IO.Pipes.AnonymousPipeServerStream(System.IO.Pipes.PipeDirection.Out);
        using var reader = new System.IO.Pipes.AnonymousPipeClientStream(System.IO.Pipes.PipeDirection.In, pipe.ClientSafePipeHandle);
        await Assert.ThrowsAsync<TimeoutException>(async () =>
        {
            await foreach (var _ in Sse.Read(reader, TimeSpan.FromMilliseconds(300)))
            {
            }
        });
        pipe.Dispose();
    }
}

public class HomeAndSettingsTests
{
    [Fact]
    public void Home_folder_as_the_server_finds_it()
    {
        Func<string, string?> env(params (string, string)[] vars) => k => vars.FirstOrDefault(v => v.Item1 == k).Item2;
        Assert.Equal("/tmp/h", HostHome.Resolve(env(("REIN_REMOTE_HOME", "/tmp/h"), ("APPDATA", "C:\\AD")), windows: true));
        Assert.Equal(Path.Combine("C:\\Users\\me\\AppData\\Roaming", "rein-remote"),
            HostHome.Resolve(env(("APPDATA", "C:\\Users\\me\\AppData\\Roaming"), ("USERPROFILE", "C:\\Users\\me")), windows: true));
        Assert.Equal(Path.Combine("/Users/me", ".rein-remote"), HostHome.Resolve(env(("HOME", "/Users/me"), ("APPDATA", "x")), windows: false));
        Assert.Equal(Path.Combine("/Users/me", ".rein-remote"), HostHome.Resolve(env(("REIN_REMOTE_HOME", ""), ("HOME", "/Users/me")), windows: false));
    }

    [Fact]
    public void Control_json_missing_half_written_or_present()
    {
        var home = Path.Combine(Path.GetTempPath(), "rr-win-test-" + Guid.NewGuid());
        Directory.CreateDirectory(home);
        try
        {
            Assert.Null(ControlInfo.TryRead(home));
            File.WriteAllText(HostHome.ControlJson(home), "{\"url\":\"http://127.0.0.1:1\"");
            Assert.Null(ControlInfo.TryRead(home));
            File.WriteAllText(HostHome.ControlJson(home), Doc.Json("\"token\""));
            Assert.Equal(41237, ControlInfo.TryRead(home)!.Pid);
        }
        finally
        {
            Directory.Delete(home, true);
        }
    }

    [Fact]
    public void Settings_values_as_the_api_takes_them()
    {
        Assert.Equal("\"auto\"", SettingsEdit.AutoOrNumber("").ToJsonString());
        Assert.Equal("\"auto\"", SettingsEdit.AutoOrNumber(" Auto ").ToJsonString());
        Assert.Equal("30", SettingsEdit.AutoOrNumber("30").ToJsonString());
        Assert.Equal("\"fast\"", SettingsEdit.AutoOrNumber("fast").ToJsonString());
        Assert.Equal("[\"wss://a\",\"ws://b\"]", SettingsEdit.Lines("wss://a\r\n\r\n ws://b ").ToJsonString());
        Assert.Equal("{}", SettingsEdit.Diff(new JsonObject { ["fps"] = 30 }, new JsonObject { ["fps"] = 30 }).ToJsonString());
        Assert.Equal("{\"mdns\":false}", SettingsEdit.Diff(new JsonObject { ["mdns"] = true, ["pair"] = true }, new JsonObject { ["mdns"] = false, ["pair"] = true }).ToJsonString());
        Assert.StartsWith("Saved. Viewers are connected", SettingsEdit.Applied("on_restart"));
    }

    [Fact]
    public void Countdown()
    {
        var now = new DateTimeOffset(2026, 10, 13, 9, 30, 0, TimeSpan.Zero);
        Assert.Equal("10:00", Format.Countdown(now.AddMinutes(10), now));
        Assert.Equal("0:05", Format.Countdown(now.AddSeconds(4.2), now));
        Assert.Equal("expired", Format.Countdown(now.AddSeconds(-1), now));
        Assert.Equal("", Format.Countdown(null, now));
    }

    [Fact]
    public void The_host_binary_beside_the_app_first_then_path()
    {
        var dir = Path.Combine(Path.GetTempPath(), "rr-win-find-" + Guid.NewGuid());
        var other = Path.Combine(dir, "bin");
        Directory.CreateDirectory(other);
        try
        {
            Assert.Null(HostLauncher.Find(dir, ""));
            File.WriteAllText(Path.Combine(other, HostLauncher.ExeName), "");
            Assert.Equal(Path.Combine(other, HostLauncher.ExeName), HostLauncher.Find(dir, other));
            File.WriteAllText(Path.Combine(dir, HostLauncher.ExeName), "");
            Assert.Equal(Path.Combine(dir, HostLauncher.ExeName), HostLauncher.Find(dir, other));
        }
        finally
        {
            Directory.Delete(dir, true);
        }
    }
}
