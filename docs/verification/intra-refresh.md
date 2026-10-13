# Intra refresh (hardware encoders)

Intra refresh codes a band of the picture intra in every P-frame. The band moves down the picture, so
the whole picture is refreshed every half second (`fps / 2` frames), and a frame lost on the way
heals by itself without a big keyframe. Clients still ask for a keyframe on a loss, and get one.

| Encoder | Intra refresh |
|---|---|
| VA-API (Linux: Intel, AMD) | rolling rows (`VAEncMiscParameterRIR`) when the driver reports `VAConfigAttribEncIntraRefresh` with rolling rows (Intel's iHD does) |
| Media Foundation (Windows) | `CODECAPI_AVEncVideoGradualIntraRefresh`, when the encoder takes it (some vendors' hardware encoders) |
| VideoToolbox (macOS), openh264 (software) | not offered by either; a lost frame is healed by the keyframe the client asks for, long-term references keep that rare |

The band's arithmetic and libva's struct layout are unit-tested (Linux CI). Neither encoder has
been run on hardware with it: there's no GPU here or on the CI runners.

## Manual test (a Linux machine with an Intel GPU, or a Windows machine with one that offers it)

1. `rein-remote serve --test-pattern 1280x720 --motion full --listen 127.0.0.1:7979`: the log's
   encoder line ends `, intra refresh` (VA-API: `H.264 in hardware (VA-API: Intel…, CBR, intra
   refresh)`; Media Foundation: the encoder's name then `, intra refresh`). If it doesn't, this GPU or
   driver doesn't offer it: note which and stop.
2. Record the stream: `rein-remote serve … --record /tmp/rec`, open the page, wait 10 s, close.
   `ffprobe -show_frames -select_streams v /tmp/rec/*.mp4 | grep -c 'pict_type=I'` is 1 (the first
   frame only), and `ffmpeg -i /tmp/rec/*.mp4 -vf "select='eq(n,40)'" -vframes 1 band.png` decodes.
3. Healing without a keyframe: with the stream open in the native viewer
   (`rein-remote-viewer http://127.0.0.1:7979 --measure`), drop everything from the viewer to the
   server for 2 s (Linux: `sudo tc qdisc add dev lo root netem loss 100%` then `sudo tc qdisc del dev
   lo root`, or block the viewer's machine for 2 s), so no keyframe request arrives, while losing
   some video too (`netem loss 5%` the other way). Pass: the picture is whole again within half a
   second of the loss (the barcode reads again in the viewer's title, no smeared blocks) with no new
   `pict_type=I` in a recording made meanwhile.
