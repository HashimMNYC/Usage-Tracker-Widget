# usagewidget

A small always-on desktop panel showing your Claude and ChatGPT/Codex usage.
Single Python file, standard library only, no credentials, no network calls.

```bash
python3 usagewidget.py doctor    # what it found on your disk, and where
python3 usagewidget.py window    # frameless desktop widget (recommended)
python3 usagewidget.py serve     # just the server, http://127.0.0.1:8787
python3 usagewidget.py once      # JSON to stdout, for piping into something else
```

Run `doctor` first. It prints every path it checked and every number it managed to
parse, which is the fastest way to find out whether your setup gives you real
percentages or only token estimates.

## Where the numbers come from

Neither provider publishes an API for *subscription* usage percentages. What they do is
cache the server's answer locally after each CLI turn, and that is what this reads.

| provider | source | what you get |
|---|---|---|
| Codex | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` | the real server-reported 5-hour and weekly percentages, with reset timestamps |
| Claude | `~/.claude/projects/**/*.jsonl` | per-turn token counts. The widget also probes for a cached limits file and uses it if your build writes one. |

**This is as fresh as your last CLI turn, not live.** Idle for three hours and you are
looking at three-hour-old figures — the widget prints the staleness age next to each
card so you always know. Anything advertising true real-time subscription usage is
either scraping the web app with your session cookie or guessing.

Honest gaps:

- Anthropic has no subscription-usage endpoint. Tracked at `anthropics/claude-code#44328`.
  Settings → Usage in the Claude apps remains the authoritative number.
- Some Codex builds write `rate_limits: null` into the rollout files
  (`openai/codex#14880`). If that is happening to you, `doctor` will say so; running
  `/status` inside Codex once refreshes the cache.
- If you only use the web apps and never the CLIs, there are no local files and this
  tool has nothing to read. Browser-extension usage badges are the route for that.

## Making it a real desktop widget

`window` mode opens a frameless Chromium app window, which works the same on all three
platforms without installing anything.

**macOS.** To keep it above other windows and on every Space, right-click its Dock icon
→ Options → Assign To → All Desktops. For autostart, System Settings → General →
Login Items → add a small `.command` file containing the `window` line.

**Windows.** Press `Win+R`, run `shell:startup`, and drop in a `.bat`:

```bat
@echo off
pythonw "C:\path\to\usagewidget.py" window
```

`pythonw` rather than `python` so you do not get a console window alongside it.

**Linux.** Add a `.desktop` file to `~/.config/autostart/` with
`Exec=python3 /path/to/usagewidget.py window`. On i3/sway, float and pin it by
`app_id`/class.

**Port.** Set `USAGE_WIDGET_PORT` if 8787 is taken.

## If you want API spend instead

This tool covers subscription usage — the caps you hit while working. Billed API
spend is a different number with proper documented endpoints, both org-tier and
requiring an admin key:

- Anthropic: `GET /v1/organizations/usage_report/messages` and
  `/v1/organizations/cost_report`, with a key starting `sk-ant-admin`.
  https://platform.claude.com/docs/en/manage-claude/usage-cost-api
- OpenAI: the organization usage and costs endpoints, also admin-key gated.

Both are unavailable on individual accounts. Say the word and I can add a panel that
polls them when the keys are present.
