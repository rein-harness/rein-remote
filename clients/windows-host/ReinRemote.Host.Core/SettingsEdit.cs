using System.Globalization;
using System.Text.Json.Nodes;

namespace ReinRemote.Host.Core;

/// <summary>
/// The settings form's values as the API takes them (docs/control-api.md, "Settings"): a partial object
/// of only the keys that changed, values typed as the host reads them. The host checks them; its
/// message is shown as it is when it refuses one.
/// </summary>
public static class SettingsEdit
{
    /// <summary><c>"auto"</c>, or a whole number as a number, or the text as typed (the host says why not).</summary>
    public static JsonNode AutoOrNumber(string? text)
    {
        var t = (text ?? "").Trim();
        if (t.Length == 0 || t.Equals("auto", StringComparison.OrdinalIgnoreCase))
            return JsonValue.Create("auto");
        if (long.TryParse(t, NumberStyles.Integer, CultureInfo.InvariantCulture, out var n))
            return JsonValue.Create(n);
        return JsonValue.Create(t);
    }

    /// <summary>A setting's value as the form shows it (<c>auto</c>, <c>60</c>, a path, empty for null).</summary>
    public static string Text(JsonNode? v) => v switch
    {
        null => "",
        JsonValue x when x.TryGetValue<string>(out var s) => s,
        JsonValue x when x.TryGetValue<long>(out var n) => n.ToString(CultureInfo.InvariantCulture),
        JsonValue x when x.TryGetValue<double>(out var d) => d.ToString(CultureInfo.InvariantCulture),
        JsonValue x when x.TryGetValue<bool>(out var b) => b ? "true" : "false",
        _ => v.ToJsonString(),
    };

    public static bool Flag(JsonNode? v, bool fallback = false) =>
        v is JsonValue x && x.TryGetValue<bool>(out var b) ? b : fallback;

    /// <summary>The relays, one per line, as the list the API takes.</summary>
    public static JsonArray Lines(string? text)
    {
        var a = new JsonArray();
        foreach (var l in (text ?? "").Split('\n', '\r', ','))
        {
            var t = l.Trim();
            if (t.Length > 0)
                a.Add(t);
        }
        return a;
    }

    public static string LinesText(JsonNode? v) =>
        v is JsonArray a ? string.Join(Environment.NewLine, a.Select(Text)) : Text(v);

    /// <summary>The keys of <paramref name="edited"/> whose value differs from <paramref name="loaded"/>'s.</summary>
    public static JsonObject Diff(JsonObject loaded, JsonObject edited)
    {
        var changes = new JsonObject();
        foreach (var (k, v) in edited)
        {
            if (!JsonNode.DeepEquals(loaded[k], v))
                changes[k] = v?.DeepClone();
        }
        return changes;
    }

    /// <summary>What <c>applied</c> means, for people.</summary>
    public static string Applied(string? applied) => applied switch
    {
        "saved" => "Saved. They apply the next time sharing starts.",
        "restarted" => "Saved and applied: sharing restarted (no one was watching).",
        "on_restart" => "Saved. Viewers are connected, so they apply when sharing restarts.",
        null => "Saved.",
        _ => $"Saved ({applied}).",
    };
}
