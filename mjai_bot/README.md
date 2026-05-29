# `mjai_bot/` — bot development

Each subdirectory here is one self-contained bot: a `bot.py` entry point, a
`pyproject.toml` declaring its Python dependencies, and an optional
`manifest.toml` describing user-configurable settings. Akagi spawns the bot as
a subprocess and talks to it over a line-based protocol.

## I/O protocol

- **stdin** — Akagi writes one JSON array of mjai events per line (a batch up
  to the current decision point).
- **stdout** — the bot writes **exactly one** JSON mjai reaction per input
  line (`{"type":"dahai",...}`, or `{"type":"none"}` when not acting). An
  optional `meta` object on the reaction is forwarded verbatim to the HUD.

Because stdout is parsed strictly as one reaction per line, **never print
anything else to stdout** — it desyncs the protocol.

- **stderr** — free-form. Lines are pumped into Akagi's logs, *except*
  notification lines (below).

## Sending notifications to the frontend (`@@AKAGI_NOTIFY@@`)

A bot can push a toast notification to the Akagi UI — it pops in the app's
bottom-right corner. This rides on stderr so it never interferes with the
stdout reaction protocol and can be sent at any time, not just on a reaction.

Write a single stderr line of the form:

```
@@AKAGI_NOTIFY@@ {"level":"warn","title":"...","body":"...","sticky":false,"id":"..."}
```

The JSON object after the `@@AKAGI_NOTIFY@@ ` prefix (note the trailing space)
is the notification:

| Field    | Type   | Required | Meaning                                                            |
| -------- | ------ | -------- | ------------------------------------------------------------------ |
| `level`  | string | yes      | Severity: `"info"`, `"success"`, `"warn"`, or `"error"`.           |
| `title`  | string | yes      | Short headline, shown in bold.                                     |
| `body`   | string | no       | Longer description.                                                |
| `sticky` | bool   | no       | `true` keeps the toast until dismissed (default `false`).          |
| `id`     | string | no       | Stable key; a later toast with the same `id` replaces the earlier. |

Anything on stderr **without** this exact prefix is logged as an ordinary
diagnostic line. A malformed payload after the prefix is logged as a warning
and dropped — it never reaches the UI.

### Helper

The reference bot in [`example/bot.py`](example/bot.py) includes a small
copy-pasteable helper:

```python
import json, sys

NOTIFY_PREFIX = "@@AKAGI_NOTIFY@@ "

def notify(level, title, body=None, *, sticky=False, id=None):
    payload = {"level": level, "title": title}
    if body is not None:
        payload["body"] = body
    if sticky:
        payload["sticky"] = True
    if id is not None:
        payload["id"] = id
    sys.stderr.write(NOTIFY_PREFIX + json.dumps(payload, separators=(",", ":")) + "\n")
    sys.stderr.flush()

# Examples:
notify("info", "Bot ready")
notify("warn", "Low wall", "Fewer than 8 tiles left — playing defensively.")
notify("error", "Model failed to load", "Falling back to rule-based play.")
```

The protocol is just a stderr line, so any language can implement it — the
helper is a convenience, not a requirement.

## Adding a new bot

1. Create `mjai_bot/<name>/` with a `bot.py` implementing the I/O protocol.
2. Add a `pyproject.toml` listing dependencies (Akagi runs `uv sync` into a
   per-bot venv on first launch).
3. Optionally add a `manifest.toml` to expose settings as a form in the UI.
4. Select the bot in Akagi's settings; it is picked up without a restart.
