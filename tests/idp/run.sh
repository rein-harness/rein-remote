#!/bin/bash
# Phase 6 verification, one command: Keycloak (OIDC and SAML) + rein-remote (an xterm on Xvfb; OIDC
# on :7979, SAML on :7980, a policy, audit logs, recording) + headless Chromium. Two users sign in;
# alice (rr-admins) types into the xterm and her command runs; bob (rr-viewers) streams and his doesn't;
# carol (no group) is refused; the audit logs have it all and no secrets; alice's recordings decode.
# Results in out/. Build first: ./build.sh (rein-remote with SAML, in Docker). KEEP=1 leaves it running.
set -uo pipefail
D="$(cd "$(dirname "$0")" && pwd)"
OUT="$D/out"; rm -rf "$OUT"; mkdir -p "$OUT"
KC_IMAGE="${KC_IMAGE:-quay.io/keycloak/keycloak:26.2.5}"
BROWSER_IMAGE="${BROWSER_IMAGE:-rr-p2fix-phone}"  # node 22 + Debian's Chromium + puppeteer-core (tests/e2e/rein-preview)
NET=rr-idp-net
cleanup() { docker rm -f rr-idp-kc rr-idp-host rr-idp-browser >/dev/null 2>&1; docker network rm $NET >/dev/null 2>&1; }
docker image inspect rr-idp-host >/dev/null 2>&1 || { echo "run ./build.sh first"; exit 1; }
docker image inspect "$BROWSER_IMAGE" >/dev/null 2>&1 || docker build -q -f "$D/../e2e/rein-preview/Dockerfile.phone" -t "$BROWSER_IMAGE" "$D/../e2e/rein-preview" >/dev/null
# REUSE=1 (with a Keycloak left running by KEEP=1): a fresh rein-remote machine, the same Keycloak (it
# takes minutes to start in a small VM).
if [ -n "${REUSE:-}" ] && [ "$(docker inspect -f '{{.State.Running}}' rr-idp-kc 2>/dev/null)" = true ]; then
  docker rm -f rr-idp-host rr-idp-browser >/dev/null 2>&1
  echo "== Keycloak: the one already running"
else
  cleanup
  docker network create $NET >/dev/null
  echo "== Keycloak (realm rr: alice in rr-admins, bob in rr-viewers, carol in neither)"
  docker run -d --name rr-idp-kc --network $NET --network-alias idp.test -m 1g \
    -e KC_BOOTSTRAP_ADMIN_USERNAME=admin -e KC_BOOTSTRAP_ADMIN_PASSWORD=admin -e JAVA_OPTS_KC_HEAP="-Xms256m -Xmx640m" \
    -v "$D/realm.json:/opt/keycloak/data/import/realm.json:ro" \
    "$KC_IMAGE" start-dev --import-realm --http-port 8080 --hostname http://idp.test:8080 >/dev/null
fi
docker run -d --name rr-idp-host --network $NET --network-alias rr.test -v "$OUT:/out" rr-idp-host >/dev/null
for i in $(seq 600); do [ -f "$OUT/ready" ] && break; sleep 1; done
[ -f "$OUT/ready" ] || { echo "FAIL rein-remote didn't come up"; docker logs rr-idp-kc 2>&1 | tail -20; tail -5 "$OUT"/serve-*.log; cleanup; exit 1; }
echo "ready after ${i}s"; grep -h "sign-in with\|OIDC with\|SAML with" "$OUT"/serve-*.log

echo "== the browser"
docker create --name rr-idp-browser --network $NET -w /phone "$BROWSER_IMAGE" node check.mjs >/dev/null
docker cp "$D/browser/check.mjs" rr-idp-browser:/phone/check.mjs
( while :; do docker stats --no-stream --format '{{.Name}} cpu={{.CPUPerc}} mem={{.MemUsage}}' rr-idp-kc rr-idp-host rr-idp-browser 2>/dev/null >> "$OUT/docker-stats.txt"; sleep 5; done ) &
SAMPLER=$!
docker start -a rr-idp-browser 2>&1 | tee "$OUT/browser.log"
docker cp rr-idp-browser:/out/. "$OUT/" 2>/dev/null
kill $SAMPLER 2>/dev/null
sleep 1

FAIL=0
check() { if eval "$2"; then echo "PASS $1"; else echo "FAIL $1"; FAIL=1; fi; }
echo "== inside the machine: which typed commands ran"
docker exec rr-idp-host sh -c 'ls /tmp/proof-* 2>/dev/null' | tee "$OUT/proofs.txt"
for p in oidc saml; do
  check "$p: alice's command ran" "grep -qx /tmp/proof-$p-alice '$OUT/proofs.txt'"
  check "$p: bob's command didn't" "! grep -q proof-$p-bob '$OUT/proofs.txt'"
done
check "the browser's checks" "jq -e '[.steps[] | .ok] | all and length == 7' '$OUT/result.json' >/dev/null"

echo "== audit logs"
for p in oidc saml; do
  A="$OUT/audit-$p.jsonl"
  jq -c '{event, user, via, rule, transport, view_only, input_dropped, reason, recording}' "$A" | sed 's/,"[a-z_]*":null//g' | tee "$OUT/audit-$p-summary.txt"
  check "$p: alice signed in" "jq -e 'select(.event==\"signin\" and .user==\"alice@corp.example\" and .via==\"$p\" and .rule==\"admins\")' $A >/dev/null"
  check "$p: bob signed in (viewers)" "jq -e 'select(.event==\"signin\" and .user==\"bob@corp.example\" and .rule==\"viewers\")' $A >/dev/null"
  check "$p: carol refused" "jq -e 'select(.event==\"signin_denied\" and .user==\"carol@corp.example\")' $A >/dev/null"
  check "$p: bob's session view only, input dropped" "jq -e 'select(.event==\"session_end\" and .user==\"bob@corp.example\" and .input_dropped > 0)' $A >/dev/null && jq -e 'select(.event==\"session_start\" and .user==\"bob@corp.example\" and .view_only==true and .addr != null and .transport != null)' $A >/dev/null"
  check "$p: alice's session recorded" "jq -e 'select(.event==\"session_end\" and .user==\"alice@corp.example\" and .recording != null)' $A >/dev/null"
  check "$p: no secret in the audit log" "! grep -q 'rr-test-oidc-secret\|ticket=\|rr_session\|-pw' $A"
done
check "the WebSocket without sign-in is refused (audited)" "jq -e 'select(.event==\"session_refused\")' $OUT/audit-oidc.jsonl >/dev/null"

echo "== recordings"
docker exec rr-idp-host sh -c 'for f in /out/rec/*.mp4; do printf "%s " "$(basename "$f")"; ffprobe -v error -count_frames -show_entries stream=codec_name,width,height,nb_read_frames -show_entries format=duration -of compact=p=0:nk=1 "$f" | tr "\n" " "; ffmpeg -v error -xerror -i "$f" -f null - && echo decoded-ok || echo decode-FAILED; done' | tee "$OUT/recordings.txt"
check "recordings: only alice's, one per protocol" "[ \$(grep -c . '$OUT/recordings.txt') = 2 ] && ! grep -q bob '$OUT/recordings.txt'"
check "recordings decode with ffmpeg, every frame" "[ \$(grep -c decoded-ok '$OUT/recordings.txt') = 2 ]"

echo "== memory and CPU (peak per container)"
sort -t= -k3 "$OUT/docker-stats.txt" | awk '{print $1}' | sort -u | while read n; do grep "^$n " "$OUT/docker-stats.txt" | tail -1; done
[ $FAIL = 0 ] && echo "ALL PASS" || echo "SOME FAILED"
[ -n "${KEEP:-}" ] || cleanup
exit $FAIL
