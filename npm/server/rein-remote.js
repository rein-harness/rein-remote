#!/usr/bin/env node
// `rein-remote …` from npm: runs this machine's binary with the same arguments, input and output.
import {spawn} from 'node:child_process';
import {constants} from 'node:os';
import {binaryPath} from '../index.js';

let bin;
try {
  bin = binaryPath();
} catch (err) {
  console.error(`rein-remote: ${err.message}`);
  process.exit(1);
}
const child = spawn(bin, process.argv.slice(2), {stdio: 'inherit'});
for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(sig, () => child.kill(sig));
child.on('error', (err) => (console.error(`rein-remote: ${err.message}`), process.exit(1)));
// Ended by a signal: exit as a shell would say it (128 + the signal's number).
child.on('exit', (code, sig) => process.exit(sig ? 128 + (constants.signals[sig] ?? 0) : (code ?? 1)));
