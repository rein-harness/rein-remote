# Verifying NAT hole punching across real NATs

`tests/nat/run.sh` checks the direct path between two NAT routers in Docker. This page is for the
real thing: two homes, or a home and a phone's hotspot. It takes about fifteen minutes and three
machines.

## What should happen

A native viewer (`rein-remote connect`) that came in through a relay is offered a direct path by the
host, inside the sealed session. Both learn their address beyond their NAT by asking the relay over
UDP from the socket QUIC will use, swap addresses, and send each other punch packets. The viewer then
opens WebTransport straight to the host from that same socket, checking the certificate against the
hash in the offer. If it connects, the session moves there and the relayed one closes. If not, it
stays on the relay.

Browsers always stay on the relay: a page has no UDP socket of its own, so it can't punch.

## You need

- **A relay both sides can reach:** a small VPS with a public IPv4 address. Open its port for **TCP
  and UDP** (7990 here). The direct path needs the relay's UDP port to be reachable directly. An HTTPS
  proxy in front of it (Caddy, nginx, Cloudflare) passes only TCP, so `wss://` through a proxy still
  relays but never goes direct. In that case, run the relay on a port of its own and use `ws://`. The
  session is still sealed end to end, and so is the offer.
- **The host** behind one NAT (home A) and **the viewer** behind another (home B, or a laptop on a
  phone's hotspot), each with `rein-remote` built from this branch.
- The same `rein-remote` version on all three.

## Steps

On the relay (VPS):

```sh
rein-remote relay --listen 0.0.0.0:7990 --secret S
# check UDP is open from outside: the firewall (ufw allow 7990/udp, the provider's security group)
```

On the host (home A):

```sh
rein-remote serve --test-pattern 1280x720 --relay ws://RELAY_IP:7990 --relay-secret S
```

It should print:

- `the relay sees WebTransport's socket as A.B.C.D:7979`: home A's public address. If it prints
  `didn't answer over UDP` instead, the relay's UDP port isn't reachable.
- `through the relay: open http://RELAY_IP:7990/?room=…#k=…`: the viewer's link. It carries the
  key, so treat it as a secret.

On the viewer (home B or the hotspot), first relayed only and then allowed to go direct, 20 seconds
each:

```sh
rein-remote connect 'http://RELAY_IP:7990/?room=…#k=…' --seconds 20 --direct off
rein-remote connect 'http://RELAY_IP:7990/?room=…#k=…' --seconds 20
```

On success, the second run prints `punching: this side …, the host … (heard from …)`, then
`now on direct-after-punch`, and its summary has a `relay` line and a `direct-after-punch` line. The
host prints `a viewer came straight here from …`.

Add `--json` to keep the numbers. Write down from each run the transport, fps, kbit/s, rtt, and
capture→arrival p50/p95. Two different machines don't share a clock, so use `latency_ms` (set
against each other from pings) and not `latency_same_clock_ms`.

## Expected outcomes by NAT type

| Viewer's network | Host's network | Expected |
|---|---|---|
| Home router | Home router | direct |
| Phone hotspot (most carriers) | Home router | direct |
| Carrier-grade NAT with a port per destination (some mobile carriers) | Home router | stays relayed |
| Both behind symmetric NATs | | stays relayed |
| A network that blocks UDP (some offices, hotels) | any | stays relayed (and over the WebSocket) |

When it stays relayed, the viewer prints `no direct path (…): staying on the relay` and the stream
carries on over the relay with no gap.

## Things to try

- **Same network.** Put both on one LAN, still through the relay. It should go direct to the LAN
  candidate (`heard from 192.168.…`) without leaving the network.
- **Hotspot to home.** This is the most common real case. Note the carrier: whether it works depends
  on its NAT.
- **Idle mapping.** Leave the host running with no viewer for 10 minutes, then connect. The host's
  keepalive to the relay (every 15 s) should keep its NAT mapping, so it still goes direct.
- **Blocked UDP.** Connect from a network that blocks UDP. It stays on the relay, and with no
  WebTransport at all it uses the WebSocket.

## Reporting

For each try, note: both network types (home router make or ISP, carrier for a hotspot), whether it
went direct, the time to go direct (`now on direct-after-punch (after N s)`), and the relayed and
direct numbers side by side.
