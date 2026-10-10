"""Exercise encrypted-checkpoint transfer through the CLI and native stores.

Uses synthetic sessions in isolated source/receiver homes. The Codex app-server
probe uses a local Responses stub and never invokes a remote model. Optional
--source-rollout adds a read-only check of an actual encrypted source session.
"""

import argparse
from contextlib import closing
import hashlib
import json
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile

from verify_codex_resume import verify


def rows(path, records):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(json.dumps(row, ensure_ascii=False) for row in records) + "\n", encoding="utf-8")


def snapshot(path):
    return {str(file.relative_to(path)): hashlib.sha256(file.read_bytes()).hexdigest()
            for file in path.rglob("*") if file.is_file()} if path.exists() else {}


def message(text, role="user"):
    return {"type": "message", "role": role, "content": [{"type": "output_text" if role == "assistant" else "input_text", "text": text}]}


def agent_mail(text, encrypted=False):
    content = [{"type": "input_text", "text": text}]
    if encrypted:
        content.append({"type": "encrypted_content", "encrypted_content": "hidden-agent-payload"})
    return {"type": "agent_message", "author": "/root/worker", "recipient": "/root", "content": content}


def record(payload, kind="response_item"):
    return {"timestamp": "2026-10-09T00:00:00Z", "type": kind, "payload": payload}


def template(home):
    rows(home / "sessions/template.jsonl", [
        record({"id": "00000000-0000-7000-8000-000000000099", "timestamp": "2026-10-09T00:00:00Z",
                "originator": "Codex Desktop", "thread_source": "user", "source": "vscode",
                "cwd": str(home), "base_instructions": {"text": "Local transfer test"}}, "session_meta"),
        record({"model": "gpt-6.1-sol", "cwd": str(home), "approval_policy": "never",
                "sandbox_policy": {"type": "danger-full-access"}, "effort": "low", "summary": "auto"}, "turn_context"),
    ])


def initialize_targets(receiver):
    template(receiver / "codex")
    database = receiver / "zcode.sqlite"
    with closing(sqlite3.connect(database)) as db:
        db.executescript("""
            create table session(id text primary key,project_id text,slug text,directory text,path text,title text,version text,permission text,time_created integer,time_updated integer,task_type text,title_source text,parent_id text);
            create table message(id text primary key,session_id text,time_created integer,time_updated integer,data text,sequence integer);
            create table part(id text primary key,message_id text,session_id text,time_created integer,time_updated integer,data text,sequence integer);
            create table session_entry(id text primary key,session_id text,type text,time_created integer,time_updated integer,data text);
        """)


def exercise(bridge, codex, source_rollout):
    with tempfile.TemporaryDirectory(prefix="hb-transfer-e2e-") as temporary:
        base = Path(temporary)
        source = base / "source-pc/codex"
        receiver = base / "receiving-pc"
        receiver.mkdir()
        initialize_targets(receiver)
        source_ids = [f"00000000-0000-7000-8000-{i:012d}" for i in range(1, 4)]
        call = {"type": "custom_tool_call", "call_id": "source-call", "name": "read", "input": "original arguments"}
        output = {"type": "custom_tool_call_output", "call_id": "source-call", "output": [{"type": "text", "text": "output evidence " * 5000}]}
        for index, session_id in enumerate(source_ids):
            initial = [record({"id": session_id, "timestamp": "2026-10-09T00:00:00Z", "cwd": "source-pc/project",
                               "originator": "Codex Desktop", "parent_thread_id": source_ids[index - 1] if index else None}, "session_meta"),
                       record(message("superseded archive evidence")), record(agent_mail("archived plaintext report")), record(call), record(output)]
            if index == 0:
                initial.append(record({"message": "", "replacement_history": [message("retained task"), agent_mail("retained plaintext report"), agent_mail("readable agent header", encrypted=True), call, output,
                    {"type": "reasoning", "summary": [{"type": "summary_text", "text": "visible reasoning"}], "encrypted_content": "hidden-reasoning-state"},
                    {"type": "compaction", "encrypted_content": "hidden-compaction-state"}]}, "compacted"))
            elif index == 1:
                initial.append(record(agent_mail("uncompacted readable header", encrypted=True)))
            elif index == 2:
                initial.append(record({"message": "legacy compaction without replacement"}, "compacted"))
            initial.append(record(agent_mail("after-checkpoint plaintext report")))
            if index == 0:
                initial.append(record(agent_mail("encrypted tail header", encrypted=True)))
            initial.append(record(message("latest task request")))
            rows(source / f"sessions/rollout-2026-10-09T00-00-00-{session_id}.jsonl", initial)
        source_before = snapshot(source)

        rows(source / "session_index.jsonl", [{"id": source_ids[0], "thread_name": "Stale index title"}])
        with closing(sqlite3.connect(source / "state_5.sqlite")) as database:
            database.execute("create table threads(id text,title text,name text)")
            database.execute("insert into threads values(?,?,?)", (source_ids[0], "Original prompt title", "Encrypted transfer fixture"))
            database.commit()
        source_before = snapshot(source)

        def run(arguments, expect=0):
            result = subprocess.run([str(bridge), *map(str, arguments)], capture_output=True, text=True, encoding="utf-8", timeout=90)
            if expect == 0 and result.returncode != 0:
                raise RuntimeError(result.stderr or result.stdout)
            if expect != 0 and result.returncode == 0:
                raise RuntimeError("Expected a rejected conversion")
            return result

        export = base / "source-pc/family.hbridge.json"
        command = ["export", "Encrypted transfer fixture", "--from", "codex", "--codex-home", source, "--output", export, "--include-subagents"]
        run([*command, "--dry-run"])
        assert not export.exists()
        report = json.loads(run(command).stdout)
        assert report["version"] == 2 and report["sessions"] == 3
        assert len(report["resume_context_unavailable"]) == 3
        exported_bytes = export.read_bytes()
        run(command, expect=1)
        assert export.read_bytes() == exported_bytes
        moved = receiver / "family.hbridge.json"
        shutil.move(str(export), moved)
        # The receiver is never pointed at the real/synthetic source store.
        receiver_source = receiver / "source-store-does-not-exist"
        assert not receiver_source.exists()
        payload = json.loads(moved.read_text(encoding="utf-8"))["payload"]
        assert "hidden-compaction-state" not in moved.read_text(encoding="utf-8")
        assert "hidden-agent-payload" not in moved.read_text(encoding="utf-8")
        assert payload["sessions"][2]["parent_session"] == source_ids[1]
        destinations = {
            "codex": ["--codex-home", receiver / "codex"],
            "claude": ["--claude-home", receiver / "claude", "--codex-home", receiver_source],
            "dsh": ["--dsh-home", receiver / "dsh", "--codex-home", receiver_source],
            "zcode": ["--zcode-db", receiver / "zcode.sqlite", "--codex-home", receiver_source],
        }
        codex_outcome = None
        transferred = {}
        for target, flags in destinations.items():
            command = ["import", moved, "--to", target, *flags, "--cwd", receiver / "project", "--resume-max-chars", 6000]
            before = snapshot(receiver)
            rejected = run(command, expect=1)
            assert "--rebuild-resume-context" in rejected.stderr
            assert snapshot(receiver) == before
            rejected = run([*command, "--rebuild-resume-context"], expect=1)
            assert "safety budget" in rejected.stderr
            assert snapshot(receiver) == before
            recovery = [*command, "--rebuild-resume-context", "--prune-resume-context"]
            preview = json.loads(run([*recovery, "--dry-run"]).stdout)
            assert len(preview["extra"]["child_sessions"]) == 2
            assert snapshot(receiver) == before
            outcome = json.loads(run(recovery).stdout)
            assert outcome["extra"]["resume_context_policy"]["reconstruction_source"] == "readable-checkpoint"
            assert len(outcome["extra"]["child_sessions"]) == 2
            assert outcome["extra"]["child_sessions"][1]["extra"]["resume_context_policy"]["reconstruction_source"] == "archive"
            roundtrip = receiver / f"{target}-roundtrip.hbridge.json"
            run(["export", outcome["native_id"], "--from", target, *flags, "--output", roundtrip, "--include-subagents"])
            reread = json.loads(roundtrip.read_text(encoding="utf-8"))["payload"]["sessions"]
            assert len(reread) == 3
            assert reread[2]["parent_session"] == reread[1]["id"]
            assert all(not session.get("resume_context_unavailable") for session in reread)
            assert "superseded archive evidence" in roundtrip.read_text(encoding="utf-8")
            assert "archived plaintext report" in roundtrip.read_text(encoding="utf-8")
            active = json.dumps(reread[0]["resume_events"])
            assert "retained plaintext report" in active and "after-checkpoint plaintext report" in active
            assert "from /root/worker to /root" in active and "Encrypted agent-message payload unavailable" in active
            assert "output evidence output evidence" in roundtrip.read_text(encoding="utf-8")
            assert moved.read_bytes() == exported_bytes
            transferred[target] = roundtrip
            print(f"PASS: CLI export/move/import/readback to {target}; nested links, guards, budgets, dry runs and archive preservation")
            if target == "codex":
                codex_outcome = outcome
        for source_provider, transfer in transferred.items():
            for destination, flags in destinations.items():
                matrix_receiver = base / f"matrix-{source_provider}-{destination}"
                matrix_receiver.mkdir()
                initialize_targets(matrix_receiver)
                relocated_flags = [str(value).replace(str(receiver), str(matrix_receiver)) for value in flags]
                result = json.loads(run(["import", transfer, "--to", destination, *relocated_flags,
                                         "--cwd", matrix_receiver / "project", "--prune-resume-context"]).stdout)
                assert len(result["extra"]["child_sessions"]) == 2
                roundtrip = matrix_receiver / "readback.hbridge.json"
                run(["export", result["native_id"], "--from", destination, *relocated_flags,
                     "--output", roundtrip, "--include-subagents"])
                sessions = json.loads(roundtrip.read_text(encoding="utf-8"))["payload"]["sessions"]
                assert len(sessions) == 3
                assert sessions[2]["parent_session"] == sessions[1]["id"]
                assert "output evidence output evidence" in roundtrip.read_text(encoding="utf-8")
                if destination == "codex":
                    verify(str(codex), Path(result["location"]), 1_000_000, 45, expected_texts=("retained plaintext report", "after-checkpoint plaintext report"))
            print(f"PASS: {source_provider} export imports and reads back through all four destination harnesses")
        assert snapshot(source) == source_before
        for outcome in [codex_outcome, *codex_outcome["extra"]["child_sessions"]]:
            verify(str(codex), Path(outcome["location"]), 1_000_000, 45, expected_texts=("after-checkpoint plaintext report",))
        print("PASS: real Codex app-server resumes checkpoint and archive recovery, including tool continuation")

        plain_id = "00000000-0000-7000-8000-000000000004"
        rows(source / f"sessions/rollout-2026-10-09T00-00-00-{plain_id}.jsonl", [
            record({"id": plain_id, "timestamp": "2026-10-09T00:00:00Z", "cwd": "source-pc/project"}, "session_meta"),
            record(agent_mail("plaintext archive report")),
            record({"replacement_history": [message("plain task"), agent_mail("plaintext retained report")]}, "compacted"),
            record(agent_mail("plaintext later report")),
        ])
        plain_bundle = receiver / "plaintext-agent-mail.hbridge.json"
        plain_report = json.loads(run(["export", plain_id, "--from", "codex", "--codex-home", source,
                                       "--output", plain_bundle]).stdout)
        assert plain_report["version"] == 1 and not plain_report["resume_context_unavailable"]
        for destination, flags in destinations.items():
            outcome = json.loads(run(["import", plain_bundle, "--to", destination, *flags]).stdout)
            assert not outcome["extra"]["resume_context_policy"].get("rebuilt")
            readback = receiver / f"plaintext-{destination}.hbridge.json"
            run(["export", outcome["native_id"], "--from", destination, *flags, "--output", readback])
            data = json.loads(readback.read_text(encoding="utf-8"))["payload"]["sessions"][0]
            assert "plaintext archive report" in json.dumps(data["events"])
            active = json.dumps(data["resume_events"])
            assert "plaintext retained report" in active and "plaintext later report" in active
            if destination == "codex":
                verify(str(codex), Path(outcome["location"]), 1_000_000, 45,
                       expected_texts=("plaintext retained report", "plaintext later report"))
        print("PASS: plaintext agent mail exports as v1 and imports into all four harnesses without reconstruction")

        if source_rollout:
            real_source = base / "real-source-copy"
            with source_rollout.open(encoding="utf-8") as handle:
                session_id = json.loads(handle.readline())["payload"]["id"]
            original_hash = hashlib.sha256(source_rollout.read_bytes()).hexdigest()
            copy = real_source / f"sessions/rollout-2026-10-09T00-00-00-{session_id}.jsonl"
            copy.parent.mkdir(parents=True)
            shutil.copyfile(source_rollout, copy)
            real_bundle = receiver / "real-session.hbridge.json"
            real_report = json.loads(run(["export", session_id, "--from", "codex", "--codex-home", real_source,
                                         "--output", real_bundle]).stdout)
            assert real_report["version"] == 2
            real_target = receiver / "real-codex"
            template(real_target)
            result = json.loads(run(["import", real_bundle, "--to", "codex", "--codex-home", real_target,
                                     "--rebuild-resume-context", "--prune-resume-context", "--cwd", receiver / "project"]).stdout)
            print("Actual encrypted source recovery:", json.dumps(result["extra"]["resume_context_policy"]))
            verify(str(codex), Path(result["location"]), 1_000_000, 45)
            assert hashlib.sha256(source_rollout.read_bytes()).hexdigest() == original_hash
            print("PASS: actual encrypted source exported, imported and resumed without source modification")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bridge", type=Path, required=True)
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--source-rollout", type=Path)
    args = parser.parse_args()
    exercise(args.bridge.resolve(), args.codex.resolve(), args.source_rollout.resolve() if args.source_rollout else None)
