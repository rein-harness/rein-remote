using System.Diagnostics;
using ReinRemote.Host.Core;
using Xunit;
using Xunit.Abstractions;

namespace ReinRemote.Host.UiTests;

/// <summary>
/// The app, driven through UI Automation as a person would, against a real <c>rein-remote host</c> with
/// a test pattern on a home folder of its own. The host's API (and its files) are the oracle.
/// </summary>
public class AppTests
{
    readonly ITestOutputHelper output;
    static readonly string[] HostArgs = { "--test-pattern", "1280x720", "--listen", "127.0.0.1:0", "--mdns", "off" };

    public AppTests(ITestOutputHelper output) => this.output = output;

    void Step(string what) => output.WriteLine($"{DateTime.Now:HH:mm:ss.f} {what}");

    /// <summary>`rein-remote connect`: a viewer that stays until it's kicked or its time is up.</summary>
    Process Viewer(string link)
    {
        var psi = new ProcessStartInfo(Env.Server) { UseShellExecute = false, RedirectStandardError = true, RedirectStandardOutput = true, CreateNoWindow = true };
        foreach (var a in new[] { "connect", link, "--seconds", "120", "--webtransport", "off" })
            psi.ArgumentList.Add(a);
        var p = Process.Start(psi)!;
        p.ErrorDataReceived += (_, e) => { if (e.Data is not null) output.WriteLine($"  connect: {e.Data}"); };
        p.OutputDataReceived += (_, e) => { if (e.Data is not null) output.WriteLine($"  connect: {e.Data}"); };
        p.BeginErrorReadLine();
        p.BeginOutputReadLine();
        return p;
    }

    [Fact]
    public void Share_pick_a_source_pair_kick_set_check_and_survive_the_host()
    {
        var home = Env.NewHome("flow");
        var host = HostProcess.Start(home, output, HostArgs);
        using var app = AppSession.Launch(home, output, env: new() { ["REIN_REMOTE_HOST_EXE"] = "none" });
        var viewers = new List<Process>();
        try
        {
            Step("connected, not sharing");
            Env.Until(() => app.Window.Title.Contains("Not sharing"), "the app connected (its title says Not sharing)", 60);
            app.Page("NavSharing", "Sharing");
            Env.Until(() => app.Text("SharingStatus") == "Not sharing", "Not sharing on the page");
            app.Shot("sharing-off");

            Step("start");
            app.Press("StartStopButton");
            Env.Until(() => host.Get(c => c.GetState()).Sharing, "the host sharing");
            Env.Until(() => app.Text("SharingStatus") == "Sharing", "Sharing on the page");
            var state = host.Get(c => c.GetState());
            Env.Until(() => app.AllText("Links").Contains(state.Links[0].Url), "the link listed");
            Assert.Equal("Stop sharing", app.ById("StartStopButton").Name);

            Step("pairing: a new code");
            var code = Env.Until(() => host.Get(c => c.GetPairing()).Code, "a pairing code");
            Env.Until(() => app.ById("PairingCode").Name.Contains(code), "the code shown");
            app.Press("NewCodeButton");
            var newCode = Env.Until(() => host.Get(c => c.GetPairing()).Code is { } c && c != code ? c : null, "a new code at the host");
            Env.Until(() => app.ById("PairingCode").Name.Contains(newCode), "the new code shown");
            Assert.Matches(@"^\d+:\d\d left$", app.Text("PairingCountdown"));
            app.Shot("sharing");

            Step("the source: the display, then the test pattern again");
            var picked = app.Pick("SourcePicker", "Display");
            Step($"picked {picked}");
            Env.Until(() => host.Get(c => c.GetState()).Source == "display", "the host showing the display");
            Env.Until(() => app.Text("CurrentSource").Contains("Display"), "the page saying so");
            app.Pick("SourcePicker", "Other: Test pattern");
            Env.Until(() => host.Get(c => c.GetState()).Source?.StartsWith("test-pattern") == true, "the test pattern again");

            Step("a viewer comes, and is kicked");
            var link = host.Get(c => c.GetState()).Links[0].Url;
            viewers.Add(Viewer(link));
            app.Page("NavViewers", "Viewers");
            Env.Until(() => host.Get(c => c.GetViewers()).Viewers.Count == 1, "the viewer at the host");
            Env.Until(() => app.Find("KickButton"), "the viewer listed");
            app.Shot("viewers");
            var id = host.Get(c => c.GetViewers()).Viewers[0].Id;
            app.Press("KickButton");
            app.Dialog("ui test");
            Env.Until(() => host.Get(c => c.GetViewers()).Viewers.Count == 0, "the viewer gone at the host");
            Env.Until(() => app.Find("NoViewers") is { IsOffscreen: false }, "no viewers on the page");
            Assert.True(viewers[0].WaitForExit(30000), "rein-remote connect ended when kicked");
            Assert.Contains("session_kicked", File.ReadAllText(Path.Combine(home, "audit.jsonl")));

            Step("live sessions, with a second viewer");
            viewers.Add(Viewer(link));
            Env.Until(() => host.Get(c => c.GetViewers()).Viewers.Count == 1, "the second viewer");
            app.Page("NavSessions", "Live sessions");
            Env.Until(() => app.Find("SessionKick"), "the session listed");
            Env.Until(() => app.AllText("SessionList").Contains("rule none"), "the policy's view of it");
            app.Shot("sessions");
            viewers[1].Kill(true);
            Env.Until(() => host.Get(c => c.GetViewers()).Viewers.Count == 0, "the second viewer gone");

            Step("a setting saved to host.toml");
            app.Page("NavSettings", "Settings");
            Env.Until(() => app.Text("FpsBox") == "auto", "the settings loaded");
            app.Type("FpsBox", "30");
            app.Press("SaveSettings");
            Env.Until(() => app.AllText("SettingsResult").Contains("Saved"), "Saved shown");
            Env.Until(() => File.ReadAllText(HostHome.HostToml(home)).Contains("fps = 30"), "fps = 30 in host.toml");
            Assert.Equal("30", host.Get(c => c.GetSettings()).Settings["fps"]!.ToString());
            app.Type("FpsBox", "500");
            app.Press("SaveSettings");
            Env.Until(() => app.AllText("SettingsResult").Contains("fps"), "the host's refusal shown");
            output.WriteLine($"refused: {app.AllText("SettingsResult")}");
            app.Type("FpsBox", "30");
            app.Shot("settings");

            Step("a bad policy: the parser's message");
            app.Page("NavPolicy", "Policy");
            app.Type("PolicyText", "[[rule]]\nname = \"everyone\"\nview_ony = true\n");
            app.Press("SavePolicy");
            Env.Until(() => app.AllText("PolicyResult").Contains("view_ony"), "the parser's error shown");
            output.WriteLine($"policy error: {app.AllText("PolicyResult")}");
            Assert.False(host.Get(c => c.GetPolicy()).Enabled);
            app.Shot("policy-error");
            app.Type("PolicyText", "[[rule]]\nname = \"everyone, view only\"\nview_only = true\n");
            app.Press("SavePolicy");
            Env.Until(() => host.Get(c => c.GetPolicy()).Enabled, "a good policy saved");
            Env.Until(() => app.Text("PolicyStatus").StartsWith("On: 1 rule"), "the policy on");
            app.Shot("policy");

            Step("the audit log shows the kick");
            app.Page("NavAudit", "Audit log");
            app.Type("EventFilter", "session_kicked");
            app.Press("SearchAudit");
            Env.Until(() => app.AllText("AuditList").Contains("session_kicked"), "session_kicked listed");
            Assert.Contains(id, app.AllText("AuditList"));
            app.Type("EventFilter", "");
            app.Press("SearchAudit");
            Env.Until(() => app.AllText("AuditList").Contains("settings_changed"), "every event listed");
            app.Shot("audit");

            Step("sign-in and metrics");
            app.Page("NavSignIn", "Sign-in");
            app.Shot("signin");
            app.Page("NavMetrics", "Metrics");
            Env.Until(() => app.AllText("MetricsList").Contains("sessions_total"), "the counters");
            app.Shot("metrics");
            app.Page("NavDevices", "Paired devices");
            app.Shot("devices");

            Step("stop");
            app.Page("NavSharing", "Sharing");
            app.Press("StartStopButton");
            Env.Until(() => !host.Get(c => c.GetState()).Sharing, "the host not sharing");
            Env.Until(() => app.Text("SharingStatus") == "Not sharing", "Not sharing on the page");

            Step("the host is gone: said so, no crash");
            host.Dispose();
            Env.Until(() => app.AllText("ConnectionBar").Contains("Not connected"), "Not connected shown");
            Assert.True(app.Running, "the app still runs");
            app.Shot("host-gone");

            Step("a new host on the same home folder: the app reconnects");
            host = HostProcess.Start(home, output, HostArgs);
            Env.Until(() => app.Window.Title.Contains("Not sharing") && app.Find("ConnectionBar") is null or { IsOffscreen: true }, "reconnected", 60);
            Env.Until(() => app.Text("SharingStatus") == "Not sharing", "the page live again");
            app.Press("StartStopButton");
            Env.Until(() => host.Get(c => c.GetState()).Sharing, "sharing through the new host");
            Assert.True(app.Running);
        }
        catch (Exception)
        {
            output.WriteLine(app.Dump());
            output.WriteLine("--- host log\n" + host.Log);
            var log = Path.Combine(home, "app.log");
            if (File.Exists(log))
                output.WriteLine("--- app.log\n" + File.ReadAllText(log));
            try { app.Shot("failure"); } catch (Exception) { }
            throw;
        }
        finally
        {
            foreach (var v in viewers)
                try { v.Kill(true); } catch (Exception) { }
            host.Dispose();
        }
    }

    [Fact]
    public void Starts_its_own_host_when_none_runs_in_the_dark_theme()
    {
        var home = Env.NewHome("launch");
        Assert.Null(ControlInfo.TryRead(home));
        // No REIN_REMOTE_HOST_EXE: the app starts the rein-remote.exe beside it.
        using var app = AppSession.Launch(home, output, theme: "dark");
        Process? child = null;
        try
        {
            var info = Env.Until(() => ControlInfo.TryRead(home), "control.json from the host the app started", 60);
            child = Process.GetProcessById(info.Pid);
            Step($"the app started {child.ProcessName} (pid {info.Pid})");
            Assert.Equal("rein-remote", child.ProcessName);
            Env.Until(() => app.Window.Title.Contains("Not sharing"), "the app connected", 60);
            app.Page("NavSharing", "Sharing");
            Env.Until(() => app.Text("SharingStatus") == "Not sharing", "Not sharing on the page");
            using var api = new ControlClient(info);
            // Sharing as it would be on a real PC: every interface, the first free port from 7979.
            app.Press("StartStopButton");
            Env.Until(() => api.GetState().GetAwaiter().GetResult().Sharing, "sharing");
            Env.Until(() => app.Find("LinkQr") is { IsOffscreen: false }, "the QR code");
            app.Shot("sharing");
            app.Page("NavSettings", "Settings");
            app.Shot("settings");
            app.Page("NavAudit", "Audit log");
            app.Shot("audit");
            app.Page("NavSharing", "Sharing");
            app.Press("StartStopButton");
            Env.Until(() => !api.GetState().GetAwaiter().GetResult().Sharing, "stopped");
            Assert.True(File.Exists(Path.Combine(home, "host.log")), "the host's log beside its files");
        }
        catch (Exception)
        {
            output.WriteLine(app.Dump());
            foreach (var f in new[] { "app.log", "host.log" })
                if (File.Exists(Path.Combine(home, f)))
                    output.WriteLine($"--- {f}\n" + File.ReadAllText(Path.Combine(home, f)));
            throw;
        }
        finally
        {
            try { child?.Kill(true); } catch (Exception) { }
        }
    }
}
