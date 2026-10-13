using Xunit;
using Xunit.Abstractions;

namespace ReinRemote.Host.UiTests;

public class AppTests
{
    readonly ITestOutputHelper output;

    public AppTests(ITestOutputHelper output) => this.output = output;

    [Fact]
    public void The_app_finds_the_host_and_shows_each_page()
    {
        var home = Env.NewHome("pages");
        using var host = HostProcess.Start(home, output, "--test-pattern", "1280x720", "--listen", "127.0.0.1:0", "--mdns", "off");
        using var app = AppSession.Launch(home, output, env: new() { ["REIN_REMOTE_HOST_EXE"] = "none" });
        try
        {
            Env.Until(() => app.Window.Title.Contains("Not sharing"), "the app connected (its title says Not sharing)", 60);
            foreach (var (nav, title) in new[] { ("NavSharing", "Sharing"), ("NavViewers", "Viewers"), ("NavDevices", "Paired devices"), ("NavSettings", "Settings"), ("NavSignIn", "Sign-in"), ("NavPolicy", "Policy"), ("NavSessions", "Live sessions"), ("NavAudit", "Audit log"), ("NavMetrics", "Metrics") })
            {
                app.Page(nav, title);
                app.Shot(title.ToLowerInvariant().Replace(' ', '-'));
            }
        }
        catch (Exception)
        {
            output.WriteLine(app.Dump());
            output.WriteLine(host.Log);
            throw;
        }
    }
}
