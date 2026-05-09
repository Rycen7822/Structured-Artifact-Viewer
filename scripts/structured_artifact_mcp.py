#!/usr/bin/env python3
"""Low-context MCP wrapper for Structured Artifact Viewer.

The MCP surface intentionally exposes one generic tool with `op` and `args`.
The detailed format handling stays in codex_view.py so the CLI, hook messages,
and MCP tool remain behaviorally aligned.
"""

from __future__ import annotations

import contextlib
import io
import json
import sys
import traceback
from pathlib import Path
from typing import Any

import codex_view

PROTOCOL_VERSION = "2025-06-18"
SERVER_INFO = {"name": "structured-artifact-viewer", "version": codex_view.VERSION}
TOOL_NAME = "structured_artifact_viewer"
OPS = {"sniff", "summary", "select", "self_check"}
COMMON_ARGS = {"path", "file", "config", "max_lines", "max_line_chars"}
OP_ARGS = {
    "sniff": {"line_check", "max_line_probe_bytes"},
    "summary": {
        "delimiter",
        "scan",
        "sample",
        "line_check",
        "max_line_probe_bytes",
        "max_record_bytes",
        "max_preview",
        "max_keys",
        "max_columns",
        "max_file_bytes",
        "force",
    },
    "select": {
        "fields",
        "delimiter",
        "limit",
        "max_chars",
        "max_record_bytes",
        "max_file_bytes",
        "force",
        "include_large_fields",
        "include_empty",
        "as_json",
    },
}


def json_dumps(obj: Any) -> str:
    return json.dumps(obj, ensure_ascii=False, separators=(",", ":"))


def tool_defs() -> list[dict[str, Any]]:
    return [
        {
            "name": TOOL_NAME,
            "description": "Bounded inspection for unknown-size, large, generated, or high-output structured artifacts.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "op": {"type": "string", "enum": ["sniff", "summary", "select", "self_check"]},
                    "args": {"type": "object"},
                },
                "required": ["op"],
                "additionalProperties": False,
            },
        }
    ]


def send(obj: dict[str, Any]) -> None:
    sys.stdout.write(json_dumps(obj) + "\n")
    sys.stdout.flush()


def result(req_id: Any, data: Any) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": req_id, "result": data}


def error(req_id: Any, code: int, msg: str, data: Any = None) -> dict[str, Any]:
    body = {"code": code, "message": msg}
    if data is not None:
        body["data"] = data
    return {"jsonrpc": "2.0", "id": req_id, "error": body}


def normalize_fields(value: Any) -> str:
    if isinstance(value, str):
        return value
    if isinstance(value, list) and all(isinstance(item, str) for item in value):
        return ",".join(value)
    raise ValueError("fields must be a comma-separated string or list of strings")


def add_flag(argv: list[str], flag: str, value: Any) -> None:
    if isinstance(value, bool):
        if value:
            argv.append(flag)
        return
    argv.extend([flag, str(value)])


def cli_argv(op: str, args: dict[str, Any]) -> list[str]:
    allowed = COMMON_ARGS | OP_ARGS.get(op, set())
    unknown = sorted(set(args) - allowed)
    if unknown:
        raise ValueError(f"unsupported args for {op}: {unknown}")
    path = args.get("path", args.get("file"))
    if not isinstance(path, str) or not path:
        raise ValueError(f"{op} requires args.path")
    argv = [op, path]
    for key in sorted(allowed - {"path", "file", "fields"}):
        if key not in args:
            continue
        add_flag(argv, "--" + key.replace("_", "-"), args[key])
    if op == "select":
        if "fields" not in args:
            raise ValueError("select requires args.fields")
        argv.extend(["--fields", normalize_fields(args["fields"])])
    return argv


def capture_cli(argv: list[str]) -> tuple[int, str, str]:
    out = io.StringIO()
    err = io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = codex_view.main(argv)
    return int(code), out.getvalue(), err.getvalue()


def next_steps(op: str, args: dict[str, Any]) -> list[dict[str, Any]]:
    path = args.get("path", args.get("file"))
    if not isinstance(path, str) or not path:
        return []
    if op == "sniff":
        return [{"op": "summary", "args": {"path": path}}]
    if op == "summary":
        suffix = Path(path).suffix.lower()
        if suffix in {".json", ".jsonl", ".ndjson", ".csv", ".tsv"}:
            return [{"op": "select", "args": {"path": path, "fields": ["name", "status", "score", "path"], "limit": 10}}]
    return []


def self_check() -> dict[str, Any]:
    root = Path(__file__).resolve().parents[1]
    return {
        "status": "ok",
        "version": codex_view.VERSION,
        "server": SERVER_INFO["name"],
        "root": str(root),
        "cli": str(root / "scripts" / "codex_view.py"),
        "ops": sorted(OPS),
    }


def call_tool(name: str, arguments: dict[str, Any]) -> dict[str, Any]:
    if name != TOOL_NAME:
        return {"status": "error", "error": "unknown_tool", "tool": name}
    op = arguments.get("op")
    if op not in OPS:
        return {"status": "error", "error": "bad_op", "valid_ops": sorted(OPS)}
    args = arguments.get("args") or {}
    if not isinstance(args, dict):
        return {"status": "error", "error": "args_must_be_object"}
    if op == "self_check":
        return self_check()
    try:
        argv = cli_argv(op, args)
        exit_code, stdout, stderr = capture_cli(argv)
    except Exception as exc:
        return {"status": "error", "error": exc.__class__.__name__, "message": str(exc)}
    status = "ok" if exit_code == 0 else "error"
    data: dict[str, Any] = {
        "status": status,
        "op": op,
        "path": args.get("path", args.get("file")),
        "exit_code": exit_code,
        "output": stdout.rstrip("\n"),
    }
    if stderr:
        data["stderr"] = stderr.rstrip("\n")
    if status == "ok":
        data["next"] = next_steps(op, args)
    return data


def handle(msg: dict[str, Any]) -> dict[str, Any] | None:
    method = msg.get("method")
    req_id = msg.get("id")
    params = msg.get("params") or {}
    if method == "initialize":
        requested = params.get("protocolVersion") or PROTOCOL_VERSION
        proto = requested if requested in {"2025-06-18", "2025-03-26", "2024-11-05"} else PROTOCOL_VERSION
        return result(
            req_id,
            {
                "protocolVersion": proto,
                "capabilities": {"tools": {"listChanged": False}},
                "serverInfo": SERVER_INFO,
                "instructions": "Use for unknown-size, large, generated, or high-output structured artifacts. Prefer summary, then select. Avoid for small known config files or jq-style queries.",
            },
        )
    if method == "notifications/initialized":
        return None
    if method == "ping":
        return result(req_id, {})
    if method == "tools/list":
        return result(req_id, {"tools": tool_defs()})
    if method == "tools/call":
        name = params.get("name")
        args = params.get("arguments") or {}
        try:
            data = call_tool(str(name), args if isinstance(args, dict) else {})
        except Exception as exc:
            data = {
                "status": "error",
                "error": exc.__class__.__name__,
                "message": str(exc),
                "trace": traceback.format_exc(limit=3),
            }
        return result(
            req_id,
            {
                "content": [{"type": "text", "text": json_dumps(data)}],
                "isError": data.get("status") == "error",
            },
        )
    if req_id is None:
        return None
    return error(req_id, -32601, f"method not found: {method}")


def main() -> int:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except Exception as exc:
            send(error(None, -32700, "parse error", str(exc)))
            continue
        resp = handle(msg)
        if resp is not None:
            send(resp)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
