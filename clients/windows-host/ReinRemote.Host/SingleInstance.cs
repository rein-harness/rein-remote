using System.Security.Cryptography;
using System.Text;

namespace ReinRemote.Host;

/// <summary>
/// One app per home folder (a named mutex in this session). A second start signals the first, which
/// shows its window, and exits.
/// </summary>
static class SingleInstance
{
    static Mutex? mutex;
    static EventWaitHandle? show;

    static string Key(string home)
    {
        var hash = SHA256.HashData(Encoding.UTF8.GetBytes(Path.GetFullPath(home).ToLowerInvariant()));
        return "rein-remote-host-app-" + Convert.ToHexString(hash, 0, 8);
    }

    public static bool Claim(string home)
    {
        var key = Key(home);
        mutex = new Mutex(true, @"Local\" + key, out var first);
        show = new EventWaitHandle(false, EventResetMode.AutoReset, @"Local\" + key + "-show");
        if (!first)
        {
            show.Set();
            return false;
        }
        return true;
    }

    /// <summary>Calls <paramref name="onShow"/> (on a worker thread) each time another start asks.</summary>
    public static void Listen(Action onShow)
    {
        var h = show;
        if (h is null)
            return;
        new Thread(() =>
        {
            while (h.WaitOne())
                onShow();
        }) { IsBackground = true, Name = "single instance" }.Start();
    }
}
