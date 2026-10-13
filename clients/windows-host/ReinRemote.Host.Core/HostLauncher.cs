using System.Diagnostics;

namespace ReinRemote.Host.Core;

/// <summary>
/// Starts <c>rein-remote host</c> as this app's child when no live host is found: the binary next to
/// the app's own (shipped with it), else one on <c>PATH</c>. Its stderr goes to <c>host.log</c> in the
/// home folder, and its last lines are kept to say why it ended.
/// </summary>
public sealed class HostLauncher : IDisposable
{
    public string ExePath { get; }
    public IReadOnlyList<string> Args { get; }
    readonly object gate = new();
    readonly LinkedList<string> tail = new();
    Process? child;
    StreamWriter? log;

    public HostLauncher(string exePath, IEnumerable<string>? args = null)
    {
        ExePath = exePath;
        Args = (args ?? new[] { "host" }).ToList();
    }

    public static string ExeName => OperatingSystem.IsWindows() ? "rein-remote.exe" : "rein-remote";

    /// <summary>The binary beside <paramref name="appDir"/>, else the first on PATH, else null.</summary>
    public static string? Find(string appDir, string? pathVar = null)
    {
        var beside = Path.Combine(appDir, ExeName);
        if (File.Exists(beside))
            return beside;
        pathVar ??= Environment.GetEnvironmentVariable("PATH") ?? "";
        foreach (var dir in pathVar.Split(Path.PathSeparator, StringSplitOptions.RemoveEmptyEntries))
        {
            try
            {
                var p = Path.Combine(dir.Trim('"'), ExeName);
                if (File.Exists(p))
                    return p;
            }
            catch (ArgumentException)
            {
            }
        }
        return null;
    }

    /// <summary>The child this launcher started, while it runs.</summary>
    public Process? Child
    {
        get
        {
            lock (gate)
                return child is { HasExited: false } ? child : null;
        }
    }

    /// <summary>Its last lines on stderr (why it stopped, when it did).</summary>
    public string Tail
    {
        get
        {
            lock (gate)
                return string.Join("\n", tail);
        }
    }

    public int? ExitCode
    {
        get
        {
            lock (gate)
                return child is { HasExited: true } c ? c.ExitCode : null;
        }
    }

    public Process Launch(string home)
    {
        lock (gate)
        {
            if (child is { HasExited: false })
                return child;
            tail.Clear();
            Directory.CreateDirectory(home);
            log?.Dispose();
            try
            {
                log = new StreamWriter(new FileStream(Path.Combine(home, "host.log"), FileMode.Append, FileAccess.Write, FileShare.ReadWrite)) { AutoFlush = true };
            }
            catch (IOException)
            {
                log = null;
            }
            var psi = new ProcessStartInfo(ExePath)
            {
                UseShellExecute = false,
                CreateNoWindow = true,
                RedirectStandardError = true,
                RedirectStandardOutput = true,
                WorkingDirectory = home,
            };
            foreach (var a in Args)
                psi.ArgumentList.Add(a);
            var p = new Process { StartInfo = psi, EnableRaisingEvents = true };
            DataReceivedEventHandler line = (_, e) =>
            {
                if (e.Data is null)
                    return;
                lock (gate)
                {
                    tail.AddLast(e.Data);
                    while (tail.Count > 20)
                        tail.RemoveFirst();
                    try
                    {
                        log?.WriteLine(e.Data);
                    }
                    catch (Exception x) when (x is IOException or ObjectDisposedException)
                    {
                    }
                }
            };
            p.ErrorDataReceived += line;
            p.OutputDataReceived += line;
            p.Start();
            p.BeginErrorReadLine();
            p.BeginOutputReadLine();
            log?.WriteLine($"--- {DateTimeOffset.Now:u} started {ExePath} {string.Join(' ', Args)} (pid {p.Id})");
            child = p;
            return p;
        }
    }

    /// <summary>Ends the child, if this launcher started one and it runs.</summary>
    public void Stop()
    {
        Process? c;
        lock (gate)
            c = child;
        try
        {
            if (c is { HasExited: false })
            {
                c.Kill(entireProcessTree: true);
                c.WaitForExit(5000);
            }
        }
        catch (Exception e) when (e is InvalidOperationException or System.ComponentModel.Win32Exception)
        {
        }
    }

    public void Dispose()
    {
        lock (gate)
        {
            log?.Dispose();
            log = null;
            child?.Dispose();
            child = null;
        }
    }
}
