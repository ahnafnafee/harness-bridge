"""Probe a converted rollout with the real app-server and a local Responses stub.

The original rollout/home is read only. No credentials or real model calls are
used. A successful thread/resume alone cannot detect an oversized next request.
"""

import argparse
import gzip
import http.server
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import tempfile
import threading
import time


def verify(codex, rollout, max_input_chars, timeout):
    captured = queue.Queue()

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            raw = self.rfile.read(int(self.headers["Content-Length"]))
            if self.headers.get("Content-Encoding") == "gzip":
                raw = gzip.decompress(raw)
            inputs = json.loads(raw).get("input", [])
            size = len(json.dumps(inputs, ensure_ascii=False))
            captured.put({"path": self.path, "input_chars": size, "items": len(inputs)})
            if size > max_input_chars:
                self.send_response(400)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(json.dumps({"error": {
                    "message": "Your input exceeds the context window of this model. Please adjust your input and try again.",
                    "type": "invalid_request_error", "code": "context_length_exceeded",
                }}).encode())
                return
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            for status in ("in_progress", "completed"):
                response = {"id": "resp_resume_probe", "object": "response", "status": status, "output": []}
                if status == "completed":
                    response["usage"] = {"input_tokens": 100, "output_tokens": 0, "total_tokens": 100}
                event = {"type": "response.created" if status == "in_progress" else "response.completed", "response": response}
                self.wfile.write(("data: " + json.dumps(event) + "\n\n").encode())
            self.wfile.flush()

    with tempfile.TemporaryDirectory(prefix="hb-resume-probe-") as temporary:
        home = Path(temporary)
        target = home / "sessions" / rollout.name
        target.parent.mkdir()
        shutil.copyfile(rollout, target)
        with target.open(encoding="utf-8") as source:
            thread_id = json.loads(source.readline())["payload"]["id"]
        workspace = home / "workspace"
        workspace.mkdir()
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        (home / "config.toml").write_text(f'''
model = "gpt-6.1-sol"
model_provider = "resume_probe"
model_context_window = 258400
model_auto_compact_token_limit = 10000000
[model_providers.resume_probe]
name = "Local resume probe"
base_url = "http://127.0.0.1:{server.server_port}/v1"
wire_api = "responses"
requires_openai_auth = false
request_max_retries = 0
stream_max_retries = 0
[features]
plugins = false
''', encoding="utf-8")
        messages = queue.Queue()
        with (home / "stderr.log").open("w", encoding="utf-8") as stderr:
            process = subprocess.Popen(
                [codex, "app-server", "--stdio"],
                env=dict(os.environ, CODEX_HOME=str(home)),
                cwd=workspace, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=stderr, text=True, encoding="utf-8",
            )

            def read_messages():
                for line in process.stdout:
                    try:
                        messages.put(json.loads(line))
                    except ValueError:
                        pass

            threading.Thread(target=read_messages, daemon=True).start()

            def send(method, params, identifier=None):
                message = {"method": method, "params": params}
                if identifier is not None:
                    message["id"] = identifier
                process.stdin.write(json.dumps(message) + "\n")
                process.stdin.flush()

            def result(identifier):
                deadline = time.monotonic() + timeout
                while time.monotonic() < deadline:
                    message = messages.get(timeout=max(0.01, deadline - time.monotonic()))
                    if message.get("id") == identifier:
                        if "error" in message:
                            raise RuntimeError(message["error"])
                        return message["result"]
                raise TimeoutError(f"app-server did not answer request {identifier}")

            try:
                send("initialize", {"clientInfo": {"name": "resume_probe", "version": "1.0"}, "capabilities": {"experimentalApi": True}}, 1)
                result(1)
                send("initialized", {})
                send("thread/resume", {"threadId": thread_id, "path": str(target), "cwd": str(workspace),
                                       "modelProvider": "resume_probe", "model": "gpt-6.1-sol", "excludeTurns": True}, 2)
                result(2)
                send("thread/turns/list", {"threadId": thread_id, "limit": 100}, 3)
                turns = result(3)
                print(f"Resumed {len(turns.get('data', []))} displayed turns", flush=True)
                send("turn/start", {"threadId": thread_id, "input": [{"type": "text", "text": "resume", "text_elements": []}]}, 4)
                result(4)
                request = captured.get(timeout=timeout)
                print(json.dumps(request), flush=True)
                deadline = time.monotonic() + timeout
                errors = []
                completed = None
                while time.monotonic() < deadline:
                    message = messages.get(timeout=max(0.01, deadline - time.monotonic()))
                    if message.get("method") == "error":
                        errors.append(message["params"]["error"])
                    if message.get("method") == "turn/completed":
                        completed = message["params"]["turn"]
                        break
                if request["input_chars"] > max_input_chars:
                    raise RuntimeError("Your input exceeds the context window of this model")
                if errors or completed is None or completed.get("status") != "completed":
                    raise RuntimeError(f"Resumed turn failed: {errors or completed}")
                print("PASS: next request fits the configured character limit and the local response completes")
            finally:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=10)
                server.shutdown()
                server.server_close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("rollout", type=Path)
    parser.add_argument("--codex", default=shutil.which("codex"), help="Codex executable path")
    parser.add_argument("--max-input-chars", type=int, default=1_000_000, help="Local request size limit; this is not a token estimate")
    parser.add_argument("--timeout", type=int, default=45)
    arguments = parser.parse_args()
    if not arguments.codex:
        parser.error("Codex is not on PATH; pass --codex")
    verify(arguments.codex, arguments.rollout.resolve(), arguments.max_input_chars, arguments.timeout)
