# A relay fleet, health and metrics

## How it works

```sh
rein-remote relay --listen 0.0.0.0:7990 --secret S        # on each relay machine (or --config relay.toml)
rein-remote serve --relay wss://relay-a.example --relay wss://relay-b.example --relay-secret S
```

- **The room is open on every relay.** `serve` connects out to each relay it's given and waits in its
  room there; after each viewer, and when a relay comes back after going away, it opens the room
  there again (every 2 s while a relay is unreachable). So whichever relay a viewer reaches, the
  machine is there.
- **The link names the fastest.** At start, `serve` asks each relay's `/healthz` at once and ranks them by
  round trip (`rein-remote: relays, fastest first: relay-a.example:443 12 ms, relay-b.example:443 31
  ms`). The link it prints names the first, and carries the others after `#`:
  `https://relay-a.example/?room=…#k=KEY&alt=wss://relay-b.example`. The part after `#` never goes to
  a server, so no relay sees the key (sessions are sealed end to end with it, as with one relay) or
  the rest of the fleet. That relay is also the one whose UDP port tells the machine its address
  beyond NAT (for a direct path).
- **A native viewer fails over by itself:** `rein-remote connect` and every rr-client app try the
  link's relay, then each `alt` in turn. A browser opens the relay the link names (open the link
  again, or put the fleet behind one name).
- **Several relays behind one name:** a load balancer (or DNS) in front, checking each relay's
  `GET /healthz` (200 while it runs). Since the room is open on all of them, any relay it picks has
  the machine. `tests/relay-fleet/haproxy.cfg` is a working example: health checks twice a second,
  `on-marked-down shutdown-sessions` (a relay that vanished has its sessions closed at once, so their
  viewers reconnect through another), `option redispatch` (a connection that fails is retried on the
  other relay).
- **Health and metrics.** `serve` checks each relay's `/healthz` every 10 s and says when one stops
  answering or answers again. `GET /metrics` on `serve` (with the token: `Authorization: Bearer …`)
  has, per relay, `rein_remote_relay_up`, `rein_remote_relay_rtt_seconds`,
  `rein_remote_relay_registered` (its room open there now) and `rein_remote_relay_registrations_total`,
  and the machine's viewers (`rein_remote_viewers{path="direct|relay"}`), sessions, frames, keyframes,
  bytes, frames skipped on a backed-up link, direct-path offers and answers. A relay's `/metrics`
  (`--metrics-listen`, `127.0.0.1:7991` by default: this machine only) has hosts waiting, sessions now
  and in all, bytes relayed each way, rendezvous probes answered, and requests refused (wrong secret,
  bad room, no machine in the room). None of it holds a room code, token, key or fingerprint (a test
  checks the text for them).

No protocol message changed: the fleet is the same relay protocol, more than once.

## Verified

### On one machine (`cargo test -p rr-server --test fleet`, CI on Linux, macOS, Windows)

Two relays, `serve --relay A --relay B`, a viewer (rr-client, relayed) through the link. The test
checks the link (key and `alt` after `#`), that the room is open on both (`/healthz` says
`hosts_waiting: 1` on each), `/healthz` and `/metrics` refused without the token and given with it as a
bearer or in the query, a session and bytes in the busy relay's metrics with no room code or key in
any of them. Then it **kills the relay the viewer is on** (`kill -9`: the OS closes its sockets, as
in a crash), opens the **same link** again (the dead relay refuses at once; the next one in `alt` has
the room), and **starts the dead relay again on the same address**: the server must be back in its
room there.

Measured on an Apple M-series Mac (macOS 26), ten runs:

| | median | worst |
|---|---|---|
| viewer sees the session end, after the kill | 1.5 ms | 5 ms |
| **a frame again through the other relay, after the kill** (same link reopened) | **60 ms** | 85 ms |
| server back in its room on the restarted relay, after it was started (includes the relay starting) | 340 ms | 351 ms |

### Behind one name, in Docker (`tests/relay-fleet/run.sh`)

`tests/relay-fleet/compose.yml`: relay-a and relay-b (`relay --config`, the secret from a file), haproxy
in front as the one name `lb:7990`, the host (`serve --config`: two relays, a 640×360 test pattern,
software H.264), a viewer that opens `http://lb:7990/?room=…#k=…` again whenever its session ends (as
an app would). Each round **`docker kill`s the relay the viewer is on** (the container vanishes:
nothing is sent to anyone, as when a machine or its network dies), waits 8 s, and starts it again.
Times are taken inside the Docker machine (one clock), from the viewer's own last and first frames.

Measured on colima (4 vCPU, 4 GB, Apple silicon host), six rounds:

```text
a relay died: no picture for 1.88 s (the reconnect itself: 1622 ms to its first frame)
a relay died: no picture for 1.16 s (the reconnect itself: 928 ms to its first frame)
a relay died: no picture for 0.68 s (the reconnect itself: 567 ms to its first frame)
a relay died: no picture for 1.47 s (the reconnect itself: 1055 ms to its first frame)
a relay died: no picture for 0.52 s (the reconnect itself: 319 ms to its first frame)
5 failovers: median 1.16 s, worst 1.88 s
round 1: relay-b restarted; the host back in its room there 2.46 s after it started
round 2: relay-a restarted; the host back in its room there 1.18 s after it started
round 3: relay-b restarted; the host back in its room there 1.85 s after it started
round 4: relay-a restarted; the host back in its room there 2.58 s after it started
round 5: relay-b restarted; the host back in its room there 1.82 s after it started
round 6: relay-a restarted; the host back in its room there 3.42 s after it started
```

(The sixth round's failover isn't in the list: the viewer was stopped before it logged that session.)
Without a picture for about a second: haproxy's health checks (2 × 500 ms) take the dead relay out,
the viewer's first try may still land on it, and the software encoder starts a new session with a
keyframe. The host is back in a restarted relay's room within the 2 s it waits between tries, plus
the relay's start.

What the host does when a relay **vanishes without a word** while it only waits there (no viewer): the
connection looks open until TCP keepalive gives up (30 s idle, then 3 probes 10 s apart: about a
minute), then it tries again every 2 s. Viewers are unaffected meanwhile: the room is open on the
other relays. When the relay comes back on the same address, its first answer to a keepalive resets
the old connection and the host is back within seconds (the rounds above).

## Manual test (real relays)

1. Two machines anyone can reach, each running `rein-remote install-service relay --system --config
   relay.toml` (`listen = "0.0.0.0:7990"`, `secret = { file = "secret" }`), each behind HTTPS (Caddy:
   `relay-a.example { reverse_proxy 127.0.0.1:7990 }`). `curl https://relay-a.example/healthz` says
   `"status":"ok"`.
2. On the machine to show: `rein-remote serve --relay wss://relay-a.example --relay wss://relay-b.example
   --relay-secret S`: it prints both relays' round trips, fastest first, and one link with `&alt=` after
   `#`.
3. `rein-remote connect LINK --seconds 60 --direct off` from a third network. Stop the relay it's on
   (`systemctl stop rein-remote-relay`): the session ends; run the same command again: it's through the
   other relay at once (`rein-remote: through wss://relay-b.example (…)`).
4. Start the stopped relay: within seconds `serve` says `back in the room on the relay relay-a.example:443`
   (or `curl https://relay-a.example/healthz` shows `"hosts_waiting":1`).
5. Prometheus: scrape `serve`'s `/metrics` with `authorization: {type: Bearer, credentials_file:
   token}`, and each relay's `127.0.0.1:7991/metrics` from the relay machine (or `--metrics-listen` on a
   private address). `rein_remote_relay_up` drops to 0 within 10 s of a relay stopping.
