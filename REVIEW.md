# Implementation review notes

## Scope

This plugin packages a compact local CLI plus a short Codex skill. It targets JSON, JSONL/NDJSON, CSV, TSV, and generated tool artifacts. It does not provide an MCP server.

## Review loop summary

### Pass 1

Initial implementation included:

- `.codex-plugin/plugin.json`
- `skills/structured-artifact-inspection/SKILL.md`
- `scripts/codex_view.py`
- unit tests

Issue found: JSON/JSONL summaries used `repr()` on nested objects. This could waste CPU/memory on very large nested fields even if output was truncated.

Fix:

- Added bounded previews for `dict`/`list` values.
- Added approximate size accounting instead of full `repr()` for JSONL field-length detection.

### Pass 2

Issue found: the skill examples assumed `scripts/codex_view.py` was directly available from the session working directory. That is not guaranteed after plugin installation.

Fix:

- Added `install-command` to create `.codex/bin/codex-view` with an absolute path to the bundled script.
- Updated skill and README to use `.codex/bin/codex-view` after setup.
- Added `bin/codex-view` wrapper for manual PATH-based usage.

### Pass 3

Issue found: line-length probing used `readline()`, which could allocate a giant first JSONL line.

Fix:

- Replaced unbounded line probing with bounded chunk-based probing.
- Added bounded JSONL iteration with `--max-record-bytes`.
- Oversize JSONL records are skipped and reported without materializing their full content.

### Pass 4

Issue found: a strict guard hook would block small, normal JSON files such as `package.json`.

Fix:

- Added `--allow-small-bytes`, default 16 KiB, for existing concrete files.
- Dynamic paths/globs and large files remain guarded.
- Added explicit bypass markers for intentional raw reads.

## Known limits

- Codex plugin-local hooks may vary by Codex version/config. The package therefore uses `install-hook` to write a repo/user hook with an absolute script path instead of relying on plugin-local hook discovery.
- The hook is a guardrail, not a security boundary. Codex hooks do not intercept every possible shell or tool path in every runtime.
- JSON files larger than the default `--max-file-bytes` are refused unless `--force` is passed. This avoids accidental giant `json.load()` calls.
- JSONL projection skips oversize physical lines by default. Increase `--max-record-bytes` when intentionally inspecting large records.

## Test status

Validated with standard-library unittest tests covering:

- JSON summary truncation
- JSONL summary field statistics
- JSONL projection and large-field skipping
- CSV summary/projection
- PreToolUse guard deny/warn behavior
- small-file allow-list behavior
- oversize JSONL record skipping
- hook merge idempotence
- `.codex/bin/codex-view` wrapper installation
