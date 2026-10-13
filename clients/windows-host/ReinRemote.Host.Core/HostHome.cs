namespace ReinRemote.Host.Core;

/// <summary>
/// The host's home folder (docs/control-api.md, "Files"): <c>REIN_REMOTE_HOME</c> when set, else
/// <c>%APPDATA%\rein-remote</c> on Windows and <c>~/.rein-remote</c> elsewhere. The same rule as the
/// server's (crates/rr-client/src/keys.rs).
/// </summary>
public static class HostHome
{
    public static string Resolve(Func<string, string?>? env = null, bool? windows = null)
    {
        env ??= Environment.GetEnvironmentVariable;
        var home = env("REIN_REMOTE_HOME");
        if (!string.IsNullOrEmpty(home))
            return home;
        if (windows ?? OperatingSystem.IsWindows())
        {
            var appData = env("APPDATA");
            if (!string.IsNullOrEmpty(appData))
                return Path.Combine(appData, "rein-remote");
        }
        var user = env("HOME");
        if (string.IsNullOrEmpty(user))
            user = env("USERPROFILE");
        return Path.Combine(string.IsNullOrEmpty(user) ? "." : user, ".rein-remote");
    }

    public static string ControlJson(string home) => Path.Combine(home, "control.json");
    public static string HostToml(string home) => Path.Combine(home, "host.toml");
}
