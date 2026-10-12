#!/bin/sh
# Prints the id of an iPhone simulator on the newest iOS runtime installed (CI's runners vary).
#   ./simulator.sh          # an iPhone
#   ./simulator.sh iPad     # an iPad
set -eu
xcrun simctl list devices available -j | python3 -c '
import json, sys
want = sys.argv[1]
d = json.load(sys.stdin)["devices"]
ios = sorted((k for k in d if ".iOS-" in k), key=lambda k: [int(x) for x in k.split(".iOS-")[1].split("-")])
print(next(x["udid"] for k in reversed(ios) for x in d[k] if x["name"].startswith(want)))
' "${1:-iPhone}"
