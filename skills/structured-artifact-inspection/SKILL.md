---
name: structured-artifact-inspection
description: Use when inspecting JSON, JSONL, CSV, TSV, Parquet metadata, or tool artifact files to avoid raw dumps and keep terminal output compact.
---

# Structured Artifact Inspection

Use this skill only for bounded inspection of structured artifacts: JSON,
JSONL/NDJSON, CSV/TSV, Parquet metadata, and generated tool artifact files.

Do not use this for search, transformation, calculation, validation, editing,
or precise jq-style querying. This plugin is a compact viewer, not a jq
replacement.

Workflow:

1. Run `codex-view sniff FILE`.
2. Run `codex-view summary FILE`.
3. If specific fields matter, run `codex-view select FILE --fields name,status,score --limit 10`.
4. Never print full JSONL records by default; one record may be tens of kilobytes.
5. If exact full content is required, first prove the file is small or write
   raw output to a file and report only the path plus a short summary.

Avoid raw `cat`, `head`, `tail`, `sed`, `nl`, and `python -m json.tool` on
these files unless the user explicitly asks for raw content.
