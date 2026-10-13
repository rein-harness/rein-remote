# A fast game scene on a bad link (phase 4's last check)

`tests/netem/run.sh` runs every link (tc netem on the loopback: loss, delay with jitter, a token
bucket for bandwidth) with each transport and client, in one Linux container, and prints one JSON
line each. On a Mac its Docker VM has 4 cores and no GPU: the server encodes and the viewer decodes
720p full motion in software at once, so the frame rate there is the VM's, not the link's (the
LAN rows shrink the picture too). What's left to check needs a Linux machine with a GPU encoder
and decoder (Intel or AMD with VA-API; NVIDIA with NVENC once it's in).

## Run

```sh
# On the GPU machine (Docker with /dev/dri passed through), from the repository:
FPS=120 CLIENTS=native RES=1920x1080 MOTION=full SECS=30 tests/netem/run.sh | tee netem.jsonl
```

To use the GPU inside the container, add `--device /dev/dri` to the `docker run` line in
`tests/netem/run.sh` and `libva2 libva-drm2 mesa-va-drivers` (AMD) or `intel-media-va-driver`
(Intel) to `tests/netem/Dockerfile`; the server's log then says `encoding H.264 in hardware
(VA-API)` and the viewer's `decoding with VA-API`.

## Pass

For the `webtransport` rows (datagrams with parity, the game path):

- `lan`: fps ≥ 115 with `FPS=120`, glass p50 ≤ 15 ms, `size` the
  full 1920×1080.
- `rtt40_loss2` and `rtt40_loss5`: fps ≥ 60, p95 ≤ 120 ms, `keyframe_requests` ≤ 2 in 30 s.
- `rtt80_jitter`: fps ≥ 60, p95 ≤ 200 ms.
- `bw1500`, `bw800`: fps ≥ 60 with a smaller `size` (the picture gives way, not the frame rate).

## Measured so far (Mac, Docker VM, software both ends, 1280×720 full motion, 15 s)

| link | transport | fps | glass p50 / p95 (ms) | size sent |
|---|---|---|---|---|
| lan | datagrams | 54.6 | 6 / 27 | 640×360 |
| lan | WebSocket | 38.7 | 18 / 1294 | 426×240 |
| 40 ms RTT, 2% loss | datagrams | 52.6 | 28 / 359 | 640×360 |
| 40 ms RTT, 2% loss | WebSocket | 45.5 | 40 / 272 | 426×240 |
| 40 ms RTT, 5% loss | datagrams | 50.4 | 36 / 445 | 640×360 |
| 40 ms RTT, 5% loss | WebSocket | 50.9 | 63 / 334 | 426×240 |
| 80 ms RTT, 10 ms jitter, 1% loss | datagrams | 33.1 | 57 / 542 | 426×240 |
| 80 ms RTT, 10 ms jitter, 1% loss | WebSocket | 54.9 | 68 / 175 | 426×240 |
| 1.5 Mbit/s | datagrams | 40.9 | 27 / 75 | 426×240 |
| 1.5 Mbit/s | WebSocket | 37.8 | 39 / 551 | 426×240 |
| 800 kbit/s | datagrams | 24.2 | 30 / 889 | 426×240 |
| 800 kbit/s | WebSocket | 40.7 | 44 / 1155 | 426×240 |
