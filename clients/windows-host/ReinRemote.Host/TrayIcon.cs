using System.ComponentModel;
using System.Runtime.InteropServices;
using ReinRemote.Host.Core;

namespace ReinRemote.Host;

/// <summary>
/// The notification-area icon (Win32 <c>Shell_NotifyIcon</c>) and its menu: the status, start or stop,
/// the pairing code, the viewers, Open and Quit. Its window and menu live on the UI thread, so its
/// commands call the model directly. The icon shows whether sharing is on; a balloon says when
/// someone starts watching.
/// </summary>
sealed class TrayIcon : IDisposable
{
    const uint CallbackMessage = 0x8000 + 1; // WM_APP + 1
    const int IdStatus = 1, IdViewers = 2, IdCode = 3, IdStartStop = 4, IdNewCode = 5, IdOpen = 6, IdQuit = 7;

    readonly HostModel model;
    readonly WndProc proc;
    readonly IntPtr hwnd;
    readonly IntPtr iconIdle, iconSharing;
    readonly uint taskbarCreated;
    readonly string className = "ReinRemoteTray" + Environment.ProcessId;
    bool added;
    int lastViewers;
    string lastTip = "";
    IntPtr lastIcon;

    public TrayIcon(HostModel model)
    {
        this.model = model;
        proc = WindowProc;
        var module = GetModuleHandle(null);
        var wc = new WNDCLASSEX
        {
            cbSize = Marshal.SizeOf<WNDCLASSEX>(),
            lpfnWndProc = Marshal.GetFunctionPointerForDelegate(proc),
            hInstance = module,
            lpszClassName = className,
        };
        if (RegisterClassEx(ref wc) == 0)
            throw new Win32Exception();
        // A hidden top-level window (never shown): the icon's messages come to it, and the menu needs
        // a window to take the foreground.
        hwnd = CreateWindowEx(0, className, "Rein Remote tray", 0, 0, 0, 0, 0, IntPtr.Zero, IntPtr.Zero, module, IntPtr.Zero);
        if (hwnd == IntPtr.Zero)
            throw new Win32Exception();
        taskbarCreated = RegisterWindowMessage("TaskbarCreated");
        var dir = Path.Combine(AppContext.BaseDirectory, "Assets");
        iconIdle = LoadIcon(Path.Combine(dir, "tray-idle.ico"));
        iconSharing = LoadIcon(Path.Combine(dir, "tray-sharing.ico"));
        Update();
        model.StateChanged += Update;
        model.StatusChanged += Update;
        model.ViewersChanged += OnViewers;
    }

    static IntPtr LoadIcon(string path)
    {
        var size = GetSystemMetrics(49); // SM_CXSMICON
        var h = LoadImage(IntPtr.Zero, path, 1 /* IMAGE_ICON */, size, size, 0x10 /* LR_LOADFROMFILE */);
        return h != IntPtr.Zero ? h : LoadImage(IntPtr.Zero, new IntPtr(32512) /* IDI_APPLICATION */, 1, 0, 0, 0x8000 /* LR_SHARED */);
    }

    /// <summary>What the icon's tooltip and the menu's first line say.</summary>
    public static string StatusText(HostModel m)
    {
        if (m.Status != LinkStatus.Connected || m.State is null)
            return m.Status switch
            {
                LinkStatus.Starting => "Starting rein-remote host…",
                LinkStatus.Connecting => "Connecting to rein-remote host…",
                _ => "Not connected to rein-remote host",
            };
        var s = m.State;
        if (!s.Sharing)
            return s.Error is { Length: > 0 } e ? $"Not sharing: {e}" : "Not sharing";
        return s.Viewers switch
        {
            0 => "Sharing, no one watching",
            1 => "Sharing with 1 viewer",
            var n => $"Sharing with {n} viewers",
        };
    }

    void Update()
    {
        var tip = "Rein Remote: " + StatusText(model);
        if (tip.Length > 127)
            tip = tip[..127];
        var icon = model.State?.Sharing == true && model.Status == LinkStatus.Connected ? iconSharing : iconIdle;
        if (added && tip == lastTip && icon == lastIcon)
            return;
        var data = Data(0x1 | 0x2 | 0x4 /* NIF_MESSAGE | NIF_ICON | NIF_TIP */);
        data.hIcon = icon;
        data.szTip = tip;
        added = Shell_NotifyIcon(added ? 1u /* NIM_MODIFY */ : 0u /* NIM_ADD */, ref data) || added;
        lastTip = tip;
        lastIcon = icon;
    }

    void OnViewers()
    {
        var list = model.Viewers.Viewers;
        if (list.Count > lastViewers && list.Count > 0 && added)
        {
            var data = Data(0x10 /* NIF_INFO */);
            data.szInfoTitle = "Rein Remote";
            data.szInfo = $"{list[^1].Name} is watching this screen.";
            data.dwInfoFlags = 0x1 | 0x10; // NIIF_INFO | NIIF_NOSOUND
            Shell_NotifyIcon(1, ref data);
        }
        lastViewers = list.Count;
        Update();
    }

    NOTIFYICONDATA Data(uint flags) => new()
    {
        cbSize = Marshal.SizeOf<NOTIFYICONDATA>(),
        hWnd = hwnd,
        uID = 1,
        uFlags = flags,
        uCallbackMessage = CallbackMessage,
        szTip = "",
        szInfo = "",
        szInfoTitle = "",
    };

    IntPtr WindowProc(IntPtr h, uint msg, IntPtr wParam, IntPtr lParam)
    {
        try
        {
            if (msg == CallbackMessage)
            {
                switch ((uint)lParam.ToInt64() & 0xffff)
                {
                    case 0x0202: // WM_LBUTTONUP
                    case 0x0203: // WM_LBUTTONDBLCLK
                        App.ShowWindow();
                        break;
                    case 0x0205: // WM_RBUTTONUP
                        ShowMenu();
                        break;
                }
                return IntPtr.Zero;
            }
            if (msg == taskbarCreated && taskbarCreated != 0)
            {
                // Explorer restarted: the icon is added again.
                added = false;
                Update();
            }
        }
        catch (Exception e)
        {
            App.Log($"tray: {e}");
        }
        return DefWindowProc(h, msg, wParam, lParam);
    }

    void ShowMenu()
    {
        var s = model.State;
        var connected = model.Status == LinkStatus.Connected && s is not null;
        var menu = CreatePopupMenu();
        const uint Grayed = 0x1, Separator = 0x800;
        AppendMenu(menu, Grayed, IdStatus, StatusText(model));
        if (connected)
        {
            AppendMenu(menu, Grayed, IdViewers, s!.Viewers == 1 ? "1 viewer" : $"{s.Viewers} viewers");
            var p = s.Pairing;
            if (s.Sharing && p.Enabled && p.Code is { } code)
                AppendMenu(menu, 0, IdCode, $"Pairing code {code} ({Format.Countdown(p.ExpiresAt, DateTimeOffset.UtcNow)} left), copy");
            else
                AppendMenu(menu, Grayed, IdCode, s.Sharing ? (p.Enabled ? "No pairing code" : "Pairing is off") : "Pairing code: share first");
            AppendMenu(menu, Separator, 0, null);
            AppendMenu(menu, 0, IdStartStop, s.Sharing ? "Stop sharing" : "Start sharing");
            AppendMenu(menu, s.Sharing && p.Enabled ? 0 : Grayed, IdNewCode, "New pairing code");
        }
        AppendMenu(menu, Separator, 0, null);
        AppendMenu(menu, 0, IdOpen, "Open Rein Remote");
        AppendMenu(menu, 0, IdQuit, "Quit Rein Remote");
        GetCursorPos(out var pt);
        SetForegroundWindow(hwnd);
        var cmd = TrackPopupMenuEx(menu, 0x0100 /* TPM_RETURNCMD */ | 0x0002 /* TPM_RIGHTBUTTON */, pt.X, pt.Y, hwnd, IntPtr.Zero);
        PostMessage(hwnd, 0, IntPtr.Zero, IntPtr.Zero);
        DestroyMenu(menu);
        _ = Command(cmd);
    }

    async Task Command(int cmd)
    {
        switch (cmd)
        {
            case IdOpen:
                App.ShowWindow();
                break;
            case IdQuit:
                App.Quit();
                break;
            case IdCode when model.State?.Pairing.Code is { } code:
                var data = new Windows.ApplicationModel.DataTransfer.DataPackage();
                data.SetText(code);
                Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(data);
                break;
            case IdStartStop:
                var sharing = model.State?.Sharing == true;
                var (state, error) = await model.Call(c => sharing ? c.StopSharing() : c.StartSharing());
                model.Take(state);
                if (error is not null)
                    Balloon(error);
                break;
            case IdNewCode:
                var (_, e) = await model.Call(c => c.NewCode());
                if (e is not null)
                    Balloon(e);
                break;
        }
    }

    void Balloon(string text)
    {
        var data = Data(0x10);
        data.szInfoTitle = "Rein Remote";
        data.szInfo = text.Length > 255 ? text[..255] : text;
        data.dwInfoFlags = 0x3 | 0x10; // NIIF_ERROR | NIIF_NOSOUND
        Shell_NotifyIcon(1, ref data);
    }

    public void Dispose()
    {
        model.StateChanged -= Update;
        model.StatusChanged -= Update;
        model.ViewersChanged -= OnViewers;
        if (added)
        {
            var data = Data(0);
            Shell_NotifyIcon(2 /* NIM_DELETE */, ref data);
            added = false;
        }
        DestroyWindow(hwnd);
        UnregisterClass(className, GetModuleHandle(null));
        DestroyIcon(iconIdle);
        DestroyIcon(iconSharing);
    }

    delegate IntPtr WndProc(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct WNDCLASSEX
    {
        public int cbSize;
        public uint style;
        public IntPtr lpfnWndProc;
        public int cbClsExtra;
        public int cbWndExtra;
        public IntPtr hInstance;
        public IntPtr hIcon;
        public IntPtr hCursor;
        public IntPtr hbrBackground;
        public string? lpszMenuName;
        public string lpszClassName;
        public IntPtr hIconSm;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct NOTIFYICONDATA
    {
        public int cbSize;
        public IntPtr hWnd;
        public uint uID;
        public uint uFlags;
        public uint uCallbackMessage;
        public IntPtr hIcon;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string szTip;
        public uint dwState;
        public uint dwStateMask;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string szInfo;
        public uint uVersion;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 64)] public string szInfoTitle;
        public uint dwInfoFlags;
        public Guid guidItem;
        public IntPtr hBalloonIcon;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct POINT
    {
        public int X, Y;
    }

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern ushort RegisterClassEx(ref WNDCLASSEX wc);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern bool UnregisterClass(string cls, IntPtr hInstance);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern IntPtr CreateWindowEx(uint exStyle, string cls, string name, uint style, int x, int y, int w, int h, IntPtr parent, IntPtr menu, IntPtr instance, IntPtr param);
    [DllImport("user32.dll")] static extern bool DestroyWindow(IntPtr hwnd);
    [DllImport("user32.dll")] static extern IntPtr DefWindowProc(IntPtr hwnd, uint msg, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern uint RegisterWindowMessage(string name);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern IntPtr GetModuleHandle(string? name);
    [DllImport("shell32.dll", CharSet = CharSet.Unicode)] static extern bool Shell_NotifyIcon(uint msg, ref NOTIFYICONDATA data);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr LoadImage(IntPtr inst, string name, uint type, int cx, int cy, uint load);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr LoadImage(IntPtr inst, IntPtr name, uint type, int cx, int cy, uint load);
    [DllImport("user32.dll")] static extern bool DestroyIcon(IntPtr icon);
    [DllImport("user32.dll")] static extern int GetSystemMetrics(int index);
    [DllImport("user32.dll")] static extern IntPtr CreatePopupMenu();
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern bool AppendMenu(IntPtr menu, uint flags, nint id, string? text);
    [DllImport("user32.dll")] static extern bool DestroyMenu(IntPtr menu);
    [DllImport("user32.dll")] static extern int TrackPopupMenuEx(IntPtr menu, uint flags, int x, int y, IntPtr hwnd, IntPtr tpm);
    [DllImport("user32.dll")] static extern bool GetCursorPos(out POINT p);
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool PostMessage(IntPtr hwnd, uint msg, IntPtr wParam, IntPtr lParam);
}
