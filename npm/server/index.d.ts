/** The platform package's name for this machine (e.g. "@rein-harness/remote-server-darwin-arm64"), or undefined where there's no build. */
export function platformPackage(platform?: string, arch?: string): string | undefined;
/** The path of the rein-remote binary for this machine (REIN_REMOTE_BINARY when set). Throws, saying why, when there's none. */
export function binaryPath(): string;
