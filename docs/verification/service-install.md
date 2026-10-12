# Headless install and config by file

## Config by file

`rein-remote serve --config FILE.toml` and `rein-remote relay --config FILE.toml` take their flags from
a TOML file. Each key is a long flag's name without its dashes, so every flag works from the file,
including ones added later:

```toml
# serve.toml
listen = "0.0.0.0:7979"
test-pattern = "1280x720"      # --test-pattern 1280x720 (true: the default size)
fps = 60
webtransport = false           # --webtransport off (true is on)
relay = ["wss://relay-a.example", "wss://relay-b.example"]   # a list: the flag once each
token = { file = "/etc/rein-remote/token" }                   # a secret: the first line of that file
relay-secret = { file = "relay-secret" }                      # relative: to the config file's folder
```

- **The command line wins:** a flag given there replaces every value the file has for it
  (`--relay X` on the command line means only X, not X and the file's list).
- **Then the file, then the environment** (`REIN_REMOTE_TOKEN`, `REIN_REMOTE_RELAY_SECRET`): a token
  in the file replaces the environment's.
- **Secrets** go in files of their own (`{ file = "…" }`), read by rein-remote itself: a service's
  command line, which `ps`, `systemctl status`, `launchctl print` and `sc.exe qc` show to anyone on
  the machine, has only `--config FILE`. rein-remote warns when such a file can be read by others than
  its owner (`chmod 600` it).
- Mistakes are said before anything starts: a key with dashes, a section (`[serve]`), a secret's file
  that isn't there or is empty, a value a flag doesn't take.

## Services

```sh
rein-remote install-service [serve|relay] [--config FILE.toml] [--system] [--name NAME] [--print]
rein-remote uninstall-service [serve|relay] [--system] [--name NAME] [--print]
```

| | the user's (the default) | the system's (`--system`, as root / Administrator) |
|---|---|---|
| Linux | systemd user unit, `~/.config/systemd/user/NAME.service`, `systemctl --user enable --now` | systemd unit, `/etc/systemd/system/NAME.service` (a relay: `ProtectSystem=strict`, `NoNewPrivileges`) |
| macOS | LaunchAgent, `~/Library/LaunchAgents/com.rein-harness.NAME.plist`, in the user's GUI session (`LimitLoadToSessionType Aqua`: where the screen can be captured), log in `~/Library/Logs/NAME.log` | LaunchDaemon, `/Library/LaunchDaemons/…`, log in `/Library/Logs/NAME.log` |
| Windows | a scheduled task at logon (`schtasks /SC ONLOGON`): the user's desktop | a Windows service (`sc.exe create`, automatic start, restarted on failure), running `rein-remote service-run NAME …` (the service manager's protocol); log in `%ProgramData%\rein-remote\NAME.log` |

Uninstalling stops the service, disables it, removes its file, then tells the service manager
(systemd: `stop`, `disable`, the file, `daemon-reload`; launchd: `bootout`, the file; Windows:
`sc.exe stop` and `delete`, or `schtasks /End` and `/Delete`).

`NAME` is `rein-remote` for serve and `rein-remote-relay` for a relay unless `--name` says otherwise.
The config file must read (it's checked first); the unit runs the binary where it is now (install it
somewhere it stays, not in a build folder).

**serve wants the user's session** (the screen, the windows, the sound are the user's): a system
service sees no one's screen. `serve --system` suits `--test-pattern`, `--vnc`, `--pipe` and Linux's
`--kms`. On Linux, a user unit needs the session's `DISPLAY`: `systemctl --user import-environment
DISPLAY XAUTHORITY` once per login (most desktops do it), or `display = ":0"` in the config. **A relay**
belongs to the system: `install-service relay --system`.

`--print` (or `--dry-run`) writes the unit file with its path and the commands it would run, and
changes nothing:

```text
$ rein-remote install-service relay --config /etc/rein-remote/relay.toml --system --print
# /etc/systemd/system/rein-remote-relay.service
# Written by `rein-remote install-service`; `rein-remote uninstall-service` removes it.
[Unit]
Description=Rein Remote relay (rein-remote-relay)
Documentation=https://github.com/rein-harness/rein-remote
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/rein-remote relay --config /etc/rein-remote/relay.toml
Restart=on-failure
RestartSec=2
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=read-only
PrivateTmp=yes

[Install]
WantedBy=multi-user.target

$ systemctl daemon-reload
$ systemctl enable --now rein-remote-relay.service
$ systemctl restart rein-remote-relay.service
```

## What's verified, and how

- **Unit tests** (`crates/rr-server/src/service.rs`, `config.rs`): the systemd unit, the plist and the
  Windows commands for every OS on every OS (quoting of spaces and `%`, user and system paths,
  launchd's GUI domain); config keys to flags, lists, `true`/`false`, secrets from files, the command
  line winning, and each mistake said.
- **Integration** (`crates/rr-server/tests/config.rs`, all three OSes): `serve` and `relay` started from
  config files, the token from a file of its own (`/healthz` refuses without it, takes it as a bearer);
  `install-service --print` for this OS, user and system.
- **CI installs for real** (`ci.yml`, "Service install" steps), a relay from a config whose secret is in a
  file, on each OS: up, `/healthz` and `/metrics` answer, the secret is on no process's command line,
  then uninstalled and gone:
  - Linux: as a systemd **user** unit (after `loginctl enable-linger`; if the runner has no user
    manager, CI says so in a warning and the user unit is only printed) and as a **system** unit
    (`sudo`).
  - macOS: as a **LaunchAgent** in the runner's GUI session (CI warns if the runner has none) and as a
    **LaunchDaemon** (`sudo`).
  - Windows: as a **Windows service** (sc.exe, `service-run`: the runner is an Administrator) and as a
    **scheduled task** at logon.
- **By hand on macOS** (this repository's Mac, Apple silicon): `install-service relay --name rr-p6-test
  --config relay.toml` wrote the LaunchAgent, `launchctl print` showed it running with
  `relay --config …/relay.toml` as its arguments (no secret), `/healthz` and `/metrics` answered, and
  `uninstall-service` stopped it and removed the plist.

## Manual test (a real machine, each OS)

1. Write `relay.toml` (`listen = "127.0.0.1:7990"`, `secret = { file = "secret" }`) and `secret` (`chmod
   600`), then `rein-remote install-service relay --config relay.toml --print`: read it.
2. `rein-remote install-service relay --config relay.toml` (and with `--system` as root /
   Administrator): `curl http://127.0.0.1:7990/healthz` says `"status":"ok"`.
3. The secret isn't on its command line: `ps -eo args | grep rein-remote` / `launchctl print
   gui/$(id -u)/com.rein-harness.rein-remote-relay` / `sc.exe qc rein-remote-relay` and
   `Get-CimInstance Win32_Process -Filter "Name='rein-remote.exe'" | select CommandLine`.
4. Reboot (or log out and in, for the user's kind): it's running again.
5. Kill it (`kill -9`, Task Manager): it's started again within seconds.
6. `rein-remote uninstall-service relay` (with `--system` if it was): stopped, and the file is gone.
7. For serve: `rein-remote install-service --config serve.toml` in a desktop session with `window =
   "…"` or nothing (the screen): open the link it prints in its log (`journalctl --user -u
   rein-remote`, `~/Library/Logs/rein-remote.log`) after logging in again.
