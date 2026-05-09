# Implementation review notes

## Scope

This plugin packages a single low-context MCP tool, compact local CLI, optional hook guard, and short Codex skill. It targets bounded inspection of unknown-size, large, generated, or high-output JSON, JSONL/NDJSON, CSV, TSV, Parquet metadata, and generated tool artifacts. It is not a `jq` replacement and should not displace direct reads of small known config files.

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

### Pass 5

Issue found: the plugin could be mistaken for a broader JSON processing or
`jq` replacement, and YAML detection could interfere with ordinary config-file
reads.

Fix:

- Shortened the skill to inspection-only guidance.
- Added generic `summary` and `select` commands so agents do not need to choose
  format-specific commands for common bounded inspection.
- Added `parquet-summary` for footer/schema metadata only; it does not read row
  data.
- Removed YAML/YML from guarded structured suffixes.

### Pass 6

Issue found: with only a short skill plus CLI guidance, repeated skill loading
could either be too terse to guide agents or too verbose and waste context.

Fix:

- Added a single MCP tool, `structured_artifact_viewer`, with a compact
  `op + args` schema.
- Kept the CLI as fallback and as the hook denial target.
- Updated the skill to prefer MCP when available while keeping the plugin
  explicitly inspection-only.
- Added `.mcp.json` and an executable `bin/structured-artifact-mcp-server`
  wrapper for plugin packaging.

### Pass 7

Issue found: after prebuilt or plugin installation, users could override budget
values only by passing CLI/MCP arguments on every call.

Fix:

- Added external budget config loading for the Python CLI.
- Supported user, project, environment, and explicit config paths with priority
  below CLI flags.
- Kept config limited to numeric budget defaults so it cannot silently enable
  `--force` or turn the viewer into a raw dump path.
- Reused the same config path through MCP because the MCP wrapper delegates to
  the CLI parser.

## Known limits

- Codex plugin-local hooks may vary by Codex version/config. The package therefore uses `install-hook` to write a repo/user hook with an absolute script path instead of relying on plugin-local hook discovery.
- The hook is a guardrail, not a security boundary. Codex hooks do not intercept every possible shell or tool path in every runtime.
- Project config lookup is based on the Python process working directory. For
  plugin hosts that start MCP from another directory, use
  `STRUCTURED_ARTIFACT_VIEWER_CONFIG` or an explicit MCP `config` arg.
- JSON files larger than the default `--max-file-bytes` are refused unless `--force` is passed. This avoids accidental giant `json.load()` calls.
- JSONL projection skips oversize physical lines by default. Increase `--max-record-bytes` when intentionally inspecting large records.
- Parquet schema/row-group details use optional `pyarrow` when available; the
  standard-library fallback reports footer/magic metadata only.
- The MCP schema intentionally does not enumerate every CLI flag. Use `op:
  sniff`, `op: summary`, and `op: select` as the stable interface.

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
- generic `summary`/`select` dispatch
- Parquet footer summary
- YAML/YML guard exclusion
- MCP `initialize`, `tools/list`, and `tools/call`
- external budget config defaults and CLI override priority
