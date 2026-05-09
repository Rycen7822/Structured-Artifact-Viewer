#!/usr/bin/env python3
"""Compare Rust CLI output against the Python implementation on representative cases."""

from __future__ import annotations

import csv
import json
import os
import struct
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
PY = ROOT / "scripts" / "codex_view.py"
PY_MCP = ROOT / "scripts" / "structured_artifact_mcp.py"
RUST = Path(__file__).resolve().parents[1] / "target" / "debug" / "codex-view"
RUST_MCP = Path(__file__).resolve().parents[1] / "target" / "debug" / "structured-artifact-mcp-server"


def run(cmd: list[str], *, stdin: str = "", cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(cmd, input=stdin, cwd=cwd or ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)


def compare_case(name: str, args: list[str], *, stdin: str = "", cwd: Path | None = None, json_stdout: bool = False) -> tuple[bool, str]:
    py = run([sys.executable, str(PY), *args], stdin=stdin, cwd=cwd)
    rs = run([str(RUST), *args], stdin=stdin, cwd=cwd)
    if py.returncode != rs.returncode:
        return False, f"{name}: returncode python={py.returncode} rust={rs.returncode}\nPY stderr={py.stderr}\nRS stderr={rs.stderr}"
    if py.stderr != rs.stderr:
        return False, f"{name}: stderr mismatch\nPY={py.stderr!r}\nRS={rs.stderr!r}"
    if json_stdout and py.stdout and rs.stdout:
        if json.loads(py.stdout) != json.loads(rs.stdout):
            return False, f"{name}: json stdout mismatch\nPY={py.stdout}\nRS={rs.stdout}"
    elif py.stdout != rs.stdout:
        return False, f"{name}: stdout mismatch\n--- PY ---\n{py.stdout}\n--- RS ---\n{rs.stdout}"
    return True, name


def compare_mcp(name: str, request: dict) -> tuple[bool, str]:
    raw = json.dumps(request, separators=(",", ":")) + "\n"
    py = run([sys.executable, str(PY_MCP)], stdin=raw)
    rs = run([str(RUST_MCP)], stdin=raw)
    if py.returncode != rs.returncode:
        return False, f"{name}: MCP returncode python={py.returncode} rust={rs.returncode}\nPY stderr={py.stderr}\nRS stderr={rs.stderr}"
    if py.stderr != rs.stderr:
        return False, f"{name}: MCP stderr mismatch\nPY={py.stderr!r}\nRS={rs.stderr!r}"
    py_obj = json.loads(py.stdout)
    rs_obj = json.loads(rs.stdout)
    if py_obj != rs_obj:
        return False, f"{name}: MCP stdout mismatch\nPY={py.stdout}\nRS={rs.stdout}"
    return True, name


def main() -> int:
    if not RUST.exists() or not RUST_MCP.exists():
        print(f"missing Rust binary: {RUST} or {RUST_MCP}", file=sys.stderr)
        return 2
    failures: list[str] = []
    with tempfile.TemporaryDirectory() as td_raw:
        td = Path(td_raw)
        status = td / "status.json"
        status.write_text(
            json.dumps({"stage": "x", "payload": "A" * 2000, "runs": [{"name": "a", "score": 1}], "nested": {"score": 7}}),
            encoding="utf-8",
        )
        rows = td / "rows.jsonl"
        rows.write_text(
            "\n".join(
                [
                    json.dumps({"name": "a", "score": 1, "payload": "B" * 1000, "nested": {"score": 2}}),
                    json.dumps({"name": "b", "score": 2, "payload": "C" * 1000, "nested": {"score": 3}}),
                ]
            )
            + "\n",
            encoding="utf-8",
        )
        bad_rows = td / "mixed.jsonl"
        bad_rows.write_text('{"name":"ok"}\nnot-json\n[1,2]\n', encoding="utf-8")
        csv_path = td / "results.csv"
        with csv_path.open("w", encoding="utf-8", newline="") as f:
            w = csv.DictWriter(f, fieldnames=["name", "status", "score", "logs"])
            w.writeheader()
            w.writerow({"name": "a", "status": "ok", "score": "0.9", "logs": "D" * 1000})
            w.writerow({"name": "b", "status": "fail", "score": "0.1", "logs": ""})
        parquet = td / "sample.parquet"
        parquet.write_bytes(b"PAR1" + struct.pack("<I", 0) + b"PAR1")
        config = td / "viewer.toml"
        config.write_text("[budget]\nscan = 1\nmax_lines = 12\n", encoding="utf-8")

        cases = [
            ("sniff", ["sniff", str(rows), "--line-check", "2"]),
            ("json-summary", ["json-summary", str(status), "--max-lines", "30", "--max-line-chars", "220"]),
            ("json-select", ["select", str(status), "--fields", "stage,payload,nested.score"]),
            ("json-select-as-json", ["select", str(status), "--fields", "stage,nested.score", "--as-json"]),
            ("jsonl-summary", ["jsonl-summary", str(rows), "--scan", "2"]),
            ("jsonl-project", ["jsonl-project", str(rows), "--fields", "name,payload,nested.score", "--limit", "1"]),
            ("jsonl-project-as-json", ["jsonl-project", str(rows), "--fields", "name,nested.score", "--limit", "1", "--as-json"]),
            ("jsonl-mixed", ["jsonl-summary", str(bad_rows), "--scan", "3"]),
            ("csv-summary", ["csv-summary", str(csv_path), "--scan", "2"]),
            ("csv-project", ["csv-project", str(csv_path), "--fields", "name,status,score,logs", "--limit", "1"]),
            ("parquet-summary", ["parquet-summary", str(parquet)]),
            ("generic-summary", ["summary", str(rows), "--scan", "2"]),
            ("generic-select", ["select", str(csv_path), "--fields", "name,status", "--limit", "1"]),
            ("config", ["--config", str(config), "jsonl-summary", str(rows)]),
        ]
        for name, args in cases:
            ok, msg = compare_case(name, args)
            if not ok:
                failures.append(msg)

        guard_payload = json.dumps({"tool_input": {"command": "sed -n '1,80p' candidate_scores.jsonl"}})
        ok, msg = compare_case("guard-deny", ["guard", "--mode", "deny"], stdin=guard_payload, json_stdout=True)
        if not ok:
            failures.append(msg)
        ok, msg = compare_case("guard-warn", ["guard", "--mode", "warn"], stdin=guard_payload, json_stdout=True)
        if not ok:
            failures.append(msg)

        ok, msg = compare_mcp("mcp-tools-list", {"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}})
        if not ok:
            failures.append(msg)
        ok, msg = compare_mcp(
            "mcp-summary-call",
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "structured_artifact_viewer",
                    "arguments": {"op": "summary", "args": {"path": str(rows), "scan": 2}},
                },
            },
        )
        if not ok:
            failures.append(msg)

    if failures:
        print("\n\n".join(failures))
        return 1
    print("PY_RUST_COMPAT_OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
