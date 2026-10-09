"""Probe a converted rollout with the real app-server and a local Responses stub.

The original rollout/home is read only. No credentials or real model calls are
used. Check request size and unsigned imported reasoning, including the response
continuation after a harmless plan-tool call. The replay check is a
local regression contract, not verification against the remote Responses service.
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
    persisted = {}
    request_number = 0

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            nonlocal request_number
            raw = self.rfile.read(int(self.headers["Content-Length"]))
            if self.headers.get("Content-Encoding") == "gzip":
                raw = gzip.decompress(raw)
            body = json.loads(raw)
            inputs = body.get("input", [])
            previous = body.get("previous_response_id")
            history = persisted.get(previous, []) + inputs
            size = len(json.dumps(inputs, ensure_ascii=False))
            unsigned = sum(item.get("type") == "reasoning" and not item.get("encrypted_content") for item in history)
            request_number += 1
            captured.put({"path": self.path, "input_chars": size, "items": len(inputs),
                          "previous_response_id": previous, "unsigned_reasoning": unsigned})
            error = None
            if unsigned and request_number > 1:
                error = {"message": "Persisted response contains unverifiable hidden reasoning state that Rustponses cannot replay.",
                         "type": "invalid_request_error", "param": "previous_response_id" if previous else "input",
                         "code": "unsupported_persisted_item_context"}
            elif size > max_input_chars:
                error = {"message": "Your input exceeds the context window of this model. Please adjust your input and try again.",
                         "type": "invalid_request_error", "param": "input", "code": "context_length_exceeded"}
            if error:
                self.send_response(400)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(json.dumps({"error": error}).encode())
                return
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            response_id = f"resp_resume_probe_{request_number}"
            output = []
            if request_number == 1:
                # Exercise a second request inside the same turn. Custom HTTP
                # providers may resend full history instead of a response id.
                # The tool updates only the isolated conversation's plan.
                output = [{"type": "function_call", "id": "fc_resume_probe", "call_id": "call_resume_probe",
                           "name": "update_plan", "arguments": json.dumps({"plan": [
                               {"step": "Verify imported context replay", "status": "completed"}]})}]
            persisted[response_id] = history + output

            def emit(event):
                self.wfile.write(("data: " + json.dumps(event) + "\n\n").encode())
                self.wfile.flush()

            for status in ("in_progress", "completed"):
                response = {"id": response_id, "object": "response", "status": status, "output": output if status == "completed" else []}
                if status == "completed":
                    response["usage"] = {"input_tokens": 100, "output_tokens": 0, "total_tokens": 100}
                event = {"type": "response.created" if status == "in_progress" else "response.completed", "response": response}
                if status == "completed":
                    for index, item in enumerate(output):
                        emit({"type": "response.output_item.added", "output_index": index, "item": dict(item, arguments="")})
                        emit({"type": "response.function_call_arguments.delta", "output_index": index, "item_id": item["id"], "delta": item["arguments"]})
                        emit({"type": "response.function_call_arguments.done", "output_index": index, "item_id": item["id"], "arguments": item["arguments"]})
                        emit({"type": "response.output_item.done", "output_index": index, "item": item})
                emit(event)

    with tempfile.TemporaryDirectory(prefix="hb-resume-probe-") as temporary:
        home = Path(temporary)
        with rollout.open(encoding="utf-8") as source:
            thread_id = json.loads(source.readline())["payload"]["id"]
        # Native discovery expects a rollout filename, including when the input
        # is a backup or repair candidate named differently from the live file.
        name = rollout.name if rollout.name.startswith("rollout-") else f"rollout-2000-01-01T00-00-00-{thread_id}.jsonl"
        target = home / "sessions" / name
        target.parent.mkdir()
        shutil.copyfile(rollout, target)
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
                requests = []
                while not captured.empty():
                    requests.append(captured.get_nowait())
                for request in requests:
                    print(json.dumps(request), flush=True)
                if any(request["input_chars"] > max_input_chars for request in requests):
                    raise RuntimeError("Your input exceeds the context window of this model")
                if errors or completed is None or completed.get("status") != "completed":
                    raise RuntimeError(f"Resumed turn failed: {errors or completed}")
                if len(requests) < 2:
                    raise RuntimeError("The probe did not exercise a tool response continuation")
                if any(request["unsigned_reasoning"] for request in requests):
                    raise RuntimeError("The imported context contains unsigned native reasoning")
                mode = "previous_response_id" if any(request["previous_response_id"] for request in requests[1:]) else "full history"
                print(f"PASS: request size and tool continuation ({mode}) pass the local replay contract")
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
