using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using ReinRemote.Host.Core;

namespace ReinRemote.Host;

public partial class App : Application
{
    public static HostModel Model { get; private set; } = null!;
    public static MainWindow? Shell { get; private set; }
    static TrayIcon? tray;
    static HostLauncher? launcher;
    static readonly object logGate = new();

    public App()
    {
        InitializeComponent();
        // Nothing the host does takes the app down: a failure is logged and shown where it happened.
        UnhandledException += (_, e) =>
        {
            Log($"unhandled: {e.Exception}");
            e.Handled = true;
        };
        TaskScheduler.UnobservedTaskException += (_, e) =>
        {
            Log($"unobserved: {e.Exception}");
            e.SetObserved();
        };
    }

    public static string HomeFolder() => HostHome.Resolve();

    /// <summary>The app's own log, <c>app.log</c> in the home folder.</summary>
    public static void Log(string line)
    {
        try
        {
            lock (logGate)
            {
                var home = HomeFolder();
                Directory.CreateDirectory(home);
                File.AppendAllText(Path.Combine(home, "app.log"), $"{DateTimeOffset.Now:u} {line}{Environment.NewLine}");
            }
        }
        catch (Exception)
        {
        }
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        var ui = DispatcherQueue.GetForCurrentThread();
        var home = HomeFolder();
        // The host binary: REIN_REMOTE_HOST_EXE (or "none": never start one), else beside this app,
        // else on PATH.
        var exe = Environment.GetEnvironmentVariable("REIN_REMOTE_HOST_EXE");
        if (exe != "none")
        {
            exe = string.IsNullOrEmpty(exe) ? HostLauncher.Find(AppContext.BaseDirectory) : exe;
            if (exe is not null)
                launcher = new HostLauncher(exe);
        }
        Log($"start: home {home}, host binary {launcher?.ExePath ?? "(none)"}");
        Model = new HostModel(ui, new HostConnection(home, launcher));
        Shell = new MainWindow();
        try
        {
            tray = new TrayIcon(Model);
        }
        catch (Exception e)
        {
            Log($"tray: {e}");
        }
        var background = Environment.GetCommandLineArgs().Contains("--background");
        if (!background)
            Shell.Activate();
        SingleInstance.Listen(() => ui.TryEnqueue(ShowWindow));
        Model.Start();
    }

    public static bool HasTray => tray is not null;

    public static void ShowWindow() => Shell?.ShowAndFocus();

    /// <summary>Quit: the tray icon removed, and the host this app started stopped with it.</summary>
    public static void Quit()
    {
        Log("quit");
        tray?.Dispose();
        tray = null;
        launcher?.Stop();
        Shell?.CloseForGood();
        Current.Exit();
    }
}
