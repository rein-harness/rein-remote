using System.Diagnostics;
using System.Text;
using FlaUI.Core;
using FlaUI.Core.AutomationElements;
using FlaUI.Core.Capturing;
using FlaUI.Core.Definitions;
using FlaUI.UIA3;
using ReinRemote.Host.Core;
using Xunit.Abstractions;

namespace ReinRemote.Host.UiTests;

static class Env
{
    public static string Server => Need("REIN_REMOTE_EXE");
    public static string App => Need("REIN_REMOTE_APP");
    public static string Shots => Environment.GetEnvironmentVariable("REIN_REMOTE_SCREENSHOTS") is { Length: > 0 } s ? s : Path.Combine(Path.GetTempPath(), "rr-app-shots");

    static string Need(string name) =>
        Environment.GetEnvironmentVariable(name) is { Length: > 0 } v && File.Exists(v) ? v
            : throw new InvalidOperationException($"{name} must name the file (it's {Environment.GetEnvironmentVariable(name) ?? "unset"})");

    public static string NewHome(string name)
    {
        var root = Environment.GetEnvironmentVariable("REIN_REMOTE_TEST_HOMES") is { Length: > 0 } r ? r : Path.GetTempPath();
        var d = Path.Combine(root, $"rr-app-{name}-{Environment.ProcessId}-{DateTime.UtcNow:HHmmss}");
        if (Directory.Exists(d))
            Directory.Delete(d, true);
        Directory.CreateDirectory(d);
        return d;
    }

    /// <summary>Polls <paramref name="f"/> until it gives something (or true), or fails the test saying what it waited for.</summary>
    public static T Until<T>(Func<T?> f, string what, double seconds = 30)
    {
        var end = DateTime.UtcNow.AddSeconds(seconds);
        Exception? last = null;
        while (DateTime.UtcNow < end)
        {
            try
            {
                var v = f();
                if (v is not null && !(v is bool b && !b))
                    return v;
            }
            catch (Exception e)
            {
                last = e;
            }
            Thread.Sleep(200);
        }
        throw new TimeoutException($"timed out ({seconds} s) waiting for {what}{(last is null ? "" : $" (last error: {last.Message})")}");
    }
}

/// <summary>A real <c>rein-remote host</c> on a home folder of its own, and a client for its API (the test's oracle).</summary>
sealed class HostProcess : IDisposable
{
    public Process Proc { get; }
    public string Home { get; }
    public ControlClient Api { get; }
    readonly StringBuilder log = new();

    HostProcess(Process p, string home, ControlClient api)
    {
        Proc = p;
        Home = home;
        Api = api;
    }

    public string Log
    {
        get
        {
            lock (log)
                return log.ToString();
        }
    }

    public static HostProcess Start(string home, ITestOutputHelper output, params string[] args)
    {
        var before = ControlInfo.TryRead(home)?.Pid;
        var psi = new ProcessStartInfo(Env.Server) { UseShellExecute = false, RedirectStandardError = true, RedirectStandardOutput = true, CreateNoWindow = true };
        psi.ArgumentList.Add("host");
        foreach (var a in args)
            psi.ArgumentList.Add(a);
        psi.Environment["REIN_REMOTE_HOME"] = home;
        var p = new Process { StartInfo = psi };
        var sb = new StringBuilder();
        var ready = new ManualResetEventSlim();
        void Line(object _, DataReceivedEventArgs e)
        {
            if (e.Data is null)
                return;
            lock (sb)
                sb.AppendLine(e.Data);
            if (e.Data.Contains("control API on http://"))
                ready.Set();
        }
        p.ErrorDataReceived += Line;
        p.OutputDataReceived += Line;
        p.Start();
        p.BeginErrorReadLine();
        p.BeginOutputReadLine();
        if (!ready.Wait(TimeSpan.FromSeconds(60)))
        {
            try { p.Kill(true); } catch (Exception) { }
            throw new TimeoutException("rein-remote host didn't start its control API:\n" + sb);
        }
        var info = Env.Until(() => ControlInfo.TryRead(home) is { } i && i.Pid == p.Id ? i : null, "control.json");
        output.WriteLine($"host pid {p.Id} (before: {before?.ToString() ?? "none"}), API {info.Url}");
        var h = new HostProcess(p, home, new ControlClient(info));
        lock (sb)
            h.log.Append(sb);
        p.ErrorDataReceived += (_, e) => { if (e.Data is not null) lock (h.log) h.log.AppendLine(e.Data); };
        return h;
    }

    public T Get<T>(Func<ControlClient, Task<T>> f) => f(Api).GetAwaiter().GetResult();

    public void Kill()
    {
        try
        {
            if (!Proc.HasExited)
            {
                Proc.Kill(true);
                Proc.WaitForExit(10000);
            }
        }
        catch (Exception)
        {
        }
    }

    public void Dispose()
    {
        Kill();
        Api.Dispose();
    }
}

/// <summary>The app, started with <c>REIN_REMOTE_HOME</c> set, and its window through UI Automation.</summary>
sealed class AppSession : IDisposable
{
    public Application App { get; }
    public UIA3Automation Automation { get; } = new();
    public Window Window { get; private set; }
    readonly ITestOutputHelper output;
    readonly string theme;

    AppSession(Application app, ITestOutputHelper output, string theme)
    {
        App = app;
        this.output = output;
        this.theme = theme;
        Window = app.GetMainWindow(Automation, TimeSpan.FromSeconds(90)) ?? throw new TimeoutException("the app's window didn't appear");
        output.WriteLine($"app pid {app.ProcessId}, window \"{Window.Title}\"");
    }

    public static AppSession Launch(string home, ITestOutputHelper output, string theme = "light", Dictionary<string, string>? env = null)
    {
        var psi = new ProcessStartInfo(Env.App) { UseShellExecute = false, WorkingDirectory = Path.GetDirectoryName(Env.App) };
        psi.Environment["REIN_REMOTE_HOME"] = home;
        psi.Environment["REIN_REMOTE_THEME"] = theme;
        foreach (var (k, v) in env ?? new())
            psi.Environment[k] = v;
        return new AppSession(Application.Launch(psi), output, theme);
    }

    public bool Running => !App.HasExited;

    public AutomationElement? Find(string automationId) =>
        Window.FindFirstDescendant(cf => cf.ByAutomationId(automationId));

    public AutomationElement ById(string automationId, double seconds = 20) =>
        Env.Until(() => Find(automationId) is { IsOffscreen: false } e ? e : Find(automationId), $"the element {automationId}", seconds);

    /// <summary>The element's text: a text block's name, an edit's value.</summary>
    public string Text(string automationId)
    {
        var e = ById(automationId);
        return e.Patterns.Value.IsSupported ? e.Patterns.Value.Pattern.Value.Value ?? "" : e.Name ?? "";
    }

    public void Press(string automationId)
    {
        var e = ById(automationId);
        Env.Until(() => e.IsEnabled, $"{automationId} enabled");
        if (e.Patterns.Invoke.IsSupported)
            e.Patterns.Invoke.Pattern.Invoke();
        else if (e.Patterns.Toggle.IsSupported)
            e.Patterns.Toggle.Pattern.Toggle();
        else if (e.Patterns.SelectionItem.IsSupported)
            e.Patterns.SelectionItem.Pattern.Select();
        else
            e.Click();
    }

    public void Type(string automationId, string text)
    {
        var e = ById(automationId);
        if (e.Patterns.Value.IsSupported)
        {
            e.Patterns.Value.Pattern.SetValue(text);
        }
        else
        {
            e.Focus();
            FlaUI.Core.Input.Keyboard.TypeSimultaneously(FlaUI.Core.WindowsAPI.VirtualKeyShort.CONTROL, FlaUI.Core.WindowsAPI.VirtualKeyShort.KEY_A);
            FlaUI.Core.Input.Keyboard.Type(text);
        }
    }

    /// <summary>Opens a page from the navigation pane.</summary>
    public void Page(string nav, string title)
    {
        var item = ById(nav);
        if (item.Patterns.SelectionItem.IsSupported)
            item.Patterns.SelectionItem.Pattern.Select();
        else
            item.Patterns.Invoke.Pattern.Invoke();
        Env.Until(() => Window.FindAllDescendants(cf => cf.ByAutomationId("PageTitle")).Any(t => t.Name == title && !t.IsOffscreen), $"the {title} page");
    }

    public void Shot(string name)
    {
        Directory.CreateDirectory(Env.Shots);
        try
        {
            Window.SetForeground();
        }
        catch (Exception)
        {
        }
        Thread.Sleep(700);
        var path = Path.Combine(Env.Shots, $"{name}-{theme}.png");
        Capture.Element(Window).ToFile(path);
        output.WriteLine($"screenshot {path}");
    }

    /// <summary>Everything in the window (names, ids, types), for the log when something's not found.</summary>
    public string Dump()
    {
        var sb = new StringBuilder();
        void Walk(AutomationElement e, int depth)
        {
            if (depth > 30)
                return;
            string id = "", name = "";
            try { id = e.AutomationId; name = e.Name; } catch (Exception) { }
            sb.AppendLine($"{new string(' ', depth * 2)}{e.ControlType} id={id} name={name}");
            foreach (var c in e.FindAllChildren())
                Walk(c, depth + 1);
        }
        try
        {
            Walk(Window, 0);
        }
        catch (Exception x)
        {
            sb.AppendLine($"(dump failed: {x.Message})");
        }
        return sb.ToString();
    }

    public void Dispose()
    {
        try
        {
            App.Kill();
        }
        catch (Exception)
        {
        }
        Automation.Dispose();
        App.Dispose();
    }
}
