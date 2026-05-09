#!/usr/bin/env python3
"""Compact structured artifact inspection for Codex.

Standard-library only. Designed to keep terminal output bounded while inspecting
JSON, JSONL/NDJSON, CSV, TSV, and generated tool artifacts.
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import re
import shlex
import struct
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence

VERSION = "0.2.0"
DEFAULT_MAX_LINES = 80
DEFAULT_MAX_LINE_CHARS = 240
DEFAULT_MAX_PREVIEW = 160
DEFAULT_MAX_JSON_BYTES = 50 * 1024 * 1024
DEFAULT_JSONL_SCAN = 1000
DEFAULT_JSONL_LINE_CHECK = 20
DEFAULT_SAMPLE_LIMIT = 10
DEFAULT_MAX_LINE_PROBE_BYTES = 1 * 1024 * 1024
DEFAULT_MAX_JSONL_RECORD_BYTES = 5 * 1024 * 1024
DEFAULT_GUARD_ALLOW_SMALL_BYTES = 16 * 1024

STRUCTURED_SUFFIXES = {
    ".json",
    ".jsonl",
    ".ndjson",
    ".csv",
    ".tsv",
    ".parquet",
}
STRUCTURED_BASENAME_HINTS = {
    "summary.json",
    "verification.json",
    "manifest.json",
    "metadata.json",
    "metrics.json",
    "results.json",
    "report.json",
    "status.json",
    "candidate_scores.jsonl",
}
LARGE_FIELD_NAMES = {
    "payload",
    "source",
    "content",
    "prompt",
    "completion",
    "trace",
    "traces",
    "logs",
    "log",
    "stdout",
    "stderr",
    "guidance",
    "messages",
    "history",
    "response",
    "request",
    "raw",
    "blob",
    "html",
    "text",
}
RAW_TOOLS = {"cat", "head", "tail", "sed", "nl"}
BYPASS_MARKERS = ("CODEX_VIEW_ALLOW_RAW=1", "codex-view-allow-raw")


class OutputBudget:
    def __init__(self, max_lines: int = DEFAULT_MAX_LINES, max_line_chars: int = DEFAULT_MAX_LINE_CHARS):
        self.max_lines = max_lines
        self.max_line_chars = max_line_chars
        self.lines = 0
        self.truncated = False

    def emit(self, text: Any = "") -> None:
        if self.lines >= self.max_lines:
            self.truncated = True
            return
        s = str(text)
        # Preserve bounded multi-line values without allowing any one call to exceed the line budget.
        for raw_line in s.splitlines() or [""]:
            if self.lines >= self.max_lines:
                self.truncated = True
                return
            line = raw_line
            if len(line) > self.max_line_chars:
                line = line[: self.max_line_chars - 3] + "..."
            print(line)
            self.lines += 1

    def close(self) -> None:
        if self.truncated and self.lines < self.max_lines + 1:
            print(f"... output truncated by codex_view after {self.max_lines} lines")


def eprint(*args: Any) -> None:
    print(*args, file=sys.stderr)


def read_text_line_lengths(path: Path, line_check: int, max_probe_bytes: int = DEFAULT_MAX_LINE_PROBE_BYTES) -> list[int | str]:
    """Return first line lengths without allocating unbounded lines.

    If a line exceeds max_probe_bytes, record a lower-bound marker and stop;
    reading the rest of a giant line merely to inspect later lines would waste IO.
    """
    lengths: list[int | str] = []
    with path.open("rb") as f:
        for _ in range(line_check):
            total = 0
            saw_any = False
            while True:
                chunk = f.readline(min(8192, max_probe_bytes - total + 1))
                if not chunk:
                    if saw_any:
                        lengths.append(total)
                    return lengths
                saw_any = True
                total += len(chunk)
                if chunk.endswith(b"\n"):
                    lengths.append(total)
                    break
                if total >= max_probe_bytes:
                    lengths.append(f">={max_probe_bytes}")
                    return lengths
    return lengths


def preview(value: Any, max_chars: int = DEFAULT_MAX_PREVIEW) -> str:
    """Return a bounded preview without materializing huge nested reprs."""
    if isinstance(value, str):
        s = repr(value)
    elif isinstance(value, Mapping):
        keys = list(value.keys())[:10]
        s = f"<dict len={len(value)} keys={keys}>"
    elif isinstance(value, (list, tuple)):
        if value:
            first = preview(value[0], max(40, min(max_chars // 2, 120)))
            s = f"<{type(value).__name__} len={len(value)} first={first}>"
        else:
            s = f"<{type(value).__name__} len=0>"
    else:
        s = repr(value)
    s = s.replace("\n", "\\n")
    if len(s) > max_chars:
        return s[: max_chars - 3] + "..."
    return s


def approx_repr_len(value: Any, *, cap: int = 1_000_000, depth: int = 0) -> int:
    """Approximate representation size without building full reprs for large containers."""
    if value is None or isinstance(value, (int, float, bool)):
        return len(repr(value))
    if isinstance(value, str):
        return len(value) + 2
    if isinstance(value, Mapping):
        total = 2
        for i, (k, v) in enumerate(value.items()):
            if i >= 20 or depth >= 3 or total >= cap:
                total += max(0, len(value) - i) * 8
                break
            total += approx_repr_len(k, cap=cap - total, depth=depth + 1)
            total += approx_repr_len(v, cap=cap - total, depth=depth + 1)
            total += 4
        return min(total, cap)
    if isinstance(value, (list, tuple)):
        total = 2
        for i, item in enumerate(value):
            if i >= 20 or depth >= 3 or total >= cap:
                total += max(0, len(value) - i) * 4
                break
            total += approx_repr_len(item, cap=cap - total, depth=depth + 1) + 2
        return min(total, cap)
    return min(len(repr(value)), cap)


def shorten_value(value: Any, max_chars: int = DEFAULT_MAX_PREVIEW) -> Any:
    if isinstance(value, str):
        return value[: max_chars - 3] + "..." if len(value) > max_chars else value
    if isinstance(value, (int, float, bool)) or value is None:
        return value
    return preview(value, max_chars)


def safe_open_text(path: Path):
    return path.open("r", encoding="utf-8-sig", errors="replace", newline="")


def parse_fields(raw: str) -> list[str]:
    fields = [x.strip() for x in raw.split(",") if x.strip()]
    if not fields:
        raise SystemExit("--fields must include at least one field")
    return fields


def get_nested(record: Mapping[str, Any], dotted: str) -> tuple[bool, Any]:
    cur: Any = record
    for part in dotted.split("."):
        if isinstance(cur, Mapping) and part in cur:
            cur = cur[part]
        else:
            return False, None
    return True, cur


def is_large_field(field: str) -> bool:
    lower_parts = {p.lower() for p in field.replace("[", ".").replace("]", "").split(".") if p}
    return bool(lower_parts & LARGE_FIELD_NAMES)


def summarize_json_value(label: str, value: Any, out: OutputBudget, max_preview: int, max_keys: int) -> None:
    if isinstance(value, dict):
        keys = list(value.keys())[:max_keys]
        suffix = " ..." if len(value) > max_keys else ""
        out.emit(f"{label}: dict len={len(value)} keys={keys}{suffix}")
    elif isinstance(value, list):
        out.emit(f"{label}: list len={len(value)}")
        if value:
            first = value[0]
            out.emit(f"  first_type={type(first).__name__}")
            if isinstance(first, dict):
                keys = list(first.keys())[:max_keys]
                suffix = " ..." if len(first) > max_keys else ""
                out.emit(f"  first_keys={keys}{suffix}")
            else:
                out.emit(f"  first_preview={preview(first, max_preview)}")
    else:
        out.emit(f"{label}: {type(value).__name__} {preview(value, max_preview)}")


def max_length_label(lengths: Sequence[int | str]) -> str:
    numeric = [x for x in lengths if isinstance(x, int)]
    lower_bounds = [x for x in lengths if isinstance(x, str)]
    if lower_bounds:
        return str(lower_bounds[0])
    return str(max(numeric)) if numeric else "0"


def iter_bounded_jsonl(path: Path, max_record_bytes: int) -> Iterable[tuple[int, str | None, bool]]:
    """Yield (line_no, decoded_line_or_none, oversize).

    Oversize records are skipped without materializing the full line.
    """
    with path.open("rb") as f:
        line_no = 0
        while True:
            chunk = f.readline(max_record_bytes + 1)
            if not chunk:
                return
            line_no += 1
            if len(chunk) > max_record_bytes:
                # Consume the remainder of this physical line in bounded chunks.
                while chunk and not chunk.endswith(b"\n"):
                    chunk = f.readline(8192)
                yield line_no, None, True
                continue
            encoding = "utf-8-sig" if line_no == 1 else "utf-8"
            yield line_no, chunk.decode(encoding, errors="replace"), False


def cmd_sniff(args: argparse.Namespace) -> int:
    path = Path(args.file)
    out = OutputBudget(args.max_lines, args.max_line_chars)
    if not path.exists():
        eprint(f"not found: {path}")
        return 2
    out.emit(f"path: {path}")
    out.emit(f"bytes: {path.stat().st_size}")
    out.emit(f"suffix: {path.suffix.lower() or '<none>'}")
    if path.is_file():
        lengths = read_text_line_lengths(path, min(args.line_check, 20), args.max_line_probe_bytes)
        if lengths:
            out.emit(f"first_line_lengths: {lengths}")
            out.emit(f"max_first_line_length: {max_length_label(lengths)}")
    out.close()
    return 0


def cmd_json_summary(args: argparse.Namespace) -> int:
    path = Path(args.file)
    out = OutputBudget(args.max_lines, args.max_line_chars)
    if not path.exists():
        eprint(f"not found: {path}")
        return 2
    size = path.stat().st_size
    out.emit(f"path: {path}")
    out.emit(f"bytes: {size}")
    if size > args.max_file_bytes and not args.force:
        out.emit(f"refused_to_load: file exceeds --max-file-bytes={args.max_file_bytes}; pass --force if intentional")
        out.close()
        return 1
    try:
        with safe_open_text(path) as f:
            obj = json.load(f)
    except Exception as exc:
        eprint(f"json parse error: {exc}")
        return 1
    out.emit(f"type: {type(obj).__name__}")
    if isinstance(obj, dict):
        keys = list(obj.keys())[: args.max_keys]
        suffix = " ..." if len(obj) > args.max_keys else ""
        out.emit(f"top_keys: {keys}{suffix}")
        for k, v in obj.items():
            summarize_json_value(str(k), v, out, args.max_preview, args.max_keys)
    elif isinstance(obj, list):
        out.emit(f"list_len: {len(obj)}")
        if obj:
            summarize_json_value("first", obj[0], out, args.max_preview, args.max_keys)
    else:
        out.emit(preview(obj, args.max_preview))
    out.close()
    return 0


def cmd_json_select(args: argparse.Namespace) -> int:
    path = Path(args.file)
    fields = parse_fields(args.fields)
    out = OutputBudget(args.max_lines, args.max_line_chars)
    if not path.exists():
        eprint(f"not found: {path}")
        return 2
    size = path.stat().st_size
    if size > args.max_file_bytes and not args.force:
        out.emit(f"refused_to_load: file exceeds --max-file-bytes={args.max_file_bytes}; pass --force if intentional")
        out.close()
        return 1
    skipped_large = [f for f in fields if is_large_field(f) and not args.include_large_fields]
    effective_fields = [f for f in fields if f not in skipped_large]
    if skipped_large:
        out.emit(f"skipped_large_fields: {skipped_large}; pass --include-large-fields if intentional")
    if not effective_fields:
        out.emit("no_effective_fields: all requested fields are known large fields")
        out.close()
        return 1
    try:
        with safe_open_text(path) as f:
            obj = json.load(f)
    except Exception as exc:
        eprint(f"json parse error: {exc}")
        return 1

    def emit_row(label: str, record: Mapping[str, Any]) -> bool:
        row: dict[str, Any] = {}
        for field in effective_fields:
            ok, value = get_nested(record, field)
            if ok:
                row[field] = shorten_value(value, args.max_chars)
        if not row and not args.include_empty:
            return False
        if args.as_json:
            out.emit(json.dumps({"item": label, **row}, ensure_ascii=False, sort_keys=True))
        else:
            out.emit(f"{label}: {row}")
        return True

    emitted = 0
    if isinstance(obj, Mapping):
        emit_row("root", obj)
    elif isinstance(obj, list):
        for i, item in enumerate(obj):
            if emitted >= args.limit:
                break
            if isinstance(item, Mapping):
                if emit_row(str(i), item):
                    emitted += 1
            elif args.include_empty:
                out.emit(f"{i}: {preview(item, args.max_chars)}")
                emitted += 1
    else:
        out.emit(f"unsupported_json_type: {type(obj).__name__}; select expects an object or list of objects")
        out.close()
        return 1
    out.close()
    return 0


def cmd_jsonl_summary(args: argparse.Namespace) -> int:
    path = Path(args.file)
    out = OutputBudget(args.max_lines, args.max_line_chars)
    if not path.exists():
        eprint(f"not found: {path}")
        return 2
    out.emit(f"path: {path}")
    out.emit(f"bytes: {path.stat().st_size}")
    lengths = read_text_line_lengths(path, args.line_check, args.max_line_probe_bytes)
    out.emit(f"first_line_lengths: {lengths}")
    if lengths:
        out.emit(f"max_first_line_length: {max_length_label(lengths)}")
    key_counter: Counter[str] = Counter()
    type_counter: dict[str, Counter[str]] = defaultdict(Counter)
    max_repr_len: dict[str, int] = defaultdict(int)
    examples: dict[str, str] = {}
    record_type_counter: Counter[str] = Counter()
    scanned = 0
    bad = 0
    oversize = 0
    total_estimated = False
    try:
        for line_no, line, is_oversize in iter_bounded_jsonl(path, args.max_record_bytes):
            if scanned >= args.scan:
                total_estimated = True
                break
            scanned += 1
            if is_oversize:
                oversize += 1
                continue
            assert line is not None
            if not line.strip():
                continue
            try:
                rec = json.loads(line)
            except Exception:
                bad += 1
                continue
            record_type_counter[type(rec).__name__] += 1
            if not isinstance(rec, dict):
                continue
            key_counter.update(map(str, rec.keys()))
            for k_raw, v in rec.items():
                k = str(k_raw)
                type_counter[k][type(v).__name__] += 1
                rlen = approx_repr_len(v)
                if rlen > max_repr_len[k]:
                    max_repr_len[k] = rlen
                    examples[k] = preview(v, args.max_preview)
    except Exception as exc:
        eprint(f"jsonl read error: {exc}")
        return 1
    out.emit(f"records_scanned: {scanned}{'+' if total_estimated else ''}")
    out.emit(f"bad_json_scanned: {bad}")
    if oversize:
        out.emit(f"oversize_records_skipped: {oversize} (>{args.max_record_bytes} bytes each)")
    out.emit(f"record_types: {dict(record_type_counter)}")
    out.emit("top_keys:")
    for k, count in key_counter.most_common(args.max_keys):
        marker = " LARGE_FIELD_NAME" if is_large_field(k) else ""
        out.emit(
            f"  {k}: count={count} types={dict(type_counter[k])} "
            f"max_repr_len={max_repr_len[k]} sample={examples.get(k, '')}{marker}"
        )
    out.close()
    return 0


def cmd_jsonl_project(args: argparse.Namespace) -> int:
    path = Path(args.file)
    fields = parse_fields(args.fields)
    out = OutputBudget(args.max_lines, args.max_line_chars)
    if not path.exists():
        eprint(f"not found: {path}")
        return 2
    skipped_large = [f for f in fields if is_large_field(f) and not args.include_large_fields]
    effective_fields = [f for f in fields if f not in skipped_large]
    if skipped_large:
        out.emit(f"skipped_large_fields: {skipped_large}; pass --include-large-fields if intentional")
    if not effective_fields:
        out.emit("no_effective_fields: all requested fields are known large fields")
        out.close()
        return 1
    emitted = 0
    bad = 0
    oversize = 0
    try:
        for line_no, line, is_oversize in iter_bounded_jsonl(path, args.max_record_bytes):
            if emitted >= args.limit:
                break
            if is_oversize:
                oversize += 1
                continue
            assert line is not None
            if not line.strip():
                continue
            try:
                rec = json.loads(line)
            except Exception:
                bad += 1
                continue
            if not isinstance(rec, dict):
                out.emit(f"{line_no}: {preview(rec, args.max_chars)}")
                emitted += 1
                continue
            row: dict[str, Any] = {}
            for field in effective_fields:
                ok, value = get_nested(rec, field)
                if ok:
                    row[field] = shorten_value(value, args.max_chars)
            if row or args.include_empty:
                if args.as_json:
                    out.emit(json.dumps({"line": line_no, **row}, ensure_ascii=False, sort_keys=True))
                else:
                    out.emit(f"{line_no}: {row}")
                emitted += 1
    except Exception as exc:
        eprint(f"jsonl project error: {exc}")
        return 1
    if bad:
        out.emit(f"bad_json_skipped: {bad}")
    if oversize:
        out.emit(f"oversize_records_skipped: {oversize} (>{args.max_record_bytes} bytes each)")
    out.close()
    return 0


def detect_delimiter(path: Path, explicit: str | None) -> str:
    if explicit:
        return "\t" if explicit == "\\t" else explicit
    if path.suffix.lower() == ".tsv":
        return "\t"
    # Lightweight sniff from first 4KB.
    try:
        with safe_open_text(path) as f:
            sample = f.read(4096)
        dialect = csv.Sniffer().sniff(sample, delimiters=",\t;|")
        return dialect.delimiter
    except Exception:
        return ","


def cmd_csv_summary(args: argparse.Namespace) -> int:
    path = Path(args.file)
    out = OutputBudget(args.max_lines, args.max_line_chars)
    if not path.exists():
        eprint(f"not found: {path}")
        return 2
    delim = detect_delimiter(path, args.delimiter)
    out.emit(f"path: {path}")
    out.emit(f"bytes: {path.stat().st_size}")
    out.emit(f"delimiter: {repr(delim)}")
    try:
        with safe_open_text(path) as f:
            reader = csv.DictReader(f, delimiter=delim)
            headers = reader.fieldnames or []
            out.emit(f"columns({len(headers)}): {headers[:args.max_columns]}{' ...' if len(headers) > args.max_columns else ''}")
            rows_scanned = 0
            nonempty_counter: Counter[str] = Counter()
            max_len: dict[str, int] = defaultdict(int)
            examples: dict[str, str] = {}
            samples: list[dict[str, Any]] = []
            for row in reader:
                rows_scanned += 1
                if len(samples) < args.sample:
                    samples.append({k: shorten_value(v, args.max_preview) for k, v in list(row.items())[: args.max_columns]})
                for k, v in row.items():
                    if v not in (None, ""):
                        nonempty_counter[str(k)] += 1
                        lv = len(str(v))
                        if lv > max_len[str(k)]:
                            max_len[str(k)] = lv
                            examples[str(k)] = preview(v, args.max_preview)
                if rows_scanned >= args.scan:
                    break
            out.emit(f"rows_scanned: {rows_scanned}{'+' if rows_scanned >= args.scan else ''}")
            out.emit("column_stats:")
            for k in headers[: args.max_columns]:
                marker = " LARGE_FIELD_NAME" if is_large_field(str(k)) else ""
                out.emit(f"  {k}: nonempty={nonempty_counter[str(k)]} max_len={max_len[str(k)]} sample={examples.get(str(k), '')}{marker}")
            if samples:
                out.emit("samples:")
                for i, sample in enumerate(samples):
                    out.emit(f"  {i}: {sample}")
    except Exception as exc:
        eprint(f"csv summary error: {exc}")
        return 1
    out.close()
    return 0


def cmd_csv_project(args: argparse.Namespace) -> int:
    path = Path(args.file)
    fields = parse_fields(args.fields)
    out = OutputBudget(args.max_lines, args.max_line_chars)
    if not path.exists():
        eprint(f"not found: {path}")
        return 2
    delim = detect_delimiter(path, args.delimiter)
    skipped_large = [f for f in fields if is_large_field(f) and not args.include_large_fields]
    effective_fields = [f for f in fields if f not in skipped_large]
    if skipped_large:
        out.emit(f"skipped_large_fields: {skipped_large}; pass --include-large-fields if intentional")
    if not effective_fields:
        out.emit("no_effective_fields: all requested fields are known large fields")
        out.close()
        return 1
    try:
        with safe_open_text(path) as f:
            reader = csv.DictReader(f, delimiter=delim)
            for i, row in enumerate(reader):
                if i >= args.limit:
                    break
                out_row = {field: shorten_value(row.get(field, ""), args.max_chars) for field in effective_fields}
                if args.as_json:
                    out.emit(json.dumps({"row": i + 1, **out_row}, ensure_ascii=False, sort_keys=True))
                else:
                    out.emit(f"{i + 1}: {out_row}")
    except Exception as exc:
        eprint(f"csv project error: {exc}")
        return 1
    out.close()
    return 0


def cmd_parquet_summary(args: argparse.Namespace) -> int:
    path = Path(args.file)
    out = OutputBudget(args.max_lines, args.max_line_chars)
    if not path.exists():
        eprint(f"not found: {path}")
        return 2
    size = path.stat().st_size
    out.emit(f"path: {path}")
    out.emit(f"bytes: {size}")
    if size < 12:
        out.emit("parquet_magic_ok: false")
        out.close()
        return 1
    try:
        with path.open("rb") as f:
            head = f.read(4)
            f.seek(-8, os.SEEK_END)
            footer = f.read(8)
    except Exception as exc:
        eprint(f"parquet read error: {exc}")
        return 1
    footer_len = struct.unpack("<I", footer[:4])[0]
    magic_ok = head == b"PAR1" and footer[4:] == b"PAR1"
    out.emit(f"parquet_magic_ok: {str(magic_ok).lower()}")
    out.emit(f"footer_length_bytes: {footer_len}")
    if not magic_ok:
        out.close()
        return 1

    try:
        import pyarrow.parquet as pq  # type: ignore[import-not-found]
    except Exception:
        out.emit("column_metadata: unavailable (install pyarrow for schema/row-group metadata)")
        out.close()
        return 0

    try:
        parquet_file = pq.ParquetFile(path)
        meta = parquet_file.metadata
        out.emit(f"rows: {meta.num_rows}")
        out.emit(f"row_groups: {meta.num_row_groups}")
        out.emit(f"columns: {meta.num_columns}")
        if meta.created_by:
            out.emit(f"created_by: {meta.created_by}")
        names = list(parquet_file.schema_arrow.names)
        suffix = " ..." if len(names) > args.max_columns else ""
        out.emit(f"column_names({len(names)}): {names[:args.max_columns]}{suffix}")
    except Exception as exc:
        out.emit(f"column_metadata_error: {type(exc).__name__}: {preview(str(exc), args.max_preview)}")
    out.close()
    return 0


def cmd_summary(args: argparse.Namespace) -> int:
    suffix = Path(args.file).suffix.lower()
    if suffix == ".json":
        return cmd_json_summary(args)
    if suffix in {".jsonl", ".ndjson"}:
        return cmd_jsonl_summary(args)
    if suffix in {".csv", ".tsv"}:
        return cmd_csv_summary(args)
    if suffix == ".parquet":
        return cmd_parquet_summary(args)
    eprint("unsupported structured summary type; use sniff for a bounded file probe")
    return 2


def cmd_select(args: argparse.Namespace) -> int:
    suffix = Path(args.file).suffix.lower()
    if suffix == ".json":
        return cmd_json_select(args)
    if suffix in {".jsonl", ".ndjson"}:
        return cmd_jsonl_project(args)
    if suffix in {".csv", ".tsv"}:
        return cmd_csv_project(args)
    eprint("unsupported structured select type; select supports JSON, JSONL/NDJSON, CSV, and TSV")
    return 2


def looks_structured_path(token: str) -> bool:
    # Strip common redirection and shell quoting residue.
    t = token.strip().strip("'\"")
    if not t or t.startswith("-"):
        return False
    # Remove trailing punctuation often left by simple token scans.
    t = t.rstrip(";,)")
    if any(ch in t for ch in "*$?{}"):
        # Avoid attempting to classify dynamic paths/globs too aggressively.
        # `*.jsonl` should still be treated as risky.
        if not re.search(r"\.(jsonl?|ndjson|csv|tsv|parquet)(\b|$|['\";,)])", t, re.I):
            return False
    lower = os.path.basename(t).lower()
    if lower in STRUCTURED_BASENAME_HINTS:
        return True
    suffix = Path(t).suffix.lower()
    if suffix in STRUCTURED_SUFFIXES:
        return True
    return bool(re.search(r"\.(jsonl?|ndjson|csv|tsv|parquet)(\b|$|['\";,)])", t, re.I))


def tokenize_shell(command: str) -> list[str]:
    try:
        return shlex.split(command, posix=True)
    except Exception:
        # Fallback keeps punctuation separated enough for a conservative scan.
        return re.findall(r"[^\s]+", command)


def structured_target_is_risky(token: str, cwd: str | None, allow_small_bytes: int) -> bool:
    if not looks_structured_path(token):
        return False
    t = token.strip().strip("'\"").rstrip(";,)")
    # Globs/dynamic paths cannot be statted reliably; treat them as risky.
    if any(ch in t for ch in "*$?{}"):
        return True
    try:
        p = Path(t)
        if not p.is_absolute() and cwd:
            p = Path(cwd) / p
        if p.exists() and p.is_file() and p.stat().st_size <= allow_small_bytes:
            return False
    except Exception:
        pass
    return True


def dangerous_raw_structured_command(command: str, cwd: str | None = None, allow_small_bytes: int = DEFAULT_GUARD_ALLOW_SMALL_BYTES) -> tuple[bool, str]:
    if not command.strip():
        return False, ""
    if any(marker in command for marker in BYPASS_MARKERS):
        return False, ""
    tokens = tokenize_shell(command)
    if not tokens:
        return False, ""

    # Direct python -m json.tool FILE.
    for i, tok in enumerate(tokens[:-2]):
        base = os.path.basename(tok)
        if base in {"python", "python3"} or base.startswith("python"):
            if tokens[i + 1] == "-m" and tokens[i + 2] == "json.tool":
                rest = tokens[i + 3 :]
                targets = [t for t in rest if structured_target_is_risky(t, cwd, allow_small_bytes)]
                if targets:
                    return True, f"raw JSON pretty-print on structured artifact: {targets[0]}"

    # Direct raw tools. Scan command segments separated by shell operators.
    operators = {"|", "||", "&&", ";"}
    i = 0
    while i < len(tokens):
        tok = tokens[i]
        if tok in operators:
            i += 1
            continue
        base = os.path.basename(tok)
        if base in RAW_TOOLS:
            segment: list[str] = []
            j = i + 1
            while j < len(tokens) and tokens[j] not in operators:
                segment.append(tokens[j])
                j += 1
            targets = [t for t in segment if structured_target_is_risky(t, cwd, allow_small_bytes)]
            if targets:
                return True, f"raw `{base}` on structured artifact: {targets[0]}"
            i = j
        else:
            i += 1

    # Shell wrappers such as bash -lc 'cat file.jsonl'. Recurse one level.
    for i, tok in enumerate(tokens[:-2]):
        base = os.path.basename(tok)
        if base in {"bash", "sh", "zsh"} and tokens[i + 1] in {"-c", "-lc"}:
            inner = tokens[i + 2]
            risky, reason = dangerous_raw_structured_command(inner, cwd, allow_small_bytes)
            if risky:
                return risky, reason
    return False, ""


def cmd_guard(args: argparse.Namespace) -> int:
    try:
        payload = json.load(sys.stdin)
    except Exception as exc:
        # Fail open; hooks should not break unrelated work if Codex changes schema.
        if args.debug:
            eprint(f"guard: failed to parse stdin JSON: {exc}")
        return 0
    tool_input = payload.get("tool_input") or {}
    command = ""
    if isinstance(tool_input, dict):
        command_value = tool_input.get("command") or tool_input.get("cmd") or ""
        if isinstance(command_value, list):
            command = " ".join(map(str, command_value))
        else:
            command = str(command_value)
    cwd = str(payload.get("cwd") or "") or None
    risky, reason = dangerous_raw_structured_command(command, cwd, args.allow_small_bytes)
    if not risky:
        return 0
    message = (
        f"Structured Artifact Viewer blocked {reason}. Use MCP `structured_artifact_viewer` "
        f"with `summary`/`select`, or CLI `codex-view summary FILE`. "
        f"Bypass only when intentional with CODEX_VIEW_ALLOW_RAW=1 or # codex-view-allow-raw."
    )
    if args.mode == "warn":
        print(json.dumps({"systemMessage": message}, ensure_ascii=False))
        return 0
    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": message,
        }
    }, ensure_ascii=False))
    return 0


def merge_hooks(existing: dict[str, Any], command: str, mode: str) -> dict[str, Any]:
    hooks_root = existing.setdefault("hooks", {})
    pre = hooks_root.setdefault("PreToolUse", [])
    if not isinstance(pre, list):
        raise SystemExit("existing hooks.PreToolUse is not a list; refusing to modify")
    marker = "structured-artifact-viewer"
    hook_cmd = f'{shlex.quote(sys.executable)} {shlex.quote(command)} guard --mode {shlex.quote(mode)} --allow-small-bytes {DEFAULT_GUARD_ALLOW_SMALL_BYTES}'
    entry = {
        "matcher": "Bash",
        "hooks": [
            {
                "type": "command",
                "command": hook_cmd,
                "timeout": 5,
                "statusMessage": "Checking structured artifact output budget"
            }
        ],
        "_managedBy": marker
    }
    # Replace existing managed entry.
    new_pre = []
    for item in pre:
        if isinstance(item, dict) and item.get("_managedBy") == marker:
            continue
        # Also replace older entries whose command references this script guard.
        item_text = json.dumps(item, ensure_ascii=False, sort_keys=True) if isinstance(item, dict) else str(item)
        if "codex_view.py" in item_text and " guard" in item_text:
            continue
        new_pre.append(item)
    new_pre.append(entry)
    hooks_root["PreToolUse"] = new_pre
    return existing


def cmd_install_hook(args: argparse.Namespace) -> int:
    script = Path(__file__).resolve()
    if args.scope == "repo":
        root = find_repo_root(Path.cwd())
        target_dir = root / ".codex"
        target = target_dir / "hooks.json"
    else:
        home = Path.home()
        target_dir = home / ".codex"
        target = target_dir / "hooks.json"
    target_dir.mkdir(parents=True, exist_ok=True)
    if target.exists():
        try:
            existing = json.loads(target.read_text(encoding="utf-8"))
        except Exception as exc:
            raise SystemExit(f"cannot parse existing {target}: {exc}")
    else:
        existing = {"hooks": {}}
    merged = merge_hooks(existing, str(script), args.mode)
    tmp = target.with_suffix(target.suffix + ".tmp")
    tmp.write_text(json.dumps(merged, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(target)
    print(f"installed {args.mode} hook: {target}")
    print(f"script: {script}")
    print("If your Codex version/config requires it, enable [features].codex_hooks = true.")
    return 0


def cmd_install_command(args: argparse.Namespace) -> int:
    script = Path(__file__).resolve()
    if args.scope == "repo":
        root = find_repo_root(Path.cwd())
        target_dir = root / ".codex" / "bin"
    else:
        target_dir = Path.home() / ".local" / "bin"
    target_dir.mkdir(parents=True, exist_ok=True)
    sh_target = target_dir / "codex-view"
    sh_content = (
        "#!/usr/bin/env sh\n"
        "set -eu\n"
        f"exec {shlex.quote(sys.executable)} {shlex.quote(str(script))} \"$@\"\n"
    )
    sh_target.write_text(sh_content, encoding="utf-8")
    sh_target.chmod(0o755)
    # Windows-friendly companion wrapper. Harmless on POSIX.
    cmd_target = target_dir / "codex-view.cmd"
    cmd_content = f'@echo off\r\n"{sys.executable}" "{script}" %*\r\n'
    cmd_target.write_text(cmd_content, encoding="utf-8")
    print(f"installed command: {sh_target}")
    print(f"installed command: {cmd_target}")
    if args.scope == "repo":
        print("Use: .codex/bin/codex-view summary FILE")
    else:
        print("Ensure ~/.local/bin is on PATH, then use: codex-view summary FILE")
    return 0


def find_repo_root(start: Path) -> Path:
    cur = start.resolve()
    for candidate in [cur, *cur.parents]:
        if (candidate / ".git").exists():
            return candidate
    return cur


def add_common_output_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--max-lines", type=int, default=DEFAULT_MAX_LINES)
    parser.add_argument("--max-line-chars", type=int, default=DEFAULT_MAX_LINE_CHARS)


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="codex_view.py", description="Compact structured artifact inspection for Codex")
    p.add_argument("--version", action="version", version=VERSION)
    sub = p.add_subparsers(dest="command", required=True)

    sp = sub.add_parser("sniff", help="Print bounded file size/type and initial line lengths")
    sp.add_argument("file")
    sp.add_argument("--line-check", type=int, default=DEFAULT_JSONL_LINE_CHECK)
    sp.add_argument("--max-line-probe-bytes", type=int, default=DEFAULT_MAX_LINE_PROBE_BYTES)
    add_common_output_args(sp)
    sp.set_defaults(func=cmd_sniff)

    sp = sub.add_parser("summary", help="Auto-summarize JSON, JSONL, CSV/TSV, or Parquet metadata")
    sp.add_argument("file")
    sp.add_argument("--delimiter")
    sp.add_argument("--scan", type=int, default=DEFAULT_JSONL_SCAN)
    sp.add_argument("--sample", type=int, default=5)
    sp.add_argument("--line-check", type=int, default=DEFAULT_JSONL_LINE_CHECK)
    sp.add_argument("--max-line-probe-bytes", type=int, default=DEFAULT_MAX_LINE_PROBE_BYTES)
    sp.add_argument("--max-record-bytes", type=int, default=DEFAULT_MAX_JSONL_RECORD_BYTES)
    sp.add_argument("--max-preview", type=int, default=DEFAULT_MAX_PREVIEW)
    sp.add_argument("--max-keys", type=int, default=60)
    sp.add_argument("--max-columns", type=int, default=40)
    sp.add_argument("--max-file-bytes", type=int, default=DEFAULT_MAX_JSON_BYTES)
    sp.add_argument("--force", action="store_true")
    add_common_output_args(sp)
    sp.set_defaults(func=cmd_summary)

    sp = sub.add_parser("select", help="Preview selected fields from JSON, JSONL, CSV, or TSV")
    sp.add_argument("file")
    sp.add_argument("--fields", required=True, help="comma-separated field names; dotted JSON paths are supported")
    sp.add_argument("--delimiter")
    sp.add_argument("--limit", type=int, default=DEFAULT_SAMPLE_LIMIT)
    sp.add_argument("--max-chars", type=int, default=DEFAULT_MAX_PREVIEW)
    sp.add_argument("--max-record-bytes", type=int, default=DEFAULT_MAX_JSONL_RECORD_BYTES)
    sp.add_argument("--max-file-bytes", type=int, default=DEFAULT_MAX_JSON_BYTES)
    sp.add_argument("--force", action="store_true")
    sp.add_argument("--include-large-fields", action="store_true")
    sp.add_argument("--include-empty", action="store_true")
    sp.add_argument("--as-json", action="store_true")
    add_common_output_args(sp)
    sp.set_defaults(func=cmd_select)

    sp = sub.add_parser("json-summary", help="Summarize JSON top-level structure without full dump")
    sp.add_argument("file")
    sp.add_argument("--max-preview", type=int, default=DEFAULT_MAX_PREVIEW)
    sp.add_argument("--max-keys", type=int, default=40)
    sp.add_argument("--max-file-bytes", type=int, default=DEFAULT_MAX_JSON_BYTES)
    sp.add_argument("--force", action="store_true")
    add_common_output_args(sp)
    sp.set_defaults(func=cmd_json_summary)

    sp = sub.add_parser("jsonl-summary", help="Summarize JSONL keys, field lengths, and samples")
    sp.add_argument("file")
    sp.add_argument("--scan", type=int, default=DEFAULT_JSONL_SCAN)
    sp.add_argument("--line-check", type=int, default=DEFAULT_JSONL_LINE_CHECK)
    sp.add_argument("--max-line-probe-bytes", type=int, default=DEFAULT_MAX_LINE_PROBE_BYTES)
    sp.add_argument("--max-record-bytes", type=int, default=DEFAULT_MAX_JSONL_RECORD_BYTES)
    sp.add_argument("--max-preview", type=int, default=120)
    sp.add_argument("--max-keys", type=int, default=60)
    add_common_output_args(sp)
    sp.set_defaults(func=cmd_jsonl_summary)

    sp = sub.add_parser("jsonl-project", help="Project selected JSONL fields with truncation")
    sp.add_argument("file")
    sp.add_argument("--fields", required=True, help="comma-separated field names; dotted paths are supported")
    sp.add_argument("--limit", type=int, default=DEFAULT_SAMPLE_LIMIT)
    sp.add_argument("--max-chars", type=int, default=DEFAULT_MAX_PREVIEW)
    sp.add_argument("--max-record-bytes", type=int, default=DEFAULT_MAX_JSONL_RECORD_BYTES)
    sp.add_argument("--include-large-fields", action="store_true")
    sp.add_argument("--include-empty", action="store_true")
    sp.add_argument("--as-json", action="store_true")
    add_common_output_args(sp)
    sp.set_defaults(func=cmd_jsonl_project)

    sp = sub.add_parser("csv-summary", help="Summarize CSV/TSV headers and column samples")
    sp.add_argument("file")
    sp.add_argument("--delimiter")
    sp.add_argument("--scan", type=int, default=1000)
    sp.add_argument("--sample", type=int, default=5)
    sp.add_argument("--max-preview", type=int, default=DEFAULT_MAX_PREVIEW)
    sp.add_argument("--max-columns", type=int, default=40)
    add_common_output_args(sp)
    sp.set_defaults(func=cmd_csv_summary)

    sp = sub.add_parser("csv-project", help="Project selected CSV/TSV columns with truncation")
    sp.add_argument("file")
    sp.add_argument("--fields", required=True)
    sp.add_argument("--delimiter")
    sp.add_argument("--limit", type=int, default=DEFAULT_SAMPLE_LIMIT)
    sp.add_argument("--max-chars", type=int, default=DEFAULT_MAX_PREVIEW)
    sp.add_argument("--include-large-fields", action="store_true")
    sp.add_argument("--as-json", action="store_true")
    add_common_output_args(sp)
    sp.set_defaults(func=cmd_csv_project)

    sp = sub.add_parser("parquet-summary", help="Summarize Parquet footer/schema metadata without reading rows")
    sp.add_argument("file")
    sp.add_argument("--max-preview", type=int, default=DEFAULT_MAX_PREVIEW)
    sp.add_argument("--max-columns", type=int, default=40)
    add_common_output_args(sp)
    sp.set_defaults(func=cmd_parquet_summary)

    sp = sub.add_parser("guard", help="PreToolUse hook guard; reads Codex hook JSON on stdin")
    sp.add_argument("--mode", choices=["deny", "warn"], default="deny")
    sp.add_argument("--allow-small-bytes", type=int, default=DEFAULT_GUARD_ALLOW_SMALL_BYTES)
    sp.add_argument("--debug", action="store_true")
    sp.set_defaults(func=cmd_guard)

    sp = sub.add_parser("install-hook", help="Install repo/user PreToolUse hook guard with absolute script path")
    sp.add_argument("--scope", choices=["repo", "user"], default="repo")
    sp.add_argument("--mode", choices=["deny", "warn"], default="deny")
    sp.set_defaults(func=cmd_install_hook)

    sp = sub.add_parser("install-command", help="Install a small codex-view wrapper into .codex/bin or ~/.local/bin")
    sp.add_argument("--scope", choices=["repo", "user"], default="repo")
    sp.set_defaults(func=cmd_install_command)

    return p


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
