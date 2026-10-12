# @rein-harness/remote

The web client of [Rein Remote](https://github.com/rein-harness/rein-remote): show a `rein-remote`
server's screen, a window or an app in any page, and control it (mouse, wheel, keyboard, touch, the
device's on-screen keyboard). Decodes with WebCodecs (hardware where the device has it), tries
WebTransport (QUIC) first when it's offered and falls back to the WebSocket, and plays H.264 through
Media Source on plain-HTTP pages. No dependencies.

```js
import {connect} from '@rein-harness/remote';

const session = connect('wss://host:7979/stream?token=…', document.querySelector('canvas'));
setInterval(() => console.log(session.stats.frames, session.stats.decoder), 1000);
```

Or as an element (importing the module defines it):

```html
<script type="module" src="node_modules/@rein-harness/remote/rein-remote.js"></script>
<rein-remote src="wss://host:7979/stream?token=…"></rein-remote>
```

The server: [`@rein-harness/remote-server`](https://www.npmjs.com/package/@rein-harness/remote-server)
(`npx rein-remote serve`), or a release build. Every server also serves this same file at
`/rein-remote.js`, matching its own version.

MIT.
