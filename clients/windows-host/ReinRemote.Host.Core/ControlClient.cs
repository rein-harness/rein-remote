using System.Net;
using System.Net.Http.Headers;
using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace ReinRemote.Host.Core;

/// <summary>
/// A request the host refused, or one that didn't reach it. <see cref="Message"/> is the host's own
/// <c>{"error"}</c> text, written to be shown as it is; <see cref="Status"/> is 0 when the host
/// couldn't be reached.
/// </summary>
public sealed class ControlException : Exception
{
    public int Status { get; }

    public ControlException(int status, string message, Exception? inner = null) : base(message, inner) => Status = status;

    public bool Unreachable => Status == 0;
}

/// <summary>
/// The control API's client (docs/control-api.md): the bearer token from <c>control.json</c> on every
/// request, JSON both ways, each error as a <see cref="ControlException"/> with the host's message.
/// </summary>
public sealed class ControlClient : IDisposable
{
    readonly HttpClient http;
    readonly HttpClient events;
    public ControlInfo Info { get; }

    public ControlClient(ControlInfo info, HttpMessageHandler? handler = null, TimeSpan? timeout = null)
    {
        Info = info;
        var baseUri = new Uri(info.Url.TrimEnd('/') + "/");
        HttpClient Make(TimeSpan t)
        {
            var c = handler is null ? new HttpClient(new SocketsHttpHandler { UseProxy = false, ConnectTimeout = TimeSpan.FromSeconds(5) }) : new HttpClient(handler, disposeHandler: false);
            c.BaseAddress = baseUri;
            c.Timeout = t;
            c.DefaultRequestHeaders.Authorization = new AuthenticationHeaderValue("Bearer", info.Token);
            c.DefaultRequestHeaders.Accept.Add(new MediaTypeWithQualityHeaderValue("application/json"));
            return c;
        }
        http = Make(timeout ?? TimeSpan.FromSeconds(30));
        events = Make(Timeout.InfiniteTimeSpan);
    }

    public void Dispose()
    {
        http.Dispose();
        events.Dispose();
    }

    async Task<string> Send(HttpMethod method, string path, JsonNode? body, CancellationToken ct)
    {
        using var req = new HttpRequestMessage(method, path.TrimStart('/'));
        if (body is not null)
            req.Content = new StringContent(body.ToJsonString(), Encoding.UTF8, "application/json");
        else if (method != HttpMethod.Get && method != HttpMethod.Delete)
            req.Content = new StringContent("", Encoding.UTF8, "application/json");
        HttpResponseMessage res;
        try
        {
            res = await http.SendAsync(req, ct).ConfigureAwait(false);
        }
        catch (HttpRequestException e)
        {
            throw new ControlException(0, $"can't reach rein-remote host ({e.Message})", e);
        }
        catch (TaskCanceledException e) when (!ct.IsCancellationRequested)
        {
            throw new ControlException(0, "rein-remote host didn't answer in time", e);
        }
        using (res)
        {
            var text = await res.Content.ReadAsStringAsync(ct).ConfigureAwait(false);
            if (res.IsSuccessStatusCode)
                return text;
            throw new ControlException((int)res.StatusCode, ErrorText(res.StatusCode, text));
        }
    }

    /// <summary>The host's <c>{"error"}</c>, or what the status says when the body isn't one.</summary>
    public static string ErrorText(HttpStatusCode status, string body)
    {
        try
        {
            if (JsonNode.Parse(body) is JsonObject o && o["error"] is JsonValue v && v.TryGetValue<string>(out var s) && s.Length > 0)
                return s;
        }
        catch (JsonException)
        {
        }
        return $"rein-remote host answered {(int)status} {status}";
    }

    async Task<T> Call<T>(HttpMethod method, string path, JsonNode? body = null, CancellationToken ct = default)
    {
        var text = await Send(method, path, body, ct).ConfigureAwait(false);
        try
        {
            return JsonSerializer.Deserialize<T>(text, Json.Options) ?? throw new JsonException("empty");
        }
        catch (JsonException e)
        {
            throw new ControlException(500, $"rein-remote host answered something this app doesn't read ({e.Message})", e);
        }
    }

    static JsonObject Obj(params (string Key, JsonNode? Value)[] kv)
    {
        var o = new JsonObject();
        foreach (var (k, v) in kv)
            o[k] = v;
        return o;
    }

    // State and sharing
    public Task<HostState> GetState(CancellationToken ct = default) => Call<HostState>(HttpMethod.Get, "control/state", ct: ct);
    public Task<HostState> StartSharing(CancellationToken ct = default) => Call<HostState>(HttpMethod.Post, "control/sharing/start", ct: ct);
    public Task<HostState> StopSharing(string? reason = null, CancellationToken ct = default) =>
        Call<HostState>(HttpMethod.Post, "control/sharing/stop", string.IsNullOrWhiteSpace(reason) ? null : Obj(("reason", reason)), ct);
    public Task<HostState> RestartSharing(CancellationToken ct = default) => Call<HostState>(HttpMethod.Post, "control/sharing/restart", ct: ct);

    // Sources
    public Task<SourceList> GetSources(CancellationToken ct = default) => Call<SourceList>(HttpMethod.Get, "control/sources", ct: ct);
    public Task<HostState> SetSource(string id, CancellationToken ct = default) => Call<HostState>(HttpMethod.Put, "control/source", Obj(("source", id)), ct);

    // Pairing and devices
    public Task<Pairing> GetPairing(CancellationToken ct = default) => Call<Pairing>(HttpMethod.Get, "control/pairing", ct: ct);
    public Task<Pairing> NewCode(CancellationToken ct = default) => Call<Pairing>(HttpMethod.Post, "control/pairing/code", ct: ct);
    public Task<DeviceList> GetDevices(CancellationToken ct = default) => Call<DeviceList>(HttpMethod.Get, "control/devices", ct: ct);
    public Task<DeviceList> Unpair(string id, CancellationToken ct = default) =>
        Call<DeviceList>(HttpMethod.Delete, "control/devices/" + Uri.EscapeDataString(id), ct: ct);

    // Viewers
    public Task<ViewerList> GetViewers(CancellationToken ct = default) => Call<ViewerList>(HttpMethod.Get, "control/viewers", ct: ct);
    public Task<ViewerList> Kick(string id, string? reason = null, CancellationToken ct = default) =>
        Call<ViewerList>(HttpMethod.Post, $"control/viewers/{Uri.EscapeDataString(id)}/kick",
            string.IsNullOrWhiteSpace(reason) ? null : Obj(("reason", reason)), ct);

    // Settings
    public Task<SettingsResponse> GetSettings(CancellationToken ct = default) => Call<SettingsResponse>(HttpMethod.Get, "control/settings", ct: ct);
    public Task<SettingsResponse> PutSettings(JsonObject changes, CancellationToken ct = default) =>
        Call<SettingsResponse>(HttpMethod.Put, "control/settings", changes, ct);
    public Task<StartAtLogin> GetStartAtLogin(CancellationToken ct = default) => Call<StartAtLogin>(HttpMethod.Get, "control/start-at-login", ct: ct);
    public Task<StartAtLogin> SetStartAtLogin(bool enabled, CancellationToken ct = default) =>
        Call<StartAtLogin>(HttpMethod.Put, "control/start-at-login", Obj(("enabled", enabled)), ct);
    public Task<Permissions> GetPermissions(CancellationToken ct = default) => Call<Permissions>(HttpMethod.Get, "control/permissions", ct: ct);

    // Admin
    public Task<JsonObject> GetSignIn(CancellationToken ct = default) => Call<JsonObject>(HttpMethod.Get, "control/admin/signin", ct: ct);
    public Task<JsonObject> PutSignIn(JsonObject changes, CancellationToken ct = default) => Call<JsonObject>(HttpMethod.Put, "control/admin/signin", changes, ct);
    public Task<Policy> GetPolicy(CancellationToken ct = default) => Call<Policy>(HttpMethod.Get, "control/admin/policy", ct: ct);
    public Task<Policy> PutPolicy(string? text, string? format, CancellationToken ct = default) =>
        Call<Policy>(HttpMethod.Put, "control/admin/policy", Obj(("text", text), ("format", format)), ct);
    public Task<SessionList> GetSessions(CancellationToken ct = default) => Call<SessionList>(HttpMethod.Get, "control/admin/sessions", ct: ct);
    public Task<AuditPage> GetAudit(AuditQuery q, CancellationToken ct = default) => Call<AuditPage>(HttpMethod.Get, "control/admin/audit" + q.ToQueryString(), ct: ct);
    public Task<JsonObject> GetMetrics(CancellationToken ct = default) => Call<JsonObject>(HttpMethod.Get, "control/admin/metrics", ct: ct);

    /// <summary>
    /// <c>GET /control/events</c>: the events as they come, until the host ends the stream (it exited)
    /// or nothing (not even the keepalive) came for <paramref name="idle"/>.
    /// </summary>
    public async IAsyncEnumerable<SseEvent> Events(TimeSpan? idle = null, [EnumeratorCancellation] CancellationToken ct = default)
    {
        using var req = new HttpRequestMessage(HttpMethod.Get, "control/events");
        req.Headers.Accept.Clear();
        req.Headers.Accept.Add(new MediaTypeWithQualityHeaderValue("text/event-stream"));
        HttpResponseMessage res;
        try
        {
            res = await events.SendAsync(req, HttpCompletionOption.ResponseHeadersRead, ct).ConfigureAwait(false);
        }
        catch (HttpRequestException e)
        {
            throw new ControlException(0, $"can't reach rein-remote host ({e.Message})", e);
        }
        using (res)
        {
            if (!res.IsSuccessStatusCode)
                throw new ControlException((int)res.StatusCode, ErrorText(res.StatusCode, await res.Content.ReadAsStringAsync(ct).ConfigureAwait(false)));
            var stream = await res.Content.ReadAsStreamAsync(ct).ConfigureAwait(false);
            await foreach (var e in Sse.Read(stream, idle ?? TimeSpan.FromSeconds(40), ct).ConfigureAwait(false))
                yield return e;
        }
    }

    /// <summary>An event's data as its object.</summary>
    public static T Parse<T>(string data) =>
        JsonSerializer.Deserialize<T>(data, Json.Options) ?? throw new JsonException("empty event");
}
