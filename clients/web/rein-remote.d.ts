// Types for the Rein Remote web client (rein-remote.js).

export interface Stats {
  /** Frames decoded and shown. */
  frames: number;
  keyframes: number;
  /** Video bytes received. */
  bytes: number;
  /** The decoder in use, e.g. "avc1.42e01f (hardware)". */
  decoder: string;
  /** This client's own round trip to the server, in ms. */
  rttMs: number;
  state: 'connecting' | 'handshake' | 'streaming' | 'closed';
  /** "websocket" or "webtransport" once connected. */
  transport?: string;
  /** The last error, if any (empty when none). */
  error: string;
  /** Glass-to-glass samples in ms (only with `measure`, against `serve --test-pattern`). */
  glassMs: number[];
}

export interface ConnectOptions {
  /** Read the test pattern's barcode from each frame (glass-to-glass latency). Default: `?measure` in the page's address. */
  measure?: boolean;
  /** The end-to-end key of a relayed session. Default: `k` in the page's address after `#`. */
  e2eKey?: string | null;
  /** WebTransport (QUIC) to try first: its https:// URL and, for a self-signed certificate, its SHA-256 (hex). */
  webtransport?: {url: string; hash?: string | null};
  /** This client's name, as the server logs it. */
  name?: string;
}

export interface Session {
  stats: Stats;
  /** Show the device's on-screen keyboard (call it from a tap: browsers allow it only then). */
  keyboard(): void;
  close(): void;
}

/** Connect to a rein-remote server's stream (ws:// or wss://…/stream?token=…) and show it on `canvas`. */
export function connect(url: string, canvas: HTMLCanvasElement, opts?: ConnectOptions): Session;

declare global {
  interface HTMLElementTagNameMap {
    'rein-remote': HTMLElement & {session?: Session; keyboard?(): void};
  }
}
