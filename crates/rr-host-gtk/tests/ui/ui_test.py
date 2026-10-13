"""The Linux host app driven through AT-SPI (pyatspi), as a screen reader sees it.

Each step acts on the app's widgets by their accessible names and roles, then checks the result at
the host itself (its control API, host.toml) and in the app. run.sh sets up Xvfb, D-Bus, the AT-SPI
bus and a fresh home folder; the app starts `rein-remote host --test-pattern 1280x720` itself.
"""

import json
import os
import re
import signal
import subprocess
import sys
import time
import urllib.request

import pyatspi

HOME = os.environ["REIN_REMOTE_HOME"]
SHOTS = os.environ.get("SHOTS", "shots")
REIN_REMOTE = os.environ["REIN_REMOTE"]
APP_PID = int(os.environ["APP_PID"])
T0 = time.time()
used = {}  # which way each kind of action went (AT-SPI, or a fallback), for the log


def log(*a):
    print(f"[{time.time() - T0:6.1f}s]", *a, flush=True)


def fail(msg):
    log("FAIL:", msg)
    shot("failure")
    dump_tree()
    sys.exit(1)


# The host, at its control API (the ground truth the UI is checked against).

def control():
    with open(os.path.join(HOME, "control.json")) as f:
        return json.load(f)


def api(method, path, body=None):
    c = control()
    req = urllib.request.Request(c["url"] + path, method=method,
                                 data=None if body is None else json.dumps(body).encode(),
                                 headers={"Authorization": "Bearer " + c["token"], "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.loads(r.read())


def until(what, f, timeout=30, every=0.3):
    end = time.time() + timeout
    last = None
    while time.time() < end:
        try:
            v = f()
            if v:
                return v
            last = v
        except Exception as e:  # noqa: BLE001 (the host may be restarting)
            last = e
        time.sleep(every)
    fail(f"timed out waiting for {what} (last: {last!r})")


# The app, through AT-SPI.

def app_root():
    desktop = pyatspi.Registry.getDesktop(0)
    for a in desktop:
        if a is None:
            continue
        try:
            if a.get_process_id() == APP_PID or a.name in ("rein-remote-host-gtk", "Rein Remote"):
                return a
        except Exception:  # noqa: BLE001
            continue
    return None


def walk(node, depth=0):
    if node is None or depth > 60:
        return
    yield node
    try:
        n = node.childCount
    except Exception:  # noqa: BLE001
        return
    for i in range(n):
        try:
            c = node.getChildAtIndex(i)
        except Exception:  # noqa: BLE001
            continue
        yield from walk(c, depth + 1)


def showing(n):
    try:
        return n.getState().contains(pyatspi.STATE_SHOWING)
    except Exception:  # noqa: BLE001
        return False


def nodes(name=None, role=None, contains=None, show=True):
    root = app_root()
    out = []
    for n in walk(root):
        try:
            nm, rl = n.name or "", n.getRoleName()
        except Exception:  # noqa: BLE001
            continue
        if name is not None and nm != name:
            continue
        if contains is not None and contains not in nm:
            continue
        if role is not None and rl not in (role if isinstance(role, tuple) else (role,)):
            continue
        if show and not showing(n):
            continue
        out.append(n)
    return out


def find(name=None, role=None, contains=None, timeout=20):
    return until(f"{role or 'widget'} {name or contains!r}",
                 lambda: (nodes(name, role, contains) or [None])[0], timeout)


def text_of(n):
    try:
        t = n.queryText()
        return t.getText(0, t.characterCount)
    except NotImplementedError:
        return n.name or ""


def sensitive(n):
    return n.getState().contains(pyatspi.STATE_SENSITIVE)


def checked(n):
    s = n.getState()
    return s.contains(pyatspi.STATE_CHECKED) or s.contains(pyatspi.STATE_PRESSED)


def act(n, prefer=("click", "activate", "press", "toggle")):
    """Does the widget's action (a click), by AT-SPI's Action interface."""
    a = n.queryAction()
    names = [a.getName(i) for i in range(a.nActions)]
    for p in prefer:
        if p in names:
            a.doAction(names.index(p))
            used.setdefault("action", set()).add(p)
            return
    if a.nActions:
        a.doAction(0)
        used.setdefault("action", set()).add(names[0])
        return
    raise RuntimeError(f"{n.name!r} ({n.getRoleName()}) has no actions")


def click(name, role=("push button", "button", "toggle button", "check box", "radio button", "switch"), timeout=20):
    n = find(name, role, timeout=timeout)
    until(f"{name!r} to be sensitive", lambda: sensitive(n), timeout)
    log("click", repr(name), f"({n.getRoleName()})")
    act(n)
    return n


def gtk_action(name, param=None):
    """The app's GActions, as GTK exports them on the session bus (org.gtk.Actions)."""
    args = f"[<'{param}'>]" if param is not None else "[]"
    subprocess.run(["gdbus", "call", "--session", "--dest", "dev.reinharness.RemoteHost",
                    "--object-path", "/dev/reinharness/RemoteHost", "--method", "org.gtk.Actions.Activate",
                    name, args, "{}"], check=True, capture_output=True)


PAGE_TITLES = {"sharing": "Sharing", "viewers": "Viewers", "devices": "Paired devices", "settings": "Settings",
               "signin": "Sign-in", "policy": "Policy", "sessions": "Live sessions", "audit": "Audit log",
               "metrics": "Metrics"}


def goto(page):
    """A page, by its row in the sidebar (AT-SPI: the row's action, else selecting it in the list)."""
    title = PAGE_TITLES[page]
    rows = [r for r in nodes(title, ("list item", "list box row", "row")) if r]
    done = False
    if rows:
        r = rows[0]
        try:
            act(r, ("activate", "click", "select"))
            done = True
            used.setdefault("page", set()).add("row action")
        except Exception:  # noqa: BLE001
            try:
                lst = r.parent
                lst.querySelection().selectChild(r.getIndexInParent())
                done = True
                used.setdefault("page", set()).add("selection")
            except Exception:  # noqa: BLE001
                pass
    if not done:
        gtk_action("show-page", page)
        used.setdefault("page", set()).add("org.gtk.Actions show-page")
    time.sleep(0.6)


def shot(name):
    os.makedirs(SHOTS, exist_ok=True)
    subprocess.run(["import", "-window", "root", os.path.join(SHOTS, f"{name}.png")], check=False)


def dump_tree():
    try:
        for n in walk(app_root()):
            if showing(n) and n.name:
                print("   ", n.getRoleName(), repr(n.name))
    except Exception as e:  # noqa: BLE001
        print("no tree:", e)


def set_text(n, text):
    """Types into a text widget: AT-SPI's EditableText, else focus and the keyboard (xdotool)."""
    try:
        e = n.queryEditableText()
        e.setTextContents(text)
        if text_of(n) == text:
            used.setdefault("text", set()).add("EditableText")
            return
    except NotImplementedError:
        pass
    n.queryComponent().grabFocus()
    time.sleep(0.2)
    subprocess.run(["xdotool", "key", "ctrl+a", "BackSpace"], check=True)
    subprocess.run(["xdotool", "type", "--delay", "5", text], check=True)
    used.setdefault("text", set()).add("xdotool")


def main():
    log("waiting for the app on the AT-SPI bus")
    until("the app on the AT-SPI bus", app_root, 30)
    log("app:", app_root().name)

    # The app started the host (none ran): its control.json answers, and the app is connected.
    st = until("the host the app started", lambda: api("GET", "/control/state"), 30)
    log("host pid", st["pid"], "source", st["source"])
    start = find("Start sharing", "push button", timeout=30)
    until("Start sharing to be sensitive", lambda: sensitive(start), 30)
    assert not st["sharing"], st

    # Start and stop.
    click("Start sharing")
    until("sharing on at the host", lambda: api("GET", "/control/state")["sharing"])
    find("Stop sharing", "push button")
    find(contains="this computer")  # a link's row
    log("sharing; links:", [l["kind"] for l in api("GET", "/control/state")["links"]])
    click("Stop sharing")
    until("sharing off at the host", lambda: not api("GET", "/control/state")["sharing"])
    find("Start sharing", "push button")
    click("Start sharing")
    until("sharing on again", lambda: api("GET", "/control/state")["sharing"])
    time.sleep(1)
    shot("sharing")

    # The source: the display (Xvfb), then the test pattern, each by its radio button.
    src = api("GET", "/control/sources")
    log("sources:", json.dumps(src)[:400])
    display = src["displays"][0]
    click(display["name"], ("radio button", "check box"))
    until(f"source {display['id']}", lambda: api("GET", "/control/state")["source"] == display["id"])
    pattern = src["other"][0]
    click(pattern["name"], ("radio button", "check box"))
    until(f"source {pattern['id']}", lambda: api("GET", "/control/state")["source"] == pattern["id"])
    log("source switched to", display["id"], "and back to", pattern["id"])

    # A new pairing code: the label shows the host's new code.
    code_label = find("Pairing code", "label")
    old = api("GET", "/control/state")["pairing"]["code"]
    until("a pairing code in the app", lambda: text_of(code_label) == old)
    click("New code")
    new = until("a new code at the host", lambda: (lambda c: c if c and c != old else None)(api("GET", "/control/state")["pairing"]["code"]))
    until("the new code in the app", lambda: text_of(find("Pairing code", "label")) == new)
    log("pairing code", old, "->", new)
    find("QR code for pairing")

    # A viewer appears; it's kicked (asked first).
    local = [l for l in api("GET", "/control/state")["links"] if l["kind"] == "local"][0]["url"]
    viewer = subprocess.Popen([REIN_REMOTE, "connect", local, "--seconds", "90"],
                              stdout=subprocess.DEVNULL, stderr=open("/tmp/viewer.log", "w"))
    v = until("a viewer at the host", lambda: (api("GET", "/control/viewers")["viewers"] or [None])[0], 30)
    who = v["device"] or v["user"] or f"Viewer {v['id']}"
    log("viewer:", v["id"], who, v["transport"])
    goto("viewers")
    kick_button = find(f"Kick {who}", "push button", timeout=20)
    find(contains=f"via {v['via']}")
    time.sleep(0.5)
    shot("viewers")
    act(kick_button)
    find("Kick viewer", "push button")  # the dialog's
    shot("viewers-kick-confirm")
    click("Kick viewer")
    until("the viewer gone at the host", lambda: not any(x["id"] == v["id"] for x in api("GET", "/control/viewers")["viewers"]))
    until("the viewer gone from the app", lambda: not nodes(f"Kick {who}", "push button"))
    log("kicked", v["id"])
    viewer.send_signal(signal.SIGTERM)

    # The audit log shows the kick.
    goto("audit")
    click("Event session_kicked", ("check box", "toggle button"))
    click("Search the audit log")
    row = until("the kick in the audit log",
                lambda: [n for n in nodes(contains="session_kicked") if not (n.name or "").startswith("Event ")])
    log("audit row:", row[0].name)
    until("the session id in the audit page", lambda: nodes(contains=v["id"]))
    time.sleep(0.5)
    shot("audit")

    # A setting saved to host.toml: quality first, mDNS (off from the command line) left alone, then
    # the frame rate.
    goto("settings")
    click("Quality first", ("radio button", "check box"))
    click("Save settings")
    toml = os.path.join(HOME, "host.toml")
    until("prefer = quality in host.toml", lambda: re.search(r'^prefer\s*=\s*"quality"', open(toml).read(), re.M))
    find(contains="Saved.")
    assert api("GET", "/control/settings")["settings"]["prefer"] == "quality"
    log("host.toml:", open(toml).read().replace("\n", " | "))
    time.sleep(0.5)
    shot("settings")
    # A value the host refuses: its message, shown.
    fps = find("Frame rate", ("text", "entry"))
    set_text(fps, "999")
    click("Save settings")
    find(contains="fps:", timeout=20)
    log("refused fps shown:", nodes(contains="fps:")[0].name)
    click("Revert the settings")

    # A bad policy shows the parser's error, and nothing changes; a good one is saved.
    goto("policy")
    view = find("Policy text", ("text", "entry", "document text"))
    set_text(view, '[[rule]]\nname = "everyone, view only"\nview_ony = true\n')
    click("Check and save the policy")
    err = find(contains="view_ony", role="label", timeout=20)
    log("policy error shown:", err.name)
    assert not api("GET", "/control/admin/policy")["enabled"]
    time.sleep(0.5)
    shot("policy-error")
    set_text(view, '[[rule]]\nname = "everyone"\n')
    click("Check and save the policy")
    until("the policy on at the host", lambda: api("GET", "/control/admin/policy")["enabled"])
    find("1 rule")

    # The other pages, for their screenshots (and that they draw with the host's answers).
    goto("sessions")
    find("No sessions", timeout=20)
    shot("sessions")
    goto("metrics")
    find(contains="sessions total", timeout=20)
    shot("metrics")
    goto("signin")
    find("OpenID Connect", ("radio button", "check box"))
    shot("signin")
    goto("devices")
    find("No paired devices")
    shot("devices")

    # The tray icon's D-Bus item and its menu.
    names = subprocess.run(["gdbus", "call", "--session", "--dest", "org.freedesktop.DBus", "--object-path",
                            "/org/freedesktop/DBus", "--method", "org.freedesktop.DBus.ListNames"],
                           check=True, capture_output=True, text=True).stdout
    sni = re.findall(r"org\.kde\.StatusNotifierItem-\d+-\d+", names)
    if not sni:
        fail(f"no StatusNotifierItem on the bus: {names}")
    intro = subprocess.run(["gdbus", "introspect", "--session", "--dest", sni[0], "--object-path", "/StatusNotifierItem"],
                           check=True, capture_output=True, text=True).stdout
    assert "org.kde.StatusNotifierItem" in intro, intro
    menu = subprocess.run(["gdbus", "call", "--session", "--dest", sni[0], "--object-path", "/MenuBar", "--method",
                           "com.canonical.dbusmenu.GetLayout", "0", "-1", "[]"],
                          check=True, capture_output=True, text=True).stdout
    for label in ("Sharing, no one watching", "Stop sharing", "Pairing code: ", "0 viewers", "Open Rein Remote", "Quit"):
        if label not in menu:
            fail(f"the tray menu has no {label!r}: {menu}")
    log("tray:", sni[0], "menu ok")

    # The host goes: the app says so (and doesn't crash), starts it again, and reconnects.
    goto("sharing")
    old_pid = control()["pid"]
    os.kill(old_pid, signal.SIGKILL)
    find(contains="looking for it again", timeout=20)
    shot("host-gone")
    log("host killed: the app says so")
    until("a new host", lambda: control()["pid"] != old_pid and api("GET", "/control/state"), 40)
    find("Stop sharing", "push button", timeout=30)  # sharing = true was remembered
    if subprocess.run(["kill", "-0", str(APP_PID)]).returncode != 0:
        fail("the app exited")
    log("reconnected to host", control()["pid"])

    log("used:", {k: sorted(v) for k, v in used.items()})
    log("PASS")


if __name__ == "__main__":
    main()
