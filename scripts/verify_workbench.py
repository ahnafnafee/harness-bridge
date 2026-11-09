"""Exercise the local UI API and optional native Qoder discovery in isolated homes.

No credentials or model requests are used. The optional Qoder check verifies
session discovery, not authenticated generation or continuation.
"""

import argparse
import gzip
import json
import os
from pathlib import Path
import queue
import re
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request


def verify(bridge, qoder=None, expected_assets=None):
    with tempfile.TemporaryDirectory(prefix="bridge-workbench-") as temporary:
        root = Path(temporary)
        project = root / "project"
        project.mkdir()
        homes = {"dsh": root / "dsh", "codex": root / "codex", "claude": root / "claude",
                 "zcode_db": root / "zcode/db.sqlite", "agy": root / "agy",
                 "qoder": root / "qoder", "qoder_ide_db": root / "ide/main.sqlite"}
        source = homes["claude"] / "projects/demo/11111111-2222-4333-8444-555555555555.jsonl"
        source.parent.mkdir(parents=True)
        rows = [{"type": "user", "uuid": "prompt", "parentUuid": None,
                 "sessionId": source.stem, "timestamp": "2026-11-08T10:00:00Z", "cwd": str(project),
                 "message": {"role": "user", "content": "previewed conversation"}},
                {"type": "assistant", "uuid": "reply", "parentUuid": "prompt",
                 "sessionId": source.stem, "timestamp": "2026-11-08T10:01:00Z", "cwd": str(project),
                 "message": {"role": "assistant", "content": [{"type": "text", "text": "saved answer"}]}}]

        def save_source():
            source.write_text("\n".join(json.dumps(row) for row in rows) + "\n", encoding="utf-8")

        save_source()
        command = [str(bridge), "ui", "--port", "0"]
        flags = {"dsh": "--dsh-home", "codex": "--codex-home", "claude": "--claude-home",
                 "zcode_db": "--zcode-db", "agy": "--agy-dir", "qoder": "--qoder-home",
                 "qoder_ide_db": "--qoder-ide-db"}
        for name, path in homes.items():
            command.extend([flags[name], str(path)])
        creation = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                   text=True, encoding="utf-8", errors="replace", creationflags=creation)
        lines = queue.Queue()

        def read_lines():
            for line in process.stdout:
                lines.put(line)

        threading.Thread(target=read_lines, daemon=True).start()
        try:
            deadline = time.monotonic() + 20
            startup = []
            match = None
            while time.monotonic() < deadline:
                try:
                    line = lines.get(timeout=0.1)
                except queue.Empty:
                    if process.poll() is not None:
                        break
                    continue
                startup.append(line)
                match = re.search(r"(http://127\.0\.0\.1:\d+)/#key=([0-9a-f]+)", line)
                if match:
                    break
            assert match, "local interface did not start: " + "".join(startup)
            base, key = match.groups()

            def api(path, body=None, expected=200, headers=None, authenticated=True):
                request_headers = {"X-Harness-Bridge-Key": key} if authenticated else {}
                request_headers.update(headers or {})
                data = None
                if body is not None:
                    data = json.dumps(body).encode()
                    request_headers["Content-Type"] = "application/json"
                request = urllib.request.Request(base + path, data=data, headers=request_headers)
                try:
                    response = urllib.request.urlopen(request, timeout=30)
                except urllib.error.HTTPError as error:
                    response = error
                with response:
                    status, raw = response.code, response.read()
                assert status == expected, (path, status, raw[:500])
                return json.loads(raw)

            api("/api/catalog", expected=401, authenticated=False)
            api("/api/catalog", expected=403, headers={"Origin": "https://other.example"})
            api("/api/catalog", expected=403, headers={"Host": "other.example"})
            for asset, signature in [("/icon.png", b"\x89PNG"), ("/fonts/ibm-plex-sans.woff2", b"wOF2"), ("/fonts/ibm-plex-mono.woff2", b"wOF2")]:
                with urllib.request.urlopen(base + asset, timeout=10) as response:
                    content = response.read()
                    assert content.startswith(signature), asset
                    if expected_assets:
                        assert content == (expected_assets / asset.lstrip("/")).read_bytes(), asset
                    assert "font-src 'self'" in response.headers["Content-Security-Policy"]
            with urllib.request.urlopen(base + "/", timeout=10) as response:
                assert b'/boot.js' in response.read()
                assert "'wasm-unsafe-eval'" in response.headers["Content-Security-Policy"]
            with urllib.request.urlopen(base + "/gpui/harness_bridge_ui.js", timeout=10) as response:
                assert b'export function run' in response.read()
            with urllib.request.urlopen(base + "/gpui/harness_bridge_ui_bg.wasm", timeout=10) as response:
                assert response.headers["Content-Type"] == "application/wasm"
                assert response.headers["Content-Encoding"] == "gzip"
                assert gzip.decompress(response.read()).startswith(b"\0asm")
            catalog = api("/api/catalog")["catalog"]
            assert len(catalog["sessions"]) == 1
            reference = catalog["sessions"][0]
            selection = {"provider": "claude", "id": reference["id"]}
            details = api("/api/inspect", selection)
            assert details["messages"] == 2
            assert [message["role"] for message in details["excerpt_messages"]] == ["user", "assistant"]
            request = {"from": "claude", "id": reference["id"], "to": "qoder"}
            preview = api("/api/preview", request)
            outcome = preview["preview"]["outcome"]
            assert not homes["qoder"].exists(), "preview wrote destination data"
            rows[0]["message"]["content"] = "new source turn after preview"
            save_source()
            source_before = source.read_bytes()
            written = api("/api/write", {"plan_id": preview["plan_id"]})
            assert written["native_id"] == outcome["native_id"]
            transcript = Path(written["location"]).read_text(encoding="utf-8")
            assert "previewed conversation" in transcript and "new source turn after preview" not in transcript
            api("/api/write", {"plan_id": preview["plan_id"]}, expected=400)
            assert source.read_bytes() == source_before
            export_path = root / "transfer.hbridge.json"
            export_request = {**selection, "path": str(export_path)}
            assert api("/api/export", export_request)["sessions"] == 1
            api("/api/export", export_request, expected=400)
            imported = api("/api/preview", {"file": str(export_path), "to": "dsh"})
            assert not homes["dsh"].exists()
            result = api("/api/write", {"plan_id": imported["plan_id"]})
            assert Path(result["location"]).is_file()
            api("/api/preview", {**request, "to": "qoder-ide"}, expected=400)
            expired = api("/api/preview", request)
            api("/api/settings", {name: str(path) for name, path in homes.items()})
            api("/api/write", {"plan_id": expired["plan_id"]}, expected=400)
            assert source.read_bytes() == source_before
            print("PASS: local access checks, discovery, inspection, immutable previews, single-use writes, export, import and settings invalidation")
            if qoder:
                probe_home = root / "native-probe"
                probe_home.mkdir()
                environment = os.environ.copy()
                environment.update(QODER_CONFIG_DIR=str(homes["qoder"]), HOME=str(probe_home), USERPROFILE=str(probe_home))
                native_command = (["node", str(qoder)] if qoder.suffix in (".js", ".mjs") else [str(qoder)])
                native = subprocess.run([*native_command, "--list-sessions", "--cwd", str(project)],
                                        cwd=project, env=environment, capture_output=True, text=True,
                                        encoding="utf-8", errors="replace", timeout=30, creationflags=creation)
                assert native.returncode == 0, native.stderr[:1000]
                assert written["native_id"] in native.stdout, native.stdout[:1000]
                print("PASS: native Qoder discovers the generated session (no model request)")
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
            process.stdout.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bridge", type=Path, default=Path("target/debug/harness-bridge.exe" if os.name == "nt" else "target/debug/harness-bridge"))
    parser.add_argument("--qoder", type=Path, help="Optional Qoder CLI executable or official qodercli.js bundle")
    args = parser.parse_args()
    verify(args.bridge.resolve(), args.qoder.resolve() if args.qoder else None)
