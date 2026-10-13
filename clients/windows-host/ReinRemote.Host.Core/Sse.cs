using System.Runtime.CompilerServices;
using System.Text;

namespace ReinRemote.Host.Core;

public sealed record SseEvent(string Event, string Data);

/// <summary>
/// Server-Sent Events read line by line (docs/control-api.md, "Events"): <c>event:</c> and <c>data:</c>
/// lines, a blank line ends an event, <c>:</c> lines are comments (the host's keepalive).
/// </summary>
public static class Sse
{
    /// <param name="idle">The longest wait for a line (the host sends a keepalive every 15 s); after
    /// it, the stream is taken for dead and this ends with a <see cref="TimeoutException"/>.</param>
    public static async IAsyncEnumerable<SseEvent> Read(Stream stream, TimeSpan idle, [EnumeratorCancellation] CancellationToken ct = default)
    {
        using var reader = new StreamReader(stream, Encoding.UTF8);
        using var timer = CancellationTokenSource.CreateLinkedTokenSource(ct);
        string ev = "message";
        var data = new StringBuilder();
        var any = false;
        while (true)
        {
            timer.CancelAfter(idle);
            string? line;
            try
            {
                line = await reader.ReadLineAsync(timer.Token).ConfigureAwait(false);
            }
            catch (OperationCanceledException) when (!ct.IsCancellationRequested)
            {
                throw new TimeoutException($"no word from the host in {idle.TotalSeconds:0} s");
            }
            if (line is null)
                yield break;
            if (line.Length == 0)
            {
                if (any)
                    yield return new SseEvent(ev, data.ToString());
                ev = "message";
                data.Clear();
                any = false;
                continue;
            }
            if (line[0] == ':')
                continue;
            var colon = line.IndexOf(':');
            var field = colon < 0 ? line : line[..colon];
            var value = colon < 0 ? "" : line[(colon + 1)..];
            if (value.StartsWith(' '))
                value = value[1..];
            switch (field)
            {
                case "event":
                    ev = value;
                    any = true;
                    break;
                case "data":
                    if (data.Length > 0)
                        data.Append('\n');
                    data.Append(value);
                    any = true;
                    break;
            }
        }
    }
}
