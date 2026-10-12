# Verifying sign-in, policies, the audit log and recording against a real identity provider

`tests/idp/run.sh` checks all of this in Docker against Keycloak (OIDC and SAML). This page is for a
real provider: Okta, Microsoft Entra ID or Google for OIDC, and Okta or Entra for SAML. It takes about
half an hour and needs an admin account at the provider and two test users.

## What should happen

- A browser that opens the machine's page without a token is sent to the provider to sign in, and
  comes back to the stream.
- The policy decides what each user may do: one user controls the machine, another only watches
  (their typing never reaches it), and a user no rule names is refused with the reason.
- The audit log has a line for each sign-in, refusal, session start and end, with no secrets in it.
- A recorded session plays afterwards in any player.

## You need

- A machine to reach with `rein-remote` built from this branch (`cargo build --release -p rr-server`;
  for SAML, `--features saml`, which needs `libxml2-dev libxmlsec1-dev libclang-dev libssl-dev
  pkg-config` on Debian/Ubuntu, or `brew install libxmlsec1 pkg-config` on a Mac).
- **HTTPS in front of it with a real name**, e.g. `https://remote.example.com` (Caddy, nginx, or
  `--tls-cert`/`--tls-key`). The session cookie is `Secure` over HTTPS; providers refuse plain-HTTP
  redirect URIs other than `localhost`. Below, `PUBLIC=https://remote.example.com`.
- Two test users at the provider, in two groups: `rr-admins` (user A) and `rr-viewers` (user B), and
  a third user in neither (user C).

## 1. Register the application at the provider

The redirect URI (OIDC) is `$PUBLIC/sso/oidc/callback`; the ACS URL (SAML) is `$PUBLIC/sso/saml/acs`
and this service provider's entity ID is `$PUBLIC/sso/saml/metadata` unless you give
`--saml-entity-id` (its metadata is served at that address too).

**Okta (OIDC):** Applications → Create App Integration → OIDC, Web Application. Sign-in redirect URI:
`$PUBLIC/sso/oidc/callback`. Grant type: Authorization Code (PKCE is used either way). Assign the
three users. Sign On → OpenID Connect ID Token → Groups claim: type *Filter*, name `groups`, *Matches
regex* `rr-.*`. The issuer is `https://YOURORG.okta.com` (or your authorization server's,
`https://YOURORG.okta.com/oauth2/default`).

**Microsoft Entra ID (OIDC):** App registrations → New registration, Web, redirect URI
`$PUBLIC/sso/oidc/callback`. Certificates & secrets → a client secret. Token configuration → Add
groups claim → Security groups (Entra puts group **object IDs** in `groups`: write those IDs in the
policy, or use app roles with `--oidc-groups-claim roles`). Entra's ID tokens may lack `email`; use
`--oidc-user-claim preferred_username`. The issuer is
`https://login.microsoftonline.com/TENANT_ID/v2.0`.

**Google (OIDC):** Google Cloud console → APIs & Services → Credentials → OAuth client ID, Web
application, redirect URI `$PUBLIC/sso/oidc/callback`. The issuer is `https://accounts.google.com`.
Google's ID tokens carry no groups: write the policy with `users = [...]` (or `*@yourdomain.com`).

**Okta (SAML):** Applications → Create App Integration → SAML 2.0. Single sign-on URL
`$PUBLIC/sso/saml/acs`, Audience URI `$PUBLIC/sso/saml/metadata`, Name ID format EmailAddress,
Application username Email. Group Attribute Statements: name `groups`, filter *Starts with* `rr-`.
Assertion Signature: Signed (the default). Copy the "Identity Provider metadata" URL.

**Microsoft Entra ID (SAML):** Enterprise applications → New → Create your own → Non-gallery. Single
sign-on → SAML: Identifier `$PUBLIC/sso/saml/metadata`, Reply URL `$PUBLIC/sso/saml/acs`. Attributes
& Claims → Add a group claim, and name it `groups` (Customize the name; Entra's default is a long URI:
or pass that URI as `--saml-groups-attr`). Signing option: *Sign SAML assertion* (or response and
assertion). Copy the App Federation Metadata Url.

## 2. The policy

`policy.toml` on the machine:

```toml
[[rule]]
name = "admins"
groups = ["rr-admins"]     # Entra: the group's object ID
record = true

[[rule]]
name = "viewers"
groups = ["rr-viewers"]
view_only = true
clipboard = false
max_session_minutes = 5
```

## 3. Start it

OIDC:

```sh
printf '%s' 'THE-CLIENT-SECRET' > ~/.rr-oidc-secret && chmod 600 ~/.rr-oidc-secret
rein-remote serve --listen 127.0.0.1:7979 --public-url "$PUBLIC" \
  --oidc-issuer https://YOURORG.okta.com --oidc-client-id CLIENT_ID --oidc-client-secret-file ~/.rr-oidc-secret \
  --policy policy.toml --audit audit.jsonl --record recordings
```

(Behind your HTTPS proxy, which forwards `$PUBLIC` to `127.0.0.1:7979`, WebSocket upgrades included.)
It says `OIDC with https://… (redirect URI …)`. A wrong issuer, client or unreachable provider fails
here, at start.

SAML (a separate run, or a second instance on another port with its own `--public-url`):

```sh
rein-remote serve --listen 127.0.0.1:7979 --public-url "$PUBLIC" \
  --saml-idp-metadata 'https://YOURORG.okta.com/app/…/sso/saml/metadata' \
  --policy policy.toml --audit audit.jsonl --record recordings
```

## 4. Check

In a private window for each user (so their sign-ins don't mix):

1. **User A** opens `$PUBLIC/`. The provider's sign-in page appears; after signing in the stream
   shows and the bar starts with `usera@… · ● recorded`. Click into a terminal on the machine and type
   `echo A was here > /tmp/rr-a`: it runs.
2. **User B** opens `$PUBLIC/`, signs in. The bar says `userb@… (view only)`. Type
   `echo B was here > /tmp/rr-b`: nothing happens on the machine, and `/tmp/rr-b` doesn't exist. Paste
   (Ctrl/Cmd-V) into the stream: nothing arrives. After five minutes from signing in, the stream ends
   and the bar says `the session reached its time limit (5 min): sign in again for another`.
3. **User C** opens `$PUBLIC/`, signs in, and gets *Not allowed: no rule in the policy lets
   userc@… in*.
4. Opening `$PUBLIC/stream` (a WebSocket) or `POST $PUBLIC/sso/ticket` with no sign-in: refused
   (403 / 401).
5. `audit.jsonl` (`jq -c . audit.jsonl`): `signin` for A and B with their rule, `signin_denied` for C
   with the reason, `session_start` for A (`view_only: false`) and B (`view_only: true`), each with
   the address and transport, `session_end` for B with `input_dropped` above 0 and
   `reason: "time limit"`, A's with `recording`. Search it for the client secret, `rr_session`,
   `ticket=`: none.
6. The recording (`recordings/…-usera@…-….mp4`) plays in VLC or QuickTime (H.264), or
   `ffprobe -count_frames recordings/*.mp4` reads every frame and
   `ffmpeg -v error -i FILE -fps_mode passthrough -enc_time_base -1 -f null -` says nothing (frames are timed by when
   they were captured, and a still screen sends none: the frame rate varies).
7. Sign out: `$PUBLIC/sso/logout`; opening `$PUBLIC/` goes back to the provider.

## Notes

- Tokens and the existing pairing still work next to sign-in when you give them: a `--token` link,
  or `--pair on`. With a policy, those viewers are matched by `via = ["token"]` and `via = ["paired"]`
  (refused unless a rule names them). With sign-in and no `--token`, `serve` makes none.
- The provider's clock and the machine's may differ by a minute (OIDC) or three (SAML).
- Sign-ins are kept in memory: restarting `rein-remote` signs everyone out.
