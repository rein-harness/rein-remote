using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;

namespace ReinRemote.Host;

public static class Program
{
    [STAThread]
    static int Main(string[] args)
    {
        WinRT.ComWrappersSupport.InitializeComWrappers();
        // One app per home folder: a second start shows the first one's window and leaves.
        if (!SingleInstance.Claim(App.HomeFolder()))
            return 0;
        Application.Start(_ =>
        {
            var context = new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread());
            SynchronizationContext.SetSynchronizationContext(context);
            _ = new App();
        });
        return 0;
    }
}
