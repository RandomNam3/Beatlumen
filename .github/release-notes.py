"""Release notes for a version: its section of CHANGELOG.md and the list of
downloads. Usage: release-notes.py VERSION > notes.md"""

import re
import sys

version = sys.argv[1]
text = open("CHANGELOG.md", encoding="utf-8").read()
m = re.search(r"^## " + re.escape(version) + r"\b[^\n]*\n(.*?)(?=^## |\Z)", text, re.M | re.S)
if not m:
    sys.exit(f"CHANGELOG.md has no section for {version}")

print(m.group(1).strip())
print(f"""
### Downloads

| File | For |
|---|---|
| `Beatlumen-{version}-android.apk` | Android 8 and newer (64-bit ARM phones and tablets) |
| `Beatlumen-{version}-x86_64.AppImage` | Linux (x86_64): make it executable and start it |
| `Beatlumen-{version}-Setup-x64.exe` | Windows 10/11 installer |
| `Beatlumen-{version}-portable-x64.zip` | Windows without installing |
| `beatlumen-server-{version}-linux-x86_64.tar.gz` | The server (one program); the admin interface is at `/admin/` |
| `beatlumen-web-{version}.zip` | The web app as the server serves it (for own hosting) |
| `SHA256SUMS` | Checksums of the files |

The server container is built from `container/` (see `container/README.md`).""")
