/*
 * Rein Remote's native client core (rr-client) as a C library: connect to a server (WebTransport with
 * video as datagrams, the WebSocket, or a relay and then a direct path), pair and sign in with keys,
 * take the stream as events, send input. No UI and no decoding: the app decodes and draws.
 *
 * Threads: rr_connect blocks (HTTP, pairing, signing in, the handshake): call it off the UI thread.
 * One thread waits for events with rr_next_event; every other function may be called from any thread,
 * at the same time. Strings in are UTF-8, NUL-terminated; strings out are freed with rr_string_free.
 */
#ifndef RR_FFI_H
#define RR_FFI_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct RrSession RrSession;

/* Codecs (RrEvent.codec; bits of RrOptions.codecs are 1 << these). */
enum { RR_CODEC_H264 = 1, RR_CODEC_H265 = 2, RR_CODEC_AV1 = 3, RR_CODEC_VP9 = 4, RR_CODEC_VP8 = 5 };

/* Input kinds a server accepts (RrEvent.server_input). */
enum { RR_INPUT_POINTER = 1, RR_INPUT_KEYBOARD = 2, RR_INPUT_TOUCH = 4, RR_INPUT_GAMEPAD = 8 };

/* Buttons (rr_button). */
enum { RR_BUTTON_LEFT = 0, RR_BUTTON_RIGHT = 1, RR_BUTTON_MIDDLE = 2, RR_BUTTON_BACK = 3, RR_BUTTON_FORWARD = 4 };

typedef struct {
  /* Try WebTransport (QUIC) before the WebSocket; video as datagrams with parity over it. */
  bool webtransport;
  bool datagrams;
  /* Through a relay: move to a direct path when one can be punched through NAT. */
  bool direct;
  /* This app plays Opus (48 kHz stereo): the server sends RR_EVENT_AUDIO. */
  bool audio;
  /* This app sends gamepads (rr_gamepad). */
  bool gamepad;
  /* What it decodes: bits (1 << RR_CODEC_*), and whether in hardware. 0: H.264. */
  uint32_t codecs;
  bool hardware;
  /* The display's refresh rate in millihertz (60000 for 60 Hz): the server paces to it. */
  uint32_t refresh_mhz;
} RrOptions;

/* Event kinds. */
enum {
  RR_EVENT_HELLO = 1,     /* text: the server's name; server_input, server_audio */
  RR_EVENT_CONFIG = 2,    /* codec, width, height, fps, bitrate_kbps */
  RR_EVENT_VIDEO = 3,     /* frame, keyframe, timestamp_us, received_us, latency_us; data: Annex B */
  RR_EVENT_CURSOR = 4,    /* x, y, visible; has_shape: shape_*, hot_*, data (RGBA) */
  RR_EVENT_AUDIO = 5,     /* timestamp_us; data: one Opus packet */
  RR_EVENT_CLIPBOARD = 6, /* text */
  RR_EVENT_TRANSPORT = 7, /* text: the path it moved to ("direct-after-punch") */
  RR_EVENT_NOTE = 8,      /* text: something worth saying */
  RR_EVENT_CLOSED = 9,    /* text: why, or NULL when the server closed it */
};

typedef struct {
  uint32_t kind;
  /* RR_EVENT_CONFIG */
  uint8_t codec;
  uint16_t width;
  uint16_t height;
  uint16_t fps;
  uint32_t bitrate_kbps;
  /* RR_EVENT_VIDEO (timestamp_us for RR_EVENT_AUDIO too). Times are microseconds: timestamp_us on
   * the server's clock (capture), received_us on this one's (Unix time); latency_us, capture to
   * arrival estimated from pings, when has_latency. */
  uint32_t frame;
  bool keyframe;
  uint64_t timestamp_us;
  uint64_t received_us;
  bool has_latency;
  int64_t latency_us;
  /* RR_EVENT_CURSOR: in the stream's pixels */
  uint16_t x;
  uint16_t y;
  bool visible;
  bool has_shape;
  uint16_t shape_width;
  uint16_t shape_height;
  uint16_t hot_x;
  uint16_t hot_y;
  /* RR_EVENT_HELLO */
  uint8_t server_input;
  bool server_audio;
  /* Bytes (video, audio, the cursor's RGBA) and text, owned by the event. */
  const uint8_t *data;
  size_t len;
  const char *text;
} RrEvent;

typedef struct {
  uint64_t frames;
  uint64_t keyframes;
  uint64_t bytes;
  uint64_t frames_lost;
  uint64_t fec_recovered;
  uint64_t fec_lost;
  uint16_t loss_permille;
  double rtt_ms;
  double rtt_min_ms;
  bool has_clock_offset;
  int64_t clock_offset_us;
  /* "webtransport-datagrams", "webtransport-streams", "websocket", "relay", "direct-after-punch":
   * static, never freed. */
  const char *transport;
} RrStats;

/* Defaults: WebTransport, datagrams, direct, H.264 in software, 60 Hz, no audio, no gamepads. */
RrOptions rr_options_default(void);

/* Where pairings are kept (a folder the app owns, e.g. Application Support/rein-remote): call once,
 * before anything else. False when it was set already. */
bool rr_set_home(const char *path);

/* Connects to `target`: a link the server printed (http(s)://host:port/?token=…), a relay's link
 * (…/?room=…#k=…), or http://host:port/ to sign in with this app's key (pairing first with
 * `pair_code`, the code the server shows, when not yet paired; NULL otherwise). `name` is what the
 * server shows for this device. NULL on failure, with *error set (free it) when error isn't NULL. */
RrSession *rr_connect(const char *target, const char *name, const char *pair_code, const RrOptions *opts, char **error);

/* The next event, waiting up to wait_ms; NULL when none came. Free it with rr_event_free. After
 * RR_EVENT_CLOSED there are no more. One thread at a time. */
RrEvent *rr_next_event(const RrSession *s, uint32_t wait_ms);
void rr_event_free(RrEvent *e);

/* Input, in the stream's pixels (RR_EVENT_CONFIG's width and height). */
void rr_pointer_move(const RrSession *s, uint16_t x, uint16_t y);
void rr_pointer_delta(const RrSession *s, int16_t dx, int16_t dy);
void rr_button(const RrSession *s, uint8_t button, bool down);
/* Positive: down / right, in pixels. */
void rr_wheel(const RrSession *s, int16_t dx, int16_t dy);
/* A key: its USB HID usage (page 7; 0 when unknown) and its name as the web has it ("a", "Enter",
 * "ArrowLeft", "Shift", " "). */
void rr_key(const RrSession *s, uint16_t hid, bool down, const char *name);
void rr_text(const RrSession *s, const char *text);
/* Touch points: ids, xs and ys, `n` of each; n = 0 when the last finger lifts. */
void rr_touch(const RrSession *s, const uint8_t *ids, const uint16_t *xs, const uint16_t *ys, size_t n);
/* A gamepad's state (the W3C standard layout: see rr-proto's Input::Gamepad). */
void rr_gamepad(const RrSession *s, uint8_t pad, uint32_t buttons, int16_t lx, int16_t ly, int16_t rx, int16_t ry, uint8_t lt, uint8_t rt);
void rr_clipboard(const RrSession *s, const char *text);
/* The size the app would like the stream at. */
void rr_resize(const RrSession *s, uint16_t width, uint16_t height);

/* The decoder lost its place (or a frame failed): ask for a keyframe. */
void rr_keyframe(const RrSession *s);
/* A frame was decoded (Unix µs, this clock): drives the server's bitrate. `lost`: frames lost since. */
void rr_feedback(const RrSession *s, uint32_t frame, uint64_t received_us, uint64_t decoded_us, uint16_t lost);

void rr_stats(const RrSession *s, RrStats *out);

/* Ends the session (RR_EVENT_CLOSED follows); the handle stays valid until rr_free. */
void rr_close(const RrSession *s);
/* Ends it if it isn't, and frees the handle: no other call on it may be running or follow. */
void rr_free(RrSession *s);

/* Pairs with the server at `url` (http(s)://host:port/) with its code: keys kept in the home folder.
 * Its fingerprint (free it), or NULL with *error set. */
char *rr_pair(const char *url, const char *code, const char *name, char **error);
/* Whether this app paired with the server whose fingerprint is `id`. */
bool rr_is_paired(const char *id);
/* Servers on this network (mDNS), found within wait_ms, as JSON: [{"name", "addrs": [...], "port",
 * "id", "webtransport", "https"}]. (On iOS, Bonjour through Network.framework is what works on a
 * device: raw multicast needs an entitlement.) Free it. */
char *rr_discover(uint32_t wait_ms, char **error);

/* Unix time, microseconds: the clock received_us is on. */
uint64_t rr_now_us(void);
/* This library's version. Static. */
const char *rr_version(void);
void rr_string_free(char *s);

#ifdef __cplusplus
}
#endif

#endif
