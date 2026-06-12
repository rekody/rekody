# rekody-hud IPC protocol v1

Transport: unix domain socket. Daemon LISTENS at
`~/Library/Application Support/rekody/hud.sock` (path overridable via
`$REKODY_HUD_SOCK`). Helper connects as a client. Framing: NDJSON — one JSON
object per line, UTF-8, `e` field tags the event.

## daemon → helper

```json
{"e":"hello","version":"0.5.11","position":"bottom"}
{"e":"state","s":"idle"}
{"e":"state","s":"listening","handsfree":false}
{"e":"level","rms":0.073}
{"e":"partial","tail":"…and the final text still lands the instant"}
{"e":"state","s":"working","verb":"inserting…"}
{"e":"done","words":94,"ms":52}
{"e":"error","msg":"mic lost — take saved","recoverable":true}
{"e":"bye"}
```

- `level` ≤60Hz, only while listening. rms is raw mean-square root 0.0–1.0;
  helper applies sqrt scaling for display.
- `partial.tail` is pre-trimmed by the daemon to ≤60 chars (helper renders
  only; daemon owns truncation).
- `done` implies a return to idle after the helper's 700ms fade.
- `hello` is sent once per connection, immediately on accept.

## helper → daemon

```json
{"e":"hello","version":"0.5.11"}
```

v1 is display-only: no actions from helper (stop/cancel stay on
⌥space/Esc via the existing CGEventTap). `action` events are reserved
for v1.1 (click-to-stop in hands-free).

## Lifecycle

- Daemon spawns the helper at pipeline start when (a) config `hud = true`
  (default) and (b) the `rekody-hud` binary is found next to the daemon
  executable or at `$REKODY_HUD_BIN`. Passes `--socket <path>`.
- Daemon supervises: relaunch on child exit with 5s backoff. Kills child on
  daemon shutdown.
- Helper exits when the socket EOFs (daemon gone) — never a zombie pill.
- Version handshake: helper compares major.minor; on mismatch it shows
  nothing and exits 2 (daemon logs a warning).
