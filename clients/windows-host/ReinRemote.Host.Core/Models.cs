using System.Text.Json.Nodes;
using System.Text.Json.Serialization;

namespace ReinRemote.Host.Core;

// The control API's objects (docs/control-api.md). Keys that mirror host.toml are written as the
// flags are (`relay-secret`); every other key is snake_case. Objects the app edits as a whole
// (settings, sign-in) or shows key by key (metrics, audit entries) stay as JSON.

/// <summary><c>control.json</c>: where the API is and its token.</summary>
public sealed class ControlInfo
{
    [JsonPropertyName("url")] public string Url { get; set; } = "";
    [JsonPropertyName("token")] public string Token { get; set; } = "";
    [JsonPropertyName("pid")] public int Pid { get; set; }
    [JsonPropertyName("version")] public string? Version { get; set; }
    [JsonPropertyName("api")] public int Api { get; set; }

    public static ControlInfo Parse(string json) =>
        System.Text.Json.JsonSerializer.Deserialize<ControlInfo>(json, Json.Options)
        ?? throw new FormatException("control.json is empty");

    /// <summary>The file in <paramref name="home"/>, or null when there's none (or it's being written).</summary>
    public static ControlInfo? TryRead(string home)
    {
        try
        {
            var path = HostHome.ControlJson(home);
            if (!File.Exists(path))
                return null;
            var info = Parse(File.ReadAllText(path));
            return string.IsNullOrEmpty(info.Url) || string.IsNullOrEmpty(info.Token) ? null : info;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or FormatException or System.Text.Json.JsonException)
        {
            return null;
        }
    }
}

public sealed class Link
{
    [JsonPropertyName("kind")] public string Kind { get; set; } = "";
    [JsonPropertyName("label")] public string Label { get; set; } = "";
    [JsonPropertyName("url")] public string Url { get; set; } = "";
    [JsonPropertyName("qr")] public string? Qr { get; set; }

    public override string ToString() => $"{Label}: {Url}";
}

public sealed class Pairing
{
    [JsonPropertyName("enabled")] public bool Enabled { get; set; }
    [JsonPropertyName("code")] public string? Code { get; set; }
    [JsonPropertyName("expires_at")] public DateTimeOffset? ExpiresAt { get; set; }
    [JsonPropertyName("expires_in_s")] public long? ExpiresInS { get; set; }
    [JsonPropertyName("url")] public string? Url { get; set; }
    [JsonPropertyName("qr")] public string? Qr { get; set; }
    [JsonPropertyName("fingerprint")] public string? Fingerprint { get; set; }
}

public sealed class HostState
{
    [JsonPropertyName("api")] public int Api { get; set; }
    [JsonPropertyName("version")] public string? Version { get; set; }
    [JsonPropertyName("pid")] public int Pid { get; set; }
    [JsonPropertyName("sharing")] public bool Sharing { get; set; }
    [JsonPropertyName("error")] public string? Error { get; set; }
    [JsonPropertyName("source")] public string? Source { get; set; }
    [JsonPropertyName("listen")] public string? Listen { get; set; }
    [JsonPropertyName("links")] public List<Link> Links { get; set; } = new();
    [JsonPropertyName("pairing")] public Pairing Pairing { get; set; } = new();
    [JsonPropertyName("viewers")] public int Viewers { get; set; }
    [JsonPropertyName("restart_needed")] public bool RestartNeeded { get; set; }
}

/// <summary>One thing that can be shared: a display, a window, an app, a test pattern.</summary>
public sealed class SourceItem
{
    [JsonPropertyName("id")] public string Id { get; set; } = "";
    [JsonPropertyName("name")] public string? Name { get; set; }
    [JsonPropertyName("app")] public string? App { get; set; }
    [JsonPropertyName("title")] public string? Title { get; set; }
    [JsonPropertyName("width")] public int? Width { get; set; }
    [JsonPropertyName("height")] public int? Height { get; set; }
    [JsonPropertyName("windows")] public int? Windows { get; set; }

    /// <summary>"Display", "Window", "App" or "Other": the group it's listed in.</summary>
    [JsonIgnore] public string Group { get; set; } = "Other";

    [JsonIgnore]
    public string Display
    {
        get
        {
            var name = Group switch
            {
                "Window" => string.IsNullOrEmpty(App) ? Title ?? Id : string.IsNullOrEmpty(Title) ? App! : $"{App}: {Title}",
                "App" => Windows is int n ? $"{Name} ({n} window{(n == 1 ? "" : "s")})" : Name ?? Id,
                _ => Name ?? Id,
            };
            var size = Width is int w && Height is int h ? $" ({w}×{h})" : "";
            return $"{Group}: {name}{size}";
        }
    }

    public override string ToString() => Display;
}

public sealed class SourceList
{
    [JsonPropertyName("displays")] public List<SourceItem> Displays { get; set; } = new();
    [JsonPropertyName("windows")] public List<SourceItem> Windows { get; set; } = new();
    [JsonPropertyName("apps")] public List<SourceItem> Apps { get; set; } = new();
    [JsonPropertyName("other")] public List<SourceItem> Other { get; set; } = new();
    [JsonPropertyName("error")] public string? Error { get; set; }

    /// <summary>Everything, in the order to list it, each tagged with its group.</summary>
    public List<SourceItem> All()
    {
        foreach (var d in Displays) d.Group = "Display";
        foreach (var w in Windows) w.Group = "Window";
        foreach (var a in Apps) a.Group = "App";
        foreach (var o in Other) o.Group = "Other";
        return Displays.Concat(Windows).Concat(Apps).Concat(Other).ToList();
    }
}

public sealed class Viewer
{
    [JsonPropertyName("id")] public string Id { get; set; } = "";
    [JsonPropertyName("device")] public string? Device { get; set; }
    [JsonPropertyName("user")] public string? User { get; set; }
    [JsonPropertyName("via")] public string? Via { get; set; }
    [JsonPropertyName("transport")] public string? Transport { get; set; }
    [JsonPropertyName("addr")] public string? Addr { get; set; }
    [JsonPropertyName("fps")] public double? Fps { get; set; }
    [JsonPropertyName("rtt_ms")] public double? RttMs { get; set; }
    [JsonPropertyName("kbps")] public double? Kbps { get; set; }
    [JsonPropertyName("width")] public int? Width { get; set; }
    [JsonPropertyName("height")] public int? Height { get; set; }
    [JsonPropertyName("since")] public DateTimeOffset? Since { get; set; }
    [JsonPropertyName("view_only")] public bool ViewOnly { get; set; }

    // GET /control/admin/sessions adds the policy's view.
    [JsonPropertyName("groups")] public List<string>? Groups { get; set; }
    [JsonPropertyName("rule")] public string? Rule { get; set; }
    [JsonPropertyName("clipboard")] public bool? Clipboard { get; set; }
    [JsonPropertyName("audio")] public bool? Audio { get; set; }
    [JsonPropertyName("recorded")] public bool? Recorded { get; set; }
    [JsonPropertyName("ends_at")] public DateTimeOffset? EndsAt { get; set; }

    [JsonIgnore] public string Name => string.IsNullOrEmpty(Device) ? User ?? Id : Device!;
    [JsonIgnore] public string Who => $"{User} · via {Via} · {Transport} · {Addr}";
    [JsonIgnore] public string Stats => Format.ViewerStats(this);
    [JsonIgnore] public string Policy =>
        $"rule {Rule ?? "none"} · groups {(Groups is { Count: > 0 } g ? string.Join(", ", g) : "none")}" +
        $" · clipboard {Format.OnOff(Clipboard)} · audio {Format.OnOff(Audio)} · recorded {Format.OnOff(Recorded)}" +
        (EndsAt is DateTimeOffset e ? $" · ends {e.ToLocalTime():t}" : "") + (ViewOnly ? " · view only" : "");

    public override string ToString() => $"{Name}, {Who}";
}

public sealed class ViewerList
{
    [JsonPropertyName("viewers")] public List<Viewer> Viewers { get; set; } = new();
}

public sealed class SessionList
{
    [JsonPropertyName("sessions")] public List<Viewer> Sessions { get; set; } = new();
}

public sealed class Device
{
    [JsonPropertyName("id")] public string Id { get; set; } = "";
    [JsonPropertyName("name")] public string? Name { get; set; }
    [JsonPropertyName("since")] public DateTimeOffset? Since { get; set; }

    [JsonIgnore] public string SinceText => Since is DateTimeOffset s ? $"paired {s.ToLocalTime():g}" : "";
    public override string ToString() => $"{Name} ({Id})";
}

public sealed class DeviceList
{
    [JsonPropertyName("devices")] public List<Device> Devices { get; set; } = new();
}

public sealed class SettingsResponse
{
    [JsonPropertyName("settings")] public JsonObject Settings { get; set; } = new();
    [JsonPropertyName("sharing")] public bool Sharing { get; set; }
    [JsonPropertyName("source")] public string? Source { get; set; }
    [JsonPropertyName("path")] public string? Path { get; set; }
    [JsonPropertyName("overridden")] public List<string> Overridden { get; set; } = new();
    [JsonPropertyName("restart_needed")] public bool RestartNeeded { get; set; }
    /// <summary>After a PUT: <c>saved</c>, <c>restarted</c> or <c>on_restart</c>.</summary>
    [JsonPropertyName("applied")] public string? Applied { get; set; }
}

public sealed class StartAtLogin
{
    [JsonPropertyName("enabled")] public bool Enabled { get; set; }
    [JsonPropertyName("kind")] public string? Kind { get; set; }
    [JsonPropertyName("name")] public string? Name { get; set; }
    [JsonPropertyName("path")] public string? Path { get; set; }
}

public sealed class Policy
{
    [JsonPropertyName("enabled")] public bool Enabled { get; set; }
    [JsonPropertyName("path")] public string? Path { get; set; }
    [JsonPropertyName("format")] public string? Format { get; set; }
    [JsonPropertyName("rules")] public int Rules { get; set; }
    [JsonPropertyName("text")] public string? Text { get; set; }
    [JsonPropertyName("applied")] public string? Applied { get; set; }
    [JsonPropertyName("restart_needed")] public bool? RestartNeeded { get; set; }
}

public sealed class AuditPage
{
    [JsonPropertyName("path")] public string? Path { get; set; }
    [JsonPropertyName("entries")] public List<JsonObject> Entries { get; set; } = new();
    [JsonPropertyName("next")] public string? Next { get; set; }

    public List<AuditRow> Rows() => Entries.Select(AuditRow.From).ToList();
}

/// <summary>An audit entry as a row: its time, event and user, and the rest as text.</summary>
public sealed class AuditRow
{
    public string Time { get; init; } = "";
    public string Event { get; init; } = "";
    public string User { get; init; } = "";
    public string Details { get; init; } = "";

    public static AuditRow From(JsonObject e)
    {
        string S(string k) => e[k] is JsonValue v && v.TryGetValue<string>(out var s) ? s : e[k]?.ToJsonString() ?? "";
        var ts = S("ts");
        var time = DateTimeOffset.TryParse(ts, out var t) ? t.ToLocalTime().ToString("yyyy-MM-dd HH:mm:ss") : ts;
        var rest = e.Where(kv => kv.Key is not ("ts" or "event" or "user"))
            .Select(kv => $"{kv.Key}={(kv.Value is JsonValue v && v.TryGetValue<string>(out var s) ? s : kv.Value?.ToJsonString())}");
        return new AuditRow { Time = time, Event = S("event"), User = S("user"), Details = string.Join("  ", rest) };
    }

    public override string ToString() => $"{Time} {Event} {User} {Details}";
}

public sealed class AuditQuery
{
    public IReadOnlyList<string> Events { get; init; } = Array.Empty<string>();
    public string? User { get; init; }
    public string? Since { get; init; }
    public int? Limit { get; init; }
    public string? Before { get; init; }

    /// <summary>The query string, <c>?event=a,b&amp;user=…</c> (empty when nothing's set).</summary>
    public string ToQueryString()
    {
        var q = new List<string>();
        void Add(string k, string? v)
        {
            if (!string.IsNullOrWhiteSpace(v))
                q.Add($"{k}={Uri.EscapeDataString(v.Trim())}");
        }
        var events = Events.Select(e => e.Trim()).Where(e => e.Length > 0).ToList();
        if (events.Count > 0)
            q.Add("event=" + string.Join(",", events.Select(Uri.EscapeDataString)));
        Add("user", User);
        Add("since", Since);
        if (Limit is int l)
            q.Add($"limit={l}");
        Add("before", Before);
        return q.Count == 0 ? "" : "?" + string.Join("&", q);
    }
}

public sealed class Permissions
{
    [JsonPropertyName("platform")] public string? Platform { get; set; }
    [JsonPropertyName("screen_recording")] public string? ScreenRecording { get; set; }
    [JsonPropertyName("accessibility")] public string? Accessibility { get; set; }
}

/// <summary>A key and its value, for pages that list an object as it is (metrics).</summary>
public sealed record KeyValueRow(string Key, string Value)
{
    public override string ToString() => $"{Key}: {Value}";
}
