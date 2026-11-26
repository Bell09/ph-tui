#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
python3 - "$ROOT" <<'PY'
import base64, gzip, pathlib, sys
root = pathlib.Path(sys.argv[1])
b64 = (root / "scripts/http.rs.gz.b64").read_text()
raw = gzip.decompress(base64.b64decode(b64))
text = raw.decode()
old = "    let html = get_html(&page_url)?;"
new = """    // Prefer a single page fetch for og:image; one retry covers brief CDN blips.
    let html = get_html_once(&page_url, false).or_else(|_| get_html_once(&page_url, true))?;"""
if old not in text:
    raise SystemExit("expected get_html call not found in restored http.rs")
text = text.replace(old, new, 1)
(root / "src/http.rs").write_text(text)
print(f"restored src/http.rs ({len(text)} bytes)")
PY
