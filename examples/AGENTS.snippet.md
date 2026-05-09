## Structured file output discipline

- For unknown-size, large, generated, or high-output JSON/JSONL/CSV/TSV/Parquet metadata/tool artifacts, use the `structured_artifact_viewer` MCP tool before reading content.
- For small known config files, direct reads are usually fine.
- If MCP is unavailable, use `structured-artifact-inspection` or `.codex/bin/codex-view`.
- Do not use raw `cat`, `head`, `tail`, `sed`, `nl`, or `python -m json.tool` on structured artifacts unless file size is proven small and raw output is intentionally needed.
- Prefer summary → projection → exact field read. Never dump full JSONL records by default.
