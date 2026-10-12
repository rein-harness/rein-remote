# @rein-harness/remote-server

The [Rein Remote](https://github.com/rein-harness/rein-remote) server for this machine (macOS arm64
and x64, Linux x64 and arm64 with glibc, Windows x64), from npm. npm installs only this platform's
binary (`@rein-harness/remote-server-<os>-<cpu>`, an optional dependency).

```sh
npx rein-remote serve --window Simulator   # one app's window
npx rein-remote serve --test-pattern       # a moving test pattern
npx rein-remote windows --json             # the windows it can show
```

From a program, run the binary itself (not the `rein-remote` script: a signal to the script on
Windows wouldn't reach the binary):

```js
import {spawn} from 'node:child_process';
import {binaryPath} from '@rein-harness/remote-server';

const p = spawn(binaryPath(), ['serve', '--test-pattern', '--listen', '127.0.0.1:0'], {
  env: {...process.env, REIN_REMOTE_TOKEN: token}, // the stream then needs ?token=…
  stdio: ['ignore', 'ignore', 'pipe'],
});
// stderr says "… on http://127.0.0.1:PORT …" once it listens.
```

`REIN_REMOTE_BINARY` overrides the binary's path. The web client is
[`@rein-harness/remote`](https://www.npmjs.com/package/@rein-harness/remote).

MIT.
