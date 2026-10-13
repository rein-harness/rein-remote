//! Rein Remote's Linux host app: the control API's client (docs/control-api.md) and what the app
//! needs that isn't GTK (events, times, QR codes, starting and following the host). The app itself
//! (`src/main.rs`, `src/ui`) needs the `gtk` feature.

pub mod client;
pub mod launch;
pub mod model;
pub mod qr;
pub mod sse;
pub mod time;
