## Structured file output discipline

- For JSON/JSONL/CSV/TSV/tool artifacts, use `structured-artifact-inspection` or `.codex/bin/codex-view` before reading content.
- Do not use raw `cat`, `head`, `tail`, `sed`, `nl`, or `python -m json.tool` on structured artifacts unless file size is proven small and raw output is intentionally needed.
- Prefer summary → projection → exact field read. Never dump full JSONL records by default.
