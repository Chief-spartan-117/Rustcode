import json, os, subprocess, sys, time, re, signal, collections
ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
host = open(f"{ROOT}/crates/infrastructure/src/extension_host.js").read().replace(
    "// @@SERVICES@@", open(f"{ROOT}/crates/infrastructure/src/extension_services.js").read()).replace(
    "// @@COMPAT@@", open(f"{ROOT}/crates/infrastructure/src/extension_compat.js").read())
open("host.js", "w").write(host)
workspace = os.environ.get("SURVEY_WORKSPACE", os.getcwd())
names = json.load(open("list.json"))
only = sys.argv[1:] 
IMPLIED = {"string": "", "boolean": False, "number": 0, "integer": 0, "array": [], "object": {}}
def declared_defaults(pkg):
    """The default of each setting an extension declares; a type without a default reads as its empty value, as in VS Code."""
    defaults = {}
    cfg = pkg.get("contributes", {}).get("configuration", {})
    for section in (cfg if isinstance(cfg, list) else [cfg]):
        for key, prop in (section.get("properties") or {}).items():
            if not isinstance(prop, dict): continue
            if "default" in prop: defaults[key] = prop["default"]
            else:
                kind = prop.get("type")
                kind = kind[0] if isinstance(kind, list) and kind else kind
                if kind in IMPLIED: defaults[key] = IMPLIED[kind]
    return defaults

def run_group(ident, infos, defaults):
    p = subprocess.Popen(["node", "host.js"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
    init = {"type": "init", "extensions": infos, "folders": [workspace], "configuration": dict(defaults), "defaults": defaults}
    p.stdin.write(json.dumps(init) + "\n"); p.stdin.flush()
    time.sleep(8)
    try: os.killpg(p.pid, signal.SIGTERM)
    except Exception: pass
    try: out, err = p.communicate(timeout=5)
    except Exception:
        os.killpg(p.pid, signal.SIGKILL); out, err = p.communicate()
    errors = []
    for line in out.splitlines():
        try: m = json.loads(line)
        except Exception: continue
        if not isinstance(m, dict): continue
        if m.get("type") == "message" and m.get("level") == "error": errors.append(m["text"])
        elif m.get("type") == "log" and m["text"].startswith("Unhandled rejection"): errors.append(m["text"])
    print("ERR" if errors else "ok ", ident, (errors[0].splitlines()[0][:150] if errors else ""), flush=True)
    results[ident] = {"status": "ran", "errors": errors[:6]}

from concurrent.futures import ThreadPoolExecutor
results = {}
def run_one(group):
    idents = group.split("+")
    ident = group
    infos = []
    defaults = {}
    for one in idents:
        d = f"ext/{one}"
        pkg = json.load(open(f"{d}/package.json"))
        infos.append({"id": one, "path": os.path.abspath(d), "main": pkg.get("main"), "activationEvents": ["*"],
            "commands": [c["command"] for c in pkg.get("contributes", {}).get("commands", [])],
            "views": [v["id"] for vs in pkg.get("contributes", {}).get("views", {}).values() for v in vs],
            "taskTypes": [t["type"] for t in pkg.get("contributes", {}).get("taskDefinitions", [])]})
        defaults.update(declared_defaults(pkg))
    if "+" in group:
        return run_group(group, infos, defaults)
    if only and ident not in only: return
    d = f"ext/{ident}"
    try:
        pkg = json.load(open(f"{d}/package.json"))
    except Exception as e:
        results[ident] = {"status": "no manifest"}; return
    main = pkg.get("main")
    if not main:
        results[ident] = {"status": "no code"}; return
    info = {"id": ident, "path": os.path.abspath(d), "main": main, "activationEvents": ["*"],
            "commands": [c["command"] for c in pkg.get("contributes", {}).get("commands", [])],
            "views": [v["id"] for vs in pkg.get("contributes", {}).get("views", {}).values() for v in vs],
            "taskTypes": [t["type"] for t in pkg.get("contributes", {}).get("taskDefinitions", [])]}
    p = subprocess.Popen(["node", "host.js"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                         text=True, start_new_session=True)
    defaults = declared_defaults(pkg)
    init = {"type": "init", "extensions": [info], "folders": [workspace], "configuration": dict(defaults), "defaults": defaults}
    p.stdin.write(json.dumps(init) + "\n"); p.stdin.flush()
    time.sleep(7)
    # a command, to see whether it ran: ask for the first one
    out, err = "", ""
    try:
        os.killpg(p.pid, signal.SIGTERM)
    except Exception: pass
    try:
        out, err = p.communicate(timeout=5)
    except Exception:
        os.killpg(p.pid, signal.SIGKILL)
        out, err = p.communicate()
    errors, kinds, logs = [], collections.Counter(), []
    for line in out.splitlines():
        try: m = json.loads(line)
        except Exception: continue
        if not isinstance(m, dict): continue
        if m.get("type") == "message" and m.get("level") == "error":
            errors.append(m["text"])
        elif m.get("type") == "log":
            logs.append(m["text"])
            if m["text"].startswith("Unhandled rejection"): errors.append(m["text"])
        else:
            kinds[m.get("type")] += 1
    results[ident] = {"status": "ran", "errors": errors[:6], "stderr": err.strip()[:400], "events": dict(kinds), "main": main}
    flag = "ERR" if errors or err.strip() else "ok "
    print(flag, ident, (errors[0].splitlines()[0][:150] if errors else err.strip()[:100].replace("\n", " ")), flush=True)


with ThreadPoolExecutor(10) as pool:
    list(pool.map(run_one, names))
json.dump(results, open("results.json", "w"), indent=1)
