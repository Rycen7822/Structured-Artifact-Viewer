---
name: structured-artifact-inspection
description: Use when inspecting JSON, JSONL, CSV, TSV, Parquet metadata, or tool artifact files to avoid raw dumps and keep terminal output compact.
---

# Structured artifact inspection

Use this skill before inspecting `.json`, `.jsonl`, `.csv`, `.tsv`, `.parquet`, or generated tool artifact files.

Rules:

1. Do not use raw `cat`, `head`, `tail`, `sed`, `nl`, or `python -m json.tool` on structured artifacts unless file size is already proven small and the user explicitly needs raw content.
2. Use the bundled CLI first: summary, then selected-field projection.
3. Never print full JSONL records by default. One JSONL line may be tens of kilobytes.
4. Truncate scalar previews. Avoid known large fields unless debugging that exact field.
5. If full content is needed, write it to an artifact file and report only the path plus a short summary.

Setup from repository root after copying this plugin to `plugins/structured-artifact-viewer`:

```bash
python plugins/structured-artifact-viewer/scripts/codex_view.py install-command --scope repo
```

This creates `.codex/bin/codex-view`. If the plugin is installed somewhere else, locate `scripts/codex_view.py` under the same plugin root as this `skills/` directory, then run `install-command`.

Default commands:

```bash
.codex/bin/codex-view sniff FILE
.codex/bin/codex-view json-summary FILE
.codex/bin/codex-view jsonl-summary FILE
.codex/bin/codex-view jsonl-project FILE --fields name,status,score,path --limit 10
.codex/bin/codex-view csv-summary FILE
.codex/bin/codex-view csv-project FILE --fields name,status,score,path --limit 10
```

Optional guard hook:

```bash
python plugins/structured-artifact-viewer/scripts/codex_view.py install-command --scope repo
python plugins/structured-artifact-viewer/scripts/codex_view.py install-hook --scope repo --mode deny
```

Known large fields to avoid by default:

`payload`, `source`, `content`, `prompt`, `completion`, `trace`, `logs`, `log`, `stdout`, `stderr`, `guidance`, `messages`, `history`, `response`, `request`, `raw`, `blob`.
