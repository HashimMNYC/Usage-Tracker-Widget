#!/usr/bin/env python3
"""
usagewidget.py — a small desktop panel showing Claude and Codex usage.

    python3 usagewidget.py doctor     # what did it find, and where
    python3 usagewidget.py serve      # http://127.0.0.1:8787
    python3 usagewidget.py window     # serve + open as a frameless desktop widget
    python3 usagewidget.py once       # print JSON and exit

No dependencies. No network calls. No credentials. It reads files the two CLIs
already write to your disk and renders them.

WHERE THE NUMBERS COME FROM

Neither Anthropic nor OpenAI publishes an API for subscription usage percentages.
What they do is cache the server's answer locally after each CLI turn:

  Codex   ~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl
          token_count events carry a rate_limits object with the real
          server-reported percentages and reset times for both windows.

  Claude  ~/.claude/projects/**/*.jsonl
          per-turn token counts. Claude Code does not reliably persist the
          server's percentage figures, so the weekly number here is a local
          token estimate unless a limits cache is found (we probe for one).

CONSEQUENCE, STATED PLAINLY: this is as fresh as your last CLI turn, not live.
Idle for three hours and it shows three-hour-old figures with a staleness age
next to them. Anything claiming true real-time for subscription usage is either
scraping the web app with your session cookie or making it up.
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
import webbrowser
import subprocess
import shutil
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

PORT = int(os.environ.get("USAGE_WIDGET_PORT", "8787"))
CACHE_TTL = 20  # seconds; scanning JSONL is cheap but not free

# --------------------------------------------------------------------------- paths


def codex_home() -> list[Path]:
    raw = os.environ.get("CODEX_HOME")
    if raw:
        return [Path(p).expanduser() for p in raw.split(",") if p.strip()]
    return [Path.home() / ".codex"]


def claude_home() -> list[Path]:
    raw = os.environ.get("CLAUDE_CONFIG_DIR")
    if raw:
        return [Path(p).expanduser() for p in raw.split(",") if p.strip()]
    return [Path.home() / ".claude", Path.home() / ".config" / "claude"]


def recent_jsonl(roots: list[Path], subdirs: list[str], days: int = 9) -> list[Path]:
    """Files touched in the last `days`, newest first. Cheap mtime filter."""
    cutoff = time.time() - days * 86400
    found: list[tuple[float, Path]] = []
    for root in roots:
        for sub in subdirs:
            base = root / sub if sub else root
            if not base.is_dir():
                continue
            for p in base.rglob("*.jsonl"):
                try:
                    st = p.stat()
                except OSError:
                    continue
                if st.st_mtime >= cutoff:
                    found.append((st.st_mtime, p))
    found.sort(reverse=True)
    return [p for _, p in found]


def read_lines_reverse(path: Path, limit: int = 4000):
    """Yield parsed JSON objects from the end of a JSONL file."""
    try:
        with path.open("r", encoding="utf-8", errors="replace") as fh:
            lines = fh.readlines()
    except OSError:
        return
    for line in reversed(lines[-limit:]):
        line = line.strip()
        if not line or not line.startswith("{"):
            continue
        try:
            yield json.loads(line)
        except json.JSONDecodeError:
            continue


# ------------------------------------------------------------------ rate limit sniff

PCT_KEYS = ("used_percent", "usedPercent", "percent_used", "percentUsed")
RESET_AT_KEYS = ("resets_at", "resetsAt", "reset_at", "resetAt")
RESET_IN_KEYS = ("resets_in_seconds", "resetsInSeconds", "reset_in_seconds", "seconds_to_reset")
WINDOW_KEYS = ("window_minutes", "windowMinutes", "window_duration_mins", "windowDurationMins")


def _as_epoch(value) -> float | None:
    """Accept unix seconds, unix millis, or an ISO-8601 string."""
    if value is None:
        return None
    if isinstance(value, (int, float)):
        v = float(value)
        return v / 1000.0 if v > 1e11 else v
    if isinstance(value, str):
        s = value.strip().replace("Z", "+00:00")
        try:
            dt = datetime.fromisoformat(s)
        except ValueError:
            return None
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        return dt.timestamp()
    return None


def looks_like_window(d: dict) -> bool:
    return isinstance(d, dict) and any(k in d for k in PCT_KEYS)


def parse_window(d: dict, label: str) -> dict | None:
    pct = next((d[k] for k in PCT_KEYS if k in d and d[k] is not None), None)
    if pct is None:
        return None
    try:
        pct = float(pct)
    except (TypeError, ValueError):
        return None
    resets_at = next((_as_epoch(d[k]) for k in RESET_AT_KEYS if k in d), None)
    if resets_at is None:
        secs = next((d[k] for k in RESET_IN_KEYS if k in d and d[k] is not None), None)
        if secs is not None:
            try:
                resets_at = time.time() + float(secs)
            except (TypeError, ValueError):
                resets_at = None
    minutes = next((d[k] for k in WINDOW_KEYS if k in d and d[k] is not None), None)
    try:
        minutes = int(minutes) if minutes is not None else None
    except (TypeError, ValueError):
        minutes = None
    return {"label": label, "used_percent": pct, "resets_at": resets_at, "window_minutes": minutes}


def find_windows(obj, path: str = "") -> list[dict]:
    """Walk any structure and pull out every rate-limit-shaped dict.

    Deliberately schema-agnostic: these payloads have changed shape several times
    across CLI versions, and a recursive sniff survives that better than a fixed
    path into the JSON.
    """
    out: list[dict] = []
    if isinstance(obj, dict):
        if looks_like_window(obj):
            w = parse_window(obj, path.rsplit(".", 1)[-1] or "window")
            if w:
                out.append(w)
        for k, v in obj.items():
            out.extend(find_windows(v, f"{path}.{k}" if path else k))
    elif isinstance(obj, list):
        for i, v in enumerate(obj):
            out.extend(find_windows(v, f"{path}[{i}]"))
    return out


def classify(windows: list[dict]) -> dict:
    """Sort windows into the 5-hour session bucket and the weekly bucket."""
    session, weekly = None, None
    for w in windows:
        m = w.get("window_minutes")
        name = (w.get("label") or "").lower()
        is_weekly = (m is not None and m >= 24 * 60) or "second" in name or "week" in name
        is_session = (m is not None and m < 24 * 60) or "primary" in name or "session" in name
        if is_weekly and weekly is None:
            weekly = w
        elif is_session and session is None:
            session = w
    # Fall back to ordering when the payload gave us nothing to classify on.
    leftovers = [w for w in windows if w is not session and w is not weekly]
    if session is None and leftovers:
        session = leftovers.pop(0)
    if weekly is None and leftovers:
        weekly = leftovers.pop(0)
    return {"session": session, "weekly": weekly}


# ------------------------------------------------------------------------- codex

def scan_codex() -> dict:
    roots = codex_home()
    files = recent_jsonl(roots, ["sessions", "archived_sessions"])
    result = {
        "provider": "codex",
        "label": "ChatGPT / Codex",
        "found": bool(files),
        "roots": [str(r) for r in roots],
        "files_scanned": 0,
        "session": None,
        "weekly": None,
        "observed_at": None,
        "source": None,
        "tokens_week": 0,
        "notes": [],
    }
    if not files:
        result["notes"].append("No rollout files under ~/.codex/sessions — run Codex CLI once.")
        return result

    week_start = time.time() - 7 * 86400
    tokens = 0
    seen_totals: set[int] = set()

    for path in files[:40]:
        result["files_scanned"] += 1
        for obj in read_lines_reverse(path):
            payload = obj.get("payload") if isinstance(obj, dict) else None
            blob = payload if isinstance(payload, dict) else obj

            # Server-reported percentages, newest wins.
            if result["session"] is None and result["weekly"] is None:
                windows = find_windows(blob.get("rate_limits")) if isinstance(blob, dict) else []
                if not windows and isinstance(blob, dict) and "rate_limits" not in blob:
                    windows = find_windows(blob)
                if windows:
                    got = classify(windows)
                    if got["session"] or got["weekly"]:
                        result.update(got)
                        result["source"] = str(path)
                        ts = _as_epoch(obj.get("timestamp") or blob.get("timestamp")) if isinstance(blob, dict) else None
                        result["observed_at"] = ts or path.stat().st_mtime

            # Rough weekly token total from cumulative counters.
            if isinstance(blob, dict) and blob.get("type") == "token_count":
                info = blob.get("info") or {}
                total = (info.get("total_token_usage") or {}).get("total_tokens")
                if isinstance(total, int) and total not in seen_totals:
                    seen_totals.add(total)
        if path.stat().st_mtime >= week_start:
            tokens += max(seen_totals) if seen_totals else 0
            seen_totals.clear()
        else:
            seen_totals.clear()

    result["tokens_week"] = tokens
    if result["session"] is None and result["weekly"] is None:
        result["notes"].append(
            "Found sessions but no rate_limits payload. Some Codex builds write null here "
            "(openai/codex#14880). Run /status in Codex once to refresh it."
        )
    return result


# ------------------------------------------------------------------------ claude

CLAUDE_LIMIT_CANDIDATES = [
    "stats-cache.json", "usage.json", "limits.json", "rate-limits.json",
    "subscription.json", "account.json",
]


def scan_claude() -> dict:
    roots = claude_home()
    result = {
        "provider": "claude",
        "label": "Claude",
        "found": False,
        "roots": [str(r) for r in roots],
        "files_scanned": 0,
        "session": None,
        "weekly": None,
        "observed_at": None,
        "source": None,
        "tokens_week": 0,
        "tokens_today": 0,
        "notes": [],
    }

    # 1. Probe for any cached server-reported limits. Undocumented and version
    #    dependent, so we sniff rather than assume a path.
    for root in roots:
        if not root.is_dir():
            continue
        for name in CLAUDE_LIMIT_CANDIDATES:
            p = root / name
            if not p.is_file():
                continue
            try:
                data = json.loads(p.read_text(encoding="utf-8", errors="replace"))
            except (OSError, json.JSONDecodeError):
                continue
            windows = find_windows(data)
            if windows:
                got = classify(windows)
                if got["session"] or got["weekly"]:
                    result.update(got)
                    result["source"] = str(p)
                    result["observed_at"] = p.stat().st_mtime
                    break

    # 2. Token totals from the transcripts. Always useful, always present.
    files = recent_jsonl(roots, ["projects", ""])
    result["found"] = bool(files) or result["source"] is not None
    week_start = time.time() - 7 * 86400
    day_start = datetime.now().replace(hour=0, minute=0, second=0, microsecond=0).timestamp()
    week_tokens = 0
    day_tokens = 0
    newest = 0.0

    for path in files[:400]:
        result["files_scanned"] += 1
        for obj in read_lines_reverse(path, limit=2000):
            usage = None
            if isinstance(obj, dict):
                msg = obj.get("message")
                if isinstance(msg, dict):
                    usage = msg.get("usage")
                if usage is None and isinstance(obj.get("usage"), dict):
                    usage = obj["usage"]
            if not isinstance(usage, dict):
                continue
            ts = _as_epoch(obj.get("timestamp")) or path.stat().st_mtime
            newest = max(newest, ts)
            n = 0
            for k in ("input_tokens", "output_tokens",
                      "cache_creation_input_tokens", "cache_read_input_tokens"):
                v = usage.get(k)
                if isinstance(v, (int, float)):
                    n += int(v)
            if ts >= week_start:
                week_tokens += n
            if ts >= day_start:
                day_tokens += n

    result["tokens_week"] = week_tokens
    result["tokens_today"] = day_tokens
    if result["observed_at"] is None and newest:
        result["observed_at"] = newest

    if not files and result["source"] is None:
        result["notes"].append("No transcripts under ~/.claude/projects — run Claude Code once.")
    elif result["session"] is None and result["weekly"] is None:
        result["notes"].append(
            "No cached server percentages found. Anthropic doesn't publish a subscription "
            "usage API (claude-code#44328), so the weekly figure below is a local token "
            "estimate. Settings > Usage in the app is the authoritative number."
        )
    return result


# ------------------------------------------------------------------------ assemble

_cache: dict = {"at": 0.0, "data": None}


def collect(force: bool = False) -> dict:
    if not force and _cache["data"] and time.time() - _cache["at"] < CACHE_TTL:
        return _cache["data"]
    data = {
        "generated_at": time.time(),
        "providers": [scan_claude(), scan_codex()],
    }
    _cache["at"] = time.time()
    _cache["data"] = data
    return data


# ---------------------------------------------------------------------------- ui

HTML = r"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>usage</title>
<style>
:root{--bg:#14131a;--panel:#1c1b25;--line:#2c2a38;--dim:#7a7689;--text:#e9e6f0;
--claude:#d0764a;--codex:#5fb08c;--warn:#d9a13a;--bad:#d2544c;}
*{box-sizing:border-box;margin:0;padding:0}
body{background:var(--bg);color:var(--text);font:13px/1.4 ui-sans-serif,-apple-system,"Segoe UI",system-ui,sans-serif;
padding:12px;-webkit-user-select:none;user-select:none;cursor:default;overflow:hidden}
body.drag{-webkit-app-region:drag}
.row{display:flex;align-items:baseline;justify-content:space-between;margin-bottom:7px}
h1{font-size:11px;letter-spacing:.14em;text-transform:uppercase;color:var(--dim);font-weight:600}
.card{background:var(--panel);border:1px solid var(--line);border-radius:9px;padding:11px 12px;margin-bottom:9px}
.hdr{display:flex;align-items:center;gap:7px;margin-bottom:9px}
.dot{width:7px;height:7px;border-radius:50%;flex:none}
.name{font-weight:600;font-size:13px}
.age{margin-left:auto;font-size:10px;color:var(--dim);font-variant-numeric:tabular-nums}
.meter{margin-bottom:8px}
.meter:last-child{margin-bottom:0}
.mtop{display:flex;justify-content:space-between;font-size:10.5px;color:var(--dim);margin-bottom:3px}
.mtop b{color:var(--text);font-weight:600;font-variant-numeric:tabular-nums}
.track{height:6px;background:#000;border-radius:3px;overflow:hidden}
.fill{height:100%;border-radius:3px;transition:width .5s ease}
.est{color:var(--warn)}
.note{font-size:10.5px;color:var(--dim);margin-top:8px;line-height:1.45}
.empty{font-size:11.5px;color:var(--dim);padding:3px 0}
kbd{background:#000;border:1px solid var(--line);border-radius:3px;padding:0 4px;font:inherit;font-size:10px}
footer{display:flex;justify-content:space-between;font-size:10px;color:var(--dim);padding-top:2px}
a{color:var(--dim)}
</style></head><body>
<div class="row"><h1>Usage</h1><span class="age" id="clock"></span></div>
<div id="cards"></div>
<footer><span id="stamp">—</span><span>refresh <kbd>R</kbd></span></footer>
<script>
const fmt = n => n >= 1e9 ? (n/1e9).toFixed(2)+'B' : n >= 1e6 ? (n/1e6).toFixed(1)+'M'
  : n >= 1e3 ? (n/1e3).toFixed(1)+'k' : String(n||0);

function ago(ts){
  if(!ts) return '';
  const s = Math.max(0, Date.now()/1000 - ts);
  if(s < 90) return Math.round(s)+'s ago';
  if(s < 5400) return Math.round(s/60)+'m ago';
  if(s < 172800) return Math.round(s/3600)+'h ago';
  return Math.round(s/86400)+'d ago';
}
function until(ts){
  if(!ts) return '';
  const s = ts - Date.now()/1000;
  if(s <= 0) return 'resetting';
  if(s < 5400) return 'resets in '+Math.round(s/60)+'m';
  if(s < 172800) return 'resets in '+Math.round(s/3600)+'h';
  return 'resets in '+Math.round(s/86400)+'d';
}
function colour(pct, base){ return pct >= 90 ? 'var(--bad)' : pct >= 70 ? 'var(--warn)' : base; }

function meter(title, w, base){
  if(!w) return '';
  const pct = Math.max(0, Math.min(100, w.used_percent));
  return `<div class="meter">
    <div class="mtop"><span>${title}</span><span><b>${pct.toFixed(0)}%</b> &nbsp;${until(w.resets_at)}</span></div>
    <div class="track"><div class="fill" style="width:${pct}%;background:${colour(pct, base)}"></div></div>
  </div>`;
}

function estimate(title, tokens, base){
  return `<div class="meter">
    <div class="mtop"><span>${title}</span><span class="est"><b>${fmt(tokens)}</b> tokens · estimate</span></div>
    <div class="track"><div class="fill" style="width:100%;background:linear-gradient(90deg,${base},transparent)"></div></div>
  </div>`;
}

function card(p){
  const base = p.provider === 'claude' ? 'var(--claude)' : 'var(--codex)';
  let body = '';
  if(!p.found){
    body = `<div class="empty">Nothing on disk yet.</div>`;
  } else if(p.session || p.weekly){
    body = meter('Session', p.session, base) + meter('Week', p.weekly, base);
  } else {
    body = estimate('This week', p.tokens_week, base);
  }
  const notes = (p.notes||[]).map(n=>`<div class="note">${n}</div>`).join('');
  return `<div class="card">
    <div class="hdr"><span class="dot" style="background:${base}"></span>
      <span class="name">${p.label}</span>
      <span class="age">${ago(p.observed_at)}</span></div>
    ${body}${notes}
  </div>`;
}

async function tick(){
  try{
    const r = await fetch('/data', {cache:'no-store'});
    const d = await r.json();
    document.getElementById('cards').innerHTML = d.providers.map(card).join('');
    document.getElementById('stamp').textContent = 'read ' + ago(d.generated_at);
  }catch(e){
    document.getElementById('stamp').textContent = 'server not responding';
  }
}
function clock(){
  document.getElementById('clock').textContent =
    new Date().toLocaleTimeString([], {hour:'2-digit', minute:'2-digit'});
}
addEventListener('keydown', e => { if(e.key.toLowerCase() === 'r') tick(); });
tick(); clock();
setInterval(tick, 20000);
setInterval(clock, 30000);
</script></body></html>
"""


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802
        if self.path.startswith("/data"):
            payload = json.dumps(collect()).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Cache-Control", "no-store")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        body = HTML.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass  # quiet


def serve(open_browser: bool = False, app_window: bool = False) -> None:
    httpd = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    url = f"http://127.0.0.1:{PORT}/"
    print(f"usage widget on {url}   (ctrl-c to stop)")
    if app_window:
        launch_window(url)
    elif open_browser:
        webbrowser.open(url)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("\nstopped")


def launch_window(url: str) -> None:
    """Open as a frameless app window. Chromium's --app is the one trick that
    works identically on all three platforms without installing anything."""
    size = "--window-size=390,300"
    flags = ["--app=" + url, size, "--disable-features=Translate", "--no-first-run"]
    candidates = [
        "google-chrome", "google-chrome-stable", "chromium", "chromium-browser",
        "microsoft-edge", "brave-browser",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    ]
    for c in candidates:
        exe = shutil.which(c) or (c if Path(c).exists() else None)
        if exe:
            try:
                subprocess.Popen([exe, *flags],
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                return
            except OSError:
                continue
    print("  no chromium-family browser found; opening a normal tab instead")
    webbrowser.open(url)


def doctor() -> None:
    print("\nusagewidget doctor\n" + "-" * 58)
    for root_fn, name, subs in (
        (claude_home, "Claude", ["projects"]),
        (codex_home, "Codex", ["sessions", "archived_sessions"]),
    ):
        print(f"\n{name}")
        for root in root_fn():
            mark = "found" if root.is_dir() else "missing"
            print(f"  {mark:>8}  {root}")
            if not root.is_dir():
                continue
            for sub in subs:
                d = root / sub
                if d.is_dir():
                    n = sum(1 for _ in d.rglob("*.jsonl"))
                    print(f"            {sub}/  {n} jsonl files")
            for f in sorted(root.glob("*.json"))[:12]:
                print(f"            {f.name}  ({f.stat().st_size} bytes)")

    print("\nparsed\n" + "-" * 58)
    data = collect(force=True)
    for p in data["providers"]:
        print(f"\n{p['label']}")
        print(f"  files scanned : {p['files_scanned']}")
        print(f"  limits source : {p.get('source') or 'none'}")
        for key in ("session", "weekly"):
            w = p.get(key)
            if w:
                rs = (datetime.fromtimestamp(w["resets_at"]).strftime("%Y-%m-%d %H:%M")
                      if w.get("resets_at") else "unknown")
                print(f"  {key:<13} : {w['used_percent']:.1f}%  window={w.get('window_minutes')}m  resets {rs}")
            else:
                print(f"  {key:<13} : not reported on disk")
        print(f"  tokens/week   : {p.get('tokens_week', 0):,}")
        for n in p.get("notes", []):
            print(f"  note          : {n}")
    print()


def main() -> None:
    cmd = sys.argv[1] if len(sys.argv) > 1 else "window"
    if cmd == "doctor":
        doctor()
    elif cmd == "once":
        print(json.dumps(collect(force=True), indent=2))
    elif cmd == "serve":
        serve()
    elif cmd == "window":
        serve(app_window=True)
    elif cmd == "open":
        serve(open_browser=True)
    else:
        print(__doc__)


if __name__ == "__main__":
    main()
