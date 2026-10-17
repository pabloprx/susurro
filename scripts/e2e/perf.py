#!/usr/bin/env python3
"""Perf suite against a running harness app (run-app.sh). Measures, prints a JSON report.
  E2E_PORT=47930 python3 scripts/e2e/perf.py <pid> [models...] [--leak N] [--idle S]
E2E latency = POST /event/hold_end -> /state shows a new last_history row (5 ms polling)."""
import json, os, subprocess, sys, time, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
FIX = os.path.join(HERE, "fixtures")
PORT = os.environ.get("E2E_PORT", "47930")
BASE = f"http://127.0.0.1:{PORT}"
LOG = os.path.join(os.environ.get("E2E_DIR", "/tmp/susurro-e2e-perf"), "logs", "susurro.log")
# fixture -> hold seconds (speech length + 0.3 s; long.wav speech ends at 46 s)
HOLD = {"en_snippet.wav": 2.35, "en_custom.wav": 7.3, "long.wav": 46.5, "es_accents.wav": 5.25}


def req(method, path, body=None):
    data = body.encode() if isinstance(body, str) else (json.dumps(body).encode() if body is not None else None)
    r = urllib.request.Request(BASE + path, data=data, method=method)
    with urllib.request.urlopen(r, timeout=30) as f:
        return json.loads(f.read())


def last_id():
    h = req("GET", "/state")["last_history"]
    return h["id"] if h else 0


def wait_idle(timeout=10):
    end = time.time() + timeout
    while time.time() < end:
        if req("GET", "/state")["session"]["phase"] == "idle":
            return
        time.sleep(0.05)


def dictate(fixture):
    req("POST", "/audio", os.path.join(FIX, fixture))
    wait_idle()
    prev = last_id()
    req("POST", "/event/hold_start")
    time.sleep(HOLD[fixture])
    t0 = time.perf_counter()
    req("POST", "/event/hold_end")
    while True:
        h = req("GET", "/state")["last_history"]
        if h and h["id"] != prev:
            t1 = time.perf_counter()
            break
        if time.perf_counter() - t0 > 60:
            return {"fixture": fixture, "error": "no history row in 60 s"}
        time.sleep(0.005)
    return {"fixture": fixture, "e2e_ms": round((t1 - t0) * 1000), "latency_ms": h["latency_ms"], "status": h["status"],
            "duration_ms": h["duration_ms"], "provider": h["provider"], "text": h["final_text"][:80]}


def rss_kb(pid):
    return int(subprocess.check_output(["ps", "-o", "rss=", "-p", str(pid)]).strip())


def cpu_s(pid):
    t = subprocess.check_output(["ps", "-o", "time=", "-p", str(pid)]).decode().strip()
    m, s = t.split(":")
    return int(m) * 60 + float(s)


def idle_cpu(pid, secs):
    c0, t0 = cpu_s(pid), time.time()
    time.sleep(secs)
    return round((cpu_s(pid) - c0) / (time.time() - t0) * 100, 3)


def set_model(model, lang):
    n = open(LOG).read().count(f"loaded {model}")
    req("POST", "/settings", {"stt": {"provider": "whisper_local", "whisper_model": model}, "language": lang})
    end = time.time() + 60
    while time.time() < end and open(LOG).read().count(f"loaded {model}") == n:
        time.sleep(0.1)


def main():
    args = sys.argv[1:]
    pid = int(args.pop(0))
    leak = int(args[args.index("--leak") + 1]) if "--leak" in args else 30
    idle = float(args[args.index("--idle") + 1]) if "--idle" in args else 60
    models = [a for a in args if a.endswith(".bin")] or ["ggml-base.en-q5_1.bin"]
    out = {"pid": pid, "models": {}}
    time.sleep(5)
    out["idle_rss_kb_start"] = rss_kb(pid)
    out["idle_cpu_pct"] = idle_cpu(pid, idle)
    for m in models:
        set_model(m, "en")
        runs = [dictate(f) for f in ("en_snippet.wav", "en_snippet.wav", "en_custom.wav", "long.wav")]
        if "base.en" not in m:
            set_model(m, "es")
            runs.append(dictate("es_accents.wav"))
            set_model(m, "en")
        out["models"][m] = {"runs": runs, "rss_kb_after": rss_kb(pid)}
        print(json.dumps({m: out["models"][m]}), flush=True)
    set_model(models[0], "en")
    rss = [rss_kb(pid)]
    lat = []
    for i in range(leak):
        r = dictate("en_snippet.wav")
        lat.append(r.get("e2e_ms"))
        if (i + 1) % 10 == 0:
            rss.append(rss_kb(pid))
    out["leak"] = {"model": models[0], "n": leak, "rss_kb_every_10": rss, "e2e_ms": lat}
    time.sleep(5)
    out["idle_rss_kb_end"] = rss_kb(pid)
    out["idle_cpu_pct_end"] = idle_cpu(pid, idle)
    print(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()
