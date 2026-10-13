using System.Globalization;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace ReinRemote.Host.Core;

internal static class Json
{
    public static readonly JsonSerializerOptions Options = new()
    {
        PropertyNameCaseInsensitive = false,
        NumberHandling = System.Text.Json.Serialization.JsonNumberHandling.AllowReadingFromString,
    };
}

/// <summary>Text for people, from the API's values.</summary>
public static class Format
{
    public static string OnOff(bool? b) => b switch { true => "on", false => "off", null => "–" };

    /// <summary>A pairing code's time left: <c>9:05</c>, or <c>expired</c>.</summary>
    public static string Countdown(DateTimeOffset? expiresAt, DateTimeOffset now)
    {
        if (expiresAt is not DateTimeOffset e)
            return "";
        var left = e - now;
        if (left <= TimeSpan.Zero)
            return "expired";
        var s = (int)Math.Ceiling(left.TotalSeconds);
        return $"{s / 60}:{s % 60:00}";
    }

    public static string ViewerStats(Viewer v)
    {
        var parts = new List<string>();
        if (v.Fps is double f) parts.Add($"{f:0} fps");
        if (v.Width is int w && v.Height is int h) parts.Add($"{w}×{h}");
        if (v.Kbps is double k) parts.Add(k >= 1000 ? $"{k / 1000:0.#} Mbit/s" : $"{k:0} kbit/s");
        if (v.RttMs is double r) parts.Add($"{r:0.#} ms round trip");
        if (v.Since is DateTimeOffset s) parts.Add($"since {s.ToLocalTime():t}");
        return string.Join(" · ", parts);
    }

    public static string Bytes(double b)
    {
        string[] units = { "B", "KB", "MB", "GB", "TB" };
        var i = 0;
        while (b >= 1024 && i < units.Length - 1)
        {
            b /= 1024;
            i++;
        }
        return i == 0 ? $"{b:0} B" : $"{b:0.0} {units[i]}";
    }

    public static string Duration(double seconds)
    {
        var t = TimeSpan.FromSeconds(Math.Max(0, seconds));
        return t.TotalDays >= 1 ? $"{(int)t.TotalDays}d {t.Hours}h {t.Minutes}m"
            : t.TotalHours >= 1 ? $"{t.Hours}h {t.Minutes}m {t.Seconds}s"
            : t.TotalMinutes >= 1 ? $"{t.Minutes}m {t.Seconds}s"
            : $"{t.Seconds}s";
    }

    /// <summary>The metrics object (GET /control/admin/metrics) as rows, each key as it is.</summary>
    public static List<KeyValueRow> MetricsRows(JsonObject m)
    {
        var rows = new List<KeyValueRow>();
        void Walk(string prefix, JsonNode? node)
        {
            switch (node)
            {
                case JsonObject o:
                    foreach (var (k, v) in o)
                        Walk(prefix.Length == 0 ? k : $"{prefix}.{k}", v);
                    break;
                case JsonArray a:
                    if (a.Count == 0)
                        rows.Add(new KeyValueRow(prefix, "none"));
                    for (var i = 0; i < a.Count; i++)
                        Walk($"{prefix}[{i}]", a[i]);
                    break;
                case JsonValue v:
                    rows.Add(new KeyValueRow(prefix, MetricValue(prefix, v)));
                    break;
                default:
                    rows.Add(new KeyValueRow(prefix, "–"));
                    break;
            }
        }
        Walk("", m);
        return rows;
    }

    static string MetricValue(string key, JsonValue v)
    {
        if (v.TryGetValue<double>(out var d))
        {
            if (key.EndsWith("bytes_sent_total", StringComparison.Ordinal))
                return $"{Bytes(d)} ({d.ToString("N0", CultureInfo.InvariantCulture)})";
            if (key == "uptime_s")
                return Duration(d);
            return d == Math.Floor(d) ? d.ToString("N0", CultureInfo.InvariantCulture) : d.ToString("0.##", CultureInfo.InvariantCulture);
        }
        if (v.TryGetValue<bool>(out var b))
            return b ? "yes" : "no";
        return v.TryGetValue<string>(out var s) ? s : v.ToJsonString();
    }
}
