#!/usr/bin/env node
// Builds Rein Remote's npm packages into a folder, ready for `npm pack` or `npm publish`:
//
//   @rein-harness/remote                  the web client (rein-remote.js, its types, the page)
//   @rein-harness/remote-server           the server: `npx rein-remote …`, and binaryPath() for programs
//   @rein-harness/remote-server-<os>-<cpu> one per platform, holding that platform's binary; the one
//                                         above depends on them all as optional, so npm installs only
//                                         the one for the machine it's on
//
//   node npm/build.mjs --out DIR [--version V] [--bin RUST_TARGET=PATH ...]
//
// Without --bin, only the packages for the binaries given are made (none: the web client and the
// server's front package alone). The version defaults to the workspace's (Cargo.toml).

import {chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync} from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const SCOPE = '@rein-harness';
const REPO = 'https://github.com/rein-harness/rein-remote';

/** Rust's target names → npm's os and cpu. Every platform a release builds. */
export const PLATFORMS = {
  'aarch64-apple-darwin': {os: 'darwin', cpu: 'arm64'},
  'x86_64-apple-darwin': {os: 'darwin', cpu: 'x64'},
  'x86_64-unknown-linux-gnu': {os: 'linux', cpu: 'x64'},
  'aarch64-unknown-linux-gnu': {os: 'linux', cpu: 'arm64'},
  'x86_64-pc-windows-msvc': {os: 'win32', cpu: 'x64'},
};
const platformName = ({os, cpu}) => `${SCOPE}/remote-server-${os}-${cpu}`;

function args(argv) {
  const out = {bins: {}, version: undefined, out: undefined};
  for (let i = 0; i < argv.length; i++) {
    const v = argv[i + 1];
    if (argv[i] === '--out') (out.out = v), i++;
    else if (argv[i] === '--version') (out.version = v), i++;
    else if (argv[i] === '--bin') {
      const [target, file] = v.split(/=(.*)/s);
      if (!PLATFORMS[target]) throw new Error(`--bin: unknown target ${target} (one of ${Object.keys(PLATFORMS).join(', ')})`);
      out.bins[target] = file;
      i++;
    } else throw new Error(`unknown argument ${argv[i]}`);
  }
  if (!out.out) throw new Error('usage: node npm/build.mjs --out DIR [--version V] [--bin RUST_TARGET=PATH ...]');
  return out;
}

const workspaceVersion = () => readFileSync(path.join(root, 'Cargo.toml'), 'utf8').match(/^version\s*=\s*"([^"]+)"/m)[1];

function write(dir, files) {
  mkdirSync(dir, {recursive: true});
  for (const [name, content] of Object.entries(files)) {
    mkdirSync(path.dirname(path.join(dir, name)), {recursive: true});
    writeFileSync(path.join(dir, name), typeof content === 'string' ? content : JSON.stringify(content, null, 2) + '\n');
  }
}

const common = (version) => ({version, license: 'MIT', repository: {type: 'git', url: `git+${REPO}.git`}, homepage: REPO});

export function build({out, version = workspaceVersion(), bins = {}}) {
  rmSync(out, {recursive: true, force: true});
  const license = readFileSync(path.join(root, 'LICENSE'), 'utf8');

  // The web client: the same file every server serves at /rein-remote.js.
  const web = path.join(out, 'remote');
  write(web, {
    'package.json': {
      name: `${SCOPE}/remote`,
      description: "Rein Remote's web client: show and control a rein-remote server's screen in any page (WebCodecs, WebTransport, <rein-remote>)",
      ...common(version),
      type: 'module',
      main: './rein-remote.js',
      types: './rein-remote.d.ts',
      exports: {'.': {types: './rein-remote.d.ts', default: './rein-remote.js'}, './rein-remote.js': './rein-remote.js', './index.html': './index.html', './package.json': './package.json'},
      files: ['rein-remote.js', 'rein-remote.d.ts', 'index.html', 'README.md', 'LICENSE'],
      sideEffects: true,
      keywords: ['remote-desktop', 'webcodecs', 'webtransport', 'h264', 'streaming'],
    },
    'README.md': readFileSync(path.join(root, 'npm', 'remote.README.md'), 'utf8'),
    LICENSE: license,
  });
  for (const f of ['rein-remote.js', 'rein-remote.d.ts', 'index.html']) copyFileSync(path.join(root, 'clients', 'web', f), path.join(web, f));

  // One package per platform binary.
  const made = [];
  for (const [target, file] of Object.entries(bins)) {
    const p = PLATFORMS[target];
    const exe = p.os === 'win32' ? 'rein-remote.exe' : 'rein-remote';
    const dir = path.join(out, `remote-server-${p.os}-${p.cpu}`);
    write(dir, {
      'package.json': {
        name: platformName(p),
        description: `The rein-remote server binary for ${p.os} ${p.cpu} (installed by ${SCOPE}/remote-server)`,
        ...common(version),
        os: [p.os],
        cpu: [p.cpu],
        ...(p.os === 'linux' ? {libc: ['glibc']} : {}),
        files: [`bin/${exe}`, 'LICENSE'],
        preferUnplugged: true,
      },
      LICENSE: license,
    });
    mkdirSync(path.join(dir, 'bin'), {recursive: true});
    copyFileSync(file, path.join(dir, 'bin', exe));
    if (p.os !== 'win32') chmodSync(path.join(dir, 'bin', exe), 0o755);
    made.push(platformName(p));
  }

  // The front package: finds the platform's binary, runs it as `rein-remote`.
  const server = path.join(out, 'remote-server');
  write(server, {
    'package.json': {
      name: `${SCOPE}/remote-server`,
      description: 'The rein-remote server (capture, encode, stream) for this machine, as an npm dependency: `npx rein-remote serve`, or binaryPath() from a program',
      ...common(version),
      type: 'module',
      main: './index.js',
      types: './index.d.ts',
      exports: {'.': {types: './index.d.ts', default: './index.js'}, './package.json': './package.json'},
      bin: {'rein-remote': './bin/rein-remote.js'},
      files: ['index.js', 'index.d.ts', 'bin/rein-remote.js', 'README.md', 'LICENSE'],
      optionalDependencies: Object.fromEntries(Object.values(PLATFORMS).map((p) => [platformName(p), version])),
      engines: {node: '>=18'},
    },
    'index.js': readFileSync(path.join(root, 'npm', 'server', 'index.js'), 'utf8'),
    'index.d.ts': readFileSync(path.join(root, 'npm', 'server', 'index.d.ts'), 'utf8'),
    'bin/rein-remote.js': readFileSync(path.join(root, 'npm', 'server', 'rein-remote.js'), 'utf8'),
    'README.md': readFileSync(path.join(root, 'npm', 'server.README.md'), 'utf8'),
    LICENSE: license,
  });
  chmodSync(path.join(server, 'bin', 'rein-remote.js'), 0o755);
  return {version, packages: [`${SCOPE}/remote`, `${SCOPE}/remote-server`, ...made]};
}

if (process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1])) {
  try {
    const a = args(process.argv.slice(2));
    for (const f of Object.values(a.bins)) if (!existsSync(f)) throw new Error(`no binary at ${f}`);
    const r = build(a);
    console.log(`built ${r.packages.join(', ')} ${r.version} into ${a.out}`);
  } catch (err) {
    console.error(`npm/build.mjs: ${err.message}`);
    process.exit(2);
  }
}
