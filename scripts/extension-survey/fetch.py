import json, urllib.request, sys, os, zipfile, io, time
API = "https://open-vsx.org/api"
def get(url, timeout=30):
    req = urllib.request.Request(url, headers={"User-Agent": "rustcode-survey"})
    return urllib.request.urlopen(req, timeout=timeout).read()
listing = []
offset = 0
while len(listing) < int(sys.argv[1]):
    data = json.loads(get(f"{API}/-/search?sortBy=downloadCount&sortOrder=desc&size=50&offset={offset}"))
    if not data["extensions"]: break
    listing += data["extensions"]; offset += 50
os.makedirs("ext", exist_ok=True)
done = []
for item in listing[: int(sys.argv[1])]:
    ns, name = item["namespace"], item["name"]
    ident = f"{ns}.{name}"
    target = f"ext/{ident}"
    if os.path.isdir(target):
        done.append(ident); continue
    try:
        meta = json.loads(get(f"{API}/{ns}/{name}"))
        downloads = meta.get("downloads", {})
        url = downloads.get("universal") or downloads.get("darwin-arm64") or downloads.get("darwin-x64")
        if not url:
            print("skip (no suitable build)", ident, list(downloads)); continue
        blob = get(url, timeout=120)
        with zipfile.ZipFile(io.BytesIO(blob)) as z:
            z.extractall(f"{target}.tmp")
        os.rename(f"{target}.tmp/extension", target) if os.path.isdir(f"{target}.tmp/extension") else os.rename(f"{target}.tmp", target)
        done.append(ident)
        print("ok", ident, item.get("downloadCount"))
    except Exception as e:
        print("fail", ident, e)
json.dump(done, open("list.json", "w"))
