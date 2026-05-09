---
name: structured-artifact-inspection
description: Use for unknown-size, large, generated, or high-output structured artifacts; avoid for small known config files or jq-style queries.
---

# Structured Artifact Inspection

Use this skill only for bounded inspection of structured artifacts when raw output could waste context: unknown-size or large JSON, JSONL/NDJSON, CSV/TSV, Parquet metadata, and generated tool artifact files.

Do not use this for search, transformation, calculation, validation, editing, or precise jq-style querying. This plugin is a compact viewer, not a jq replacement.
For small known files, especially ordinary config files, read them directly unless they contain long records or large fields.

Prefer the MCP tool `structured_artifact_viewer` when available.
Use the CLI `codex-view` only as a fallback or when the user explicitly asks for shell commands.

Workflow:

1. Inspect the file shape with `op: "sniff"` or `codex-view sniff FILE`.
2. Summarize structure with `op: "summary"` or `codex-view summary FILE`.
3. Preview selected fields with `op: "select"` or `codex-view select FILE --fields name,status,score --limit 10`.
4. Never print full JSONL records by default; one record may be tens of kilobytes.
5. If exact full content is required, first prove the file is small or write raw output to a file and report only the path plus a short summary.

Avoid raw `cat`, `head`, `tail`, `sed`, `nl`, and `python -m json.tool` on these files unless the user explicitly asks for raw content.
