using System.Net;
using System.Text;
using System.Text.Json.Nodes;
using System.Text.RegularExpressions;

namespace ReinRemote.Host.Core.Tests;

/// <summary>The JSON examples in docs/control-api.md, the contract, used as the fixtures.</summary>
static class Doc
{
    static readonly Lazy<List<string>> blocks = new(() =>
    {
        // (A Windows checkout may have CRLF line ends.)
        var text = File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "control-api.md")).Replace("\r\n", "\n");
        return Regex.Matches(text, "```json\n(.*?)```", RegexOptions.Singleline)
            // The audit page's example elides an entry's other keys with "…".
            .Select(m => m.Groups[1].Value.Replace(", …}", "}"))
            .ToList();
    });

    public static IReadOnlyList<string> All => blocks.Value;

    /// <summary>The first example that has <paramref name="marker"/> in it.</summary>
    public static string Json(string marker) =>
        blocks.Value.FirstOrDefault(b => b.Contains(marker)) ?? throw new KeyNotFoundException($"no JSON example with {marker} in control-api.md");

    public static JsonObject Obj(string marker) => (JsonObject)JsonNode.Parse(Json(marker))!;
}

/// <summary>An HttpMessageHandler that answers from a list and keeps what it was asked.</summary>
sealed class Stub : HttpMessageHandler
{
    public readonly List<(HttpMethod Method, string Path, string? Auth, string? Body)> Requests = new();
    public Func<HttpRequestMessage, (HttpStatusCode, string)> Answer = _ => (HttpStatusCode.OK, "{}");

    protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct)
    {
        var body = request.Content is null ? null : await request.Content.ReadAsStringAsync(ct);
        Requests.Add((request.Method, request.RequestUri!.PathAndQuery, request.Headers.Authorization?.ToString(), body));
        var (status, text) = Answer(request);
        return new HttpResponseMessage(status) { Content = new StringContent(text, Encoding.UTF8, "application/json") };
    }
}
