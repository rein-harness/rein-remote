// The rein-remote binary for this machine, from the platform package npm installed next to this one
// (one optional dependency per platform; npm installs only the one that matches).
import {existsSync} from 'node:fs';
import {createRequire} from 'node:module';
import path from 'node:path';

const require = createRequire(import.meta.url);

/** The platform package's name for this machine, or undefined where there's no build. */
export function platformPackage(platform = process.platform, arch = process.arch) {
  const ok = {darwin: ['arm64', 'x64'], linux: ['arm64', 'x64'], win32: ['x64']};
  return ok[platform]?.includes(arch) ? `@rein-harness/remote-server-${platform}-${arch}` : undefined;
}

/**
 * The path of the rein-remote binary: REIN_REMOTE_BINARY when set, else the platform package's.
 * Throws, saying why, when there's none for this machine.
 */
export function binaryPath() {
  const own = process.env.REIN_REMOTE_BINARY;
  if (own) return own;
  const pkg = platformPackage();
  if (!pkg) throw new Error(`rein-remote has no build for ${process.platform} ${process.arch}`);
  let dir;
  try {
    dir = path.dirname(require.resolve(`${pkg}/package.json`));
  } catch {
    throw new Error(`${pkg} isn't installed (npm skipped optional dependencies? reinstall without --omit=optional)`);
  }
  const bin = path.join(dir, 'bin', process.platform === 'win32' ? 'rein-remote.exe' : 'rein-remote');
  if (!existsSync(bin)) throw new Error(`${pkg} has no binary at ${bin}`);
  return bin;
}
