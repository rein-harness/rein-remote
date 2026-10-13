using System.Text.Json;
using System.Text.Json.Nodes;
using Xunit;

namespace ReinRemote.Host.Core.Tests;

/// <summary>Each example in the contract read into this app's models, with the values it shows.</summary>
public class ModelTests
{
    [Fact]
    public void Every_example_in_the_doc_is_json()
    {
        Assert.True(Doc.All.Count >= 18, $"only {Doc.All.Count} JSON examples found");
        foreach (var b in Doc.All)
            JsonNode.Parse(b);
    }

    [Fact]
    public void Control_json()
    {
        var c = ControlInfo.Parse(Doc.Json("\"token\""));
        Assert.Equal("http://127.0.0.1:52817", c.Url);
        Assert.StartsWith("4f9c0d", c.Token);
        Assert.Equal(41237, c.Pid);
        Assert.Equal(1, c.Api);
    }

    [Fact]
    public void State()
    {
        var s = ControlClient.Parse<HostState>(Doc.Json("\"links\": ["));
        Assert.True(s.Sharing);
        Assert.Null(s.Error);
        Assert.Equal("display:1", s.Source);
        Assert.Equal(4, s.Links.Count);
        Assert.Equal(new[] { "tailnet", "lan", "local", "relay" }, s.Links.Select(l => l.Kind));
        Assert.Equal(s.Links[1].Url, s.Links[1].Qr);
        Assert.Equal("this network", s.Links[1].Label);
        Assert.Equal("123-456", s.Pairing.Code);
        Assert.Equal(new DateTimeOffset(2026, 10, 13, 9, 40, 0, TimeSpan.Zero), s.Pairing.ExpiresAt);
        Assert.Equal(1, s.Viewers);
        Assert.False(s.RestartNeeded);
    }

    [Fact]
    public void Sources()
    {
        var l = ControlClient.Parse<SourceList>(Doc.Json("\"displays\""));
        var all = l.All();
        Assert.Equal(new[] { "display:1", "display:2", "window:4242", "app:Safari", "test-pattern" }, all.Select(s => s.Id));
        Assert.Equal("Display: Display 1 (main) (3024×1964)", all[0].Display);
        Assert.Equal("Window: Safari: Apple (1440×900)", all[2].Display);
        Assert.Equal("App: Safari (2 windows)", all[3].Display);
        Assert.Equal("Other: Test pattern", all[4].Display);
        Assert.Null(l.Error);
    }

    [Fact]
    public void Pairing()
    {
        var p = ControlClient.Parse<Pairing>(Doc.Json("\"expires_in_s\""));
        Assert.True(p.Enabled);
        Assert.Equal(512, p.ExpiresInS);
        Assert.Equal("abcd-efgh-ijkl-mnop", p.Fingerprint);
        Assert.Contains("#pair=123456", p.Qr);
    }

    [Fact]
    public void Devices_and_viewers()
    {
        var d = ControlClient.Parse<DeviceList>(Doc.Json("{\"devices\""));
        Assert.Equal("Gary's iPhone", Assert.Single(d.Devices).Name);
        var v = Assert.Single(ControlClient.Parse<ViewerList>(Doc.Json("{\"viewers\"")).Viewers);
        Assert.Equal("5f1c2a9b0e7d4c31", v.Id);
        Assert.Equal("paired", v.Via);
        Assert.Equal("webtransport", v.Transport);
        Assert.Equal(60, v.Fps);
        Assert.Equal(4.2, v.RttMs);
        Assert.False(v.ViewOnly);
        Assert.Equal("Gary's iPhone", v.Name);
        Assert.StartsWith("60 fps · 1280×720 · 8 Mbit/s · 4.2 ms round trip", v.Stats);
    }

    [Fact]
    public void Settings()
    {
        var s = ControlClient.Parse<SettingsResponse>(Doc.Json("\"overridden\""));
        Assert.Equal("framerate", SettingsEdit.Text(s.Settings["prefer"]));
        Assert.Equal("auto", SettingsEdit.Text(s.Settings["fps"]));
        Assert.True(SettingsEdit.Flag(s.Settings["relay-secret-set"]));
        Assert.Equal("wss://relay.example", SettingsEdit.LinesText(s.Settings["relay"]));
        Assert.Equal(new[] { "test-pattern" }, s.Overridden);
        Assert.Null(s.Applied);
        Assert.Equal("display:1", s.Source);
    }

    [Fact]
    public void Start_at_login_and_permissions()
    {
        var s = ControlClient.Parse<StartAtLogin>(Doc.Json("\"launchd\""));
        Assert.True(s.Enabled);
        Assert.Equal("rein-remote-host", s.Name);
        var p = ControlClient.Parse<Permissions>(Doc.Json("\"screen_recording\": \"granted\""));
        Assert.Equal("denied", p.Accessibility);
    }

    [Fact]
    public void Sign_in_keeps_its_secret_out()
    {
        var s = Doc.Obj("\"oidc-issuer\": \"https://example.okta.com\"");
        Assert.Equal("oidc", SettingsEdit.Text(s["kind"]));
        Assert.True(SettingsEdit.Flag(s["oidc-client-secret-set"]));
        Assert.False(s.ContainsKey("oidc-client-secret"));
    }

    [Fact]
    public void Policy()
    {
        var p = ControlClient.Parse<Policy>(Doc.Json("\"rules\": 2"));
        Assert.True(p.Enabled);
        Assert.Equal("toml", p.Format);
        Assert.Contains("[[rule]]", p.Text);
        var off = ControlClient.Parse<Policy>("{\"enabled\": false, \"path\": null, \"format\": null, \"rules\": 0, \"text\": null}");
        Assert.False(off.Enabled);
        Assert.Null(off.Text);
    }

    [Fact]
    public void Audit_page()
    {
        var a = ControlClient.Parse<AuditPage>(Doc.Json("\"entries\""));
        Assert.Equal("1841", a.Next);
        var rows = a.Rows();
        Assert.Equal(new[] { "session_end", "session_start" }, rows.Select(r => r.Event));
        Assert.Equal("alice@corp.example", rows[0].User);
        Assert.Contains("reason=kicked", rows[0].Details);
        Assert.Contains("session=5f1c2a9b0e7d4c31", rows[0].Details);
    }

    [Fact]
    public void Metrics()
    {
        var m = Doc.Obj("\"uptime_s\"");
        var rows = Format.MetricsRows(m);
        Assert.Contains(new KeyValueRow("uptime_s", "1h 0m 0s"), rows);
        Assert.Contains(new KeyValueRow("viewers.direct", "1"), rows);
        Assert.Contains(new KeyValueRow("frames_sent_total", "182,344"), rows);
        Assert.Contains(new KeyValueRow("relays[0].up", "yes"), rows);
        Assert.Contains(rows, r => r.Key == "video_bytes_sent_total" && r.Value.StartsWith("869.0 MB"));
    }

    [Fact]
    public void Errors_show_the_hosts_text()
    {
        Assert.Equal("no viewer has that id (it may have just left)",
            ControlClient.ErrorText(System.Net.HttpStatusCode.NotFound, Doc.Json("no viewer has that id")));
        Assert.StartsWith("policy: unknown field `view_ony`",
            ControlClient.ErrorText(System.Net.HttpStatusCode.BadRequest, Doc.Json("view_ony")));
        Assert.Equal("rein-remote host answered 502 BadGateway", ControlClient.ErrorText(System.Net.HttpStatusCode.BadGateway, "<html>"));
    }

    [Fact]
    public void Requests_in_the_doc_are_what_the_client_sends()
    {
        // PUT /control/settings's example, built the way the settings page builds it.
        var loaded = new JsonObject { ["prefer"] = "framerate", ["fps"] = "auto", ["codec"] = "h264" };
        var edited = new JsonObject { ["prefer"] = "quality", ["fps"] = SettingsEdit.AutoOrNumber("60"), ["codec"] = "h264", ["relay-secret"] = "s3cret" };
        var diff = SettingsEdit.Diff(loaded, edited);
        Assert.True(JsonNode.DeepEquals(Doc.Obj("\"relay-secret\": \"s3cret\""), diff), diff.ToJsonString());
    }
}
