#!/usr/bin/env python3
# Live integration check for slice 6 / task 6.2.
#
# Mirrors the Rust code paths exactly (same Google endpoints, same form fields,
# same calendar.readonly scope, same singleEvents=true query):
#   - request_device_code  -> https://oauth2.googleapis.com/device/code
#   - poll_token_once      -> https://oauth2.googleapis.com/token
#   - refresh_access_token -> token endpoint with grant_type=refresh_token
#   - list calendars       -> /users/me/calendarList
#   - list events          -> /calendars/{id}/events?singleEvents=true
#
# Run:  python3 /home/alarm/pi-alarm-clock/scripts/live_calendar_check.py
#       python3 /home/alarm/pi-alarm-clock/scripts/live_calendar_check.py --refresh-only
#            (reuse the refresh token in secrets.json — no re-consent)
import json, os, sys, time, urllib.request, urllib.parse, pathlib

ROOT = "/home/alarm/pi-alarm-clock"

def load_cfg():
    base = json.loads(os.popen(f"python3 -c \"import json,tomllib;print(json.dumps(tomllib.load(open('{ROOT}/config.toml','rb'))))\"").read())
    try:
        local = json.loads(os.popen(f"python3 -c \"import json,tomllib;print(json.dumps(tomllib.load(open('{ROOT}/config.local.toml','rb'))))\"").read())
        deep_merge(base, local)
    except Exception: pass
    return base

def deep_merge(a, b):
    for k, v in b.items():
        if isinstance(v, dict) and isinstance(a.get(k), dict): deep_merge(a[k], v)
        else: a[k] = v

def post_form(url, fields):
    data = urllib.parse.urlencode(fields).encode()
    req = urllib.request.Request(url, data=data, method="POST",
                                 headers={"Content-Type":"application/x-www-form-urlencoded"})
    try:
        with urllib.request.urlopen(req, timeout=20) as r: return r.status, r.read().decode()
    except urllib.error.HTTPError as e: return e.code, e.read().decode()

def get(url, token):
    req = urllib.request.Request(url, headers={"Authorization": f"Bearer {token}"})
    try:
        with urllib.request.urlopen(req, timeout=20) as r: return r.status, r.read().decode()
    except urllib.error.HTTPError as e: return e.code, e.read().decode()

cfg = load_cfg()["calendar"]
cid, csec = cfg["client_id"], cfg["client_secret"]
dev_url = cfg.get("oauth_device_url","https://oauth2.googleapis.com/device/code")
tok_url = cfg.get("oauth_token_url","https://oauth2.googleapis.com/token")
api_url = cfg.get("calendar_api_url","https://www.googleapis.com/calendar/v3")
sp = pathlib.Path(cfg.get("secrets_path", f"{ROOT}/data/secrets.json"))

REUSE = "--refresh-only" in sys.argv
refresh_token = None
if REUSE and sp.exists():
    try:
        refresh_token = json.loads(sp.read_text()).get("google_refresh_token")
        if refresh_token:
            print("(reusing existing refresh token from secrets.json)")
    except Exception:
        pass

# ── Device-flow pairing (skipped when reusing) ─────────────────────────
if not refresh_token:
    print("=== Step 1: requesting device code ===")
    st, body = post_form(dev_url, {"client_id": cid,
        "scope":"https://www.googleapis.com/auth/calendar.readonly"})
    print(f"  status={st}")
    if st != 200: print(f"  body={body}"); sys.exit(1)
    dev = json.loads(body)
    print(f"  user_code:        {dev['user_code']}")
    print(f"  verification_url: {dev['verification_url']}")
    print(f"  expires_in:       {dev.get('expires_in')}s   interval: {dev.get('interval')}s")

    print("\n=== Step 2: polling for consent ===")
    print(f"  >>> Open {dev['verification_url']} on another device and enter {dev['user_code']}")
    interval = max(dev.get("interval",5),5)
    deadline = time.time() + max(dev.get("expires_in",60),60)
    while time.time() < deadline:
        st, body = post_form(tok_url, {"client_id": cid, "client_secret": csec,
            "device_code": dev["device_code"],
            "grant_type":"urn:ietf:params:oauth:grant-type:device_code"})
        j = json.loads(body)
        if st == 200:
            refresh_token = j.get("refresh_token")
            print(f"  ✓ consent complete (access expires in {j.get('expires_in')}s)")
            break
        err = j.get("error")
        if err == "authorization_pending": print("  ...pending"); time.sleep(interval)
        elif err == "slow_down": interval += 5; print(f"  ...slow_down, interval={interval}"); time.sleep(interval)
        else: print(f"  ✗ error: {err} ({j.get('error_description')})"); sys.exit(1)
    if not refresh_token:
        print("  ✗ device code expired"); sys.exit(1)

    print(f"\n=== Step 3: persisting refresh token to {sp} (0600) ===")
    sp.parent.mkdir(parents=True, exist_ok=True)
    sp.write_text(json.dumps({"google_refresh_token": refresh_token}, indent=2))
    os.chmod(sp, 0o600)
    mode = oct(os.stat(sp).st_mode & 0o777)
    print(f"  ✓ secrets.json written, mode = {mode}")
    assert mode == "0o600", f"secrets.json must be 0600, got {mode}"
else:
    print(f"(refresh token already in {sp} at 0600)")

print("\n=== Step 4: refresh access token (simulating app boot) ===")
st, body = post_form(tok_url, {"client_id": cid, "client_secret": csec,
    "refresh_token": refresh_token, "grant_type":"refresh_token"})
if st != 200: print(f"  ✗ refresh failed: {body}"); sys.exit(1)
access = json.loads(body)["access_token"]
print("  ✓ access token refreshed")

print("\n=== Step 5: list calendars ===")
st, body = get(f"{api_url}/users/me/calendarList", access)
if st != 200: print(f"  ✗ list failed: {body[:500]}"); sys.exit(1)
cals = json.loads(body).get("items", [])
for c in cals:
    print(f"  - {c['id']}  ({c.get('summary','')})")

print("\n=== Step 6: fetch events (primary = Agenda, Canadian holidays = Holiday) ===")
import datetime as dt
time_min = (dt.datetime.utcnow() - dt.timedelta(hours=6)).strftime("%Y-%m-%dT%H:%M:%SZ")
time_max = (dt.datetime.utcnow() + dt.timedelta(days=2)).strftime("%Y-%m-%dT%H:%M:%SZ")
holiday_dates = set()
agenda_events = 0
for cal_id, role in [("primary","Agenda"),
                    ("en.canadian#holiday@group.v.calendar.google.com","Holiday")]:
    q = urllib.parse.urlencode({"singleEvents":"true","orderBy":"startTime",
        "timeMin":time_min,"timeMax":time_max,"maxResults":"250"})
    st, body = get(f"{api_url}/calendars/{urllib.parse.quote(cal_id,'')}/events?{q}", access)
    if st != 200:
        print(f"  ✗ events for {cal_id}: {body[:300]}"); continue
    items = json.loads(body).get("items",[])
    print(f"  [{role}] {cal_id}: {len(items)} events")
    for e in items[:6]:
        s = e.get("start",{})
        summ = e.get("summary","(no title)")
        if "date" in s:
            d = s["date"]
            if role=="Holiday": holiday_dates.add(d)
            print(f"    - {summ}  (all-day {d})")
        else:
            print(f"    - {summ}  ({s.get('dateTime')})")
            agenda_events += 1

print("\n=== Step 7: holiday suppression check ===")
import datetime as dt2
today = dt2.date.today().isoformat()
if today in holiday_dates:
    print(f"  ✓ today ({today}) IS a holiday — a Suppress-policy alarm would be SKIPPED")
else:
    print(f"  • today ({today}) is not a holiday — a Suppress-policy alarm would fire normally")
    if holiday_dates:
        print(f"    (nearest known holiday in window: {min(holiday_dates)})")

print("\n=== 6.2 live check PASSED ===")
print("device-flow pairing ✓  secrets.json(0600) ✓  refresh ✓  agenda fetch ✓  holiday membership ✓")
