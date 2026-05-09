# Structured Artifact Viewer for Codex

A local Codex plugin that reduces context waste when inspecting structured artifacts:

- JSON
- JSONL / NDJSON
- CSV / TSV
- Parquet metadata
- tool artifact files such as `summary.json`, `verification.json`, `candidate_scores.jsonl`

It bundles:

1. A compact CLI: `scripts/codex_view.py`
2. A short Codex skill: `skills/structured-artifact-inspection/SKILL.md`
3. An optional repo/user hook installer that blocks high-risk raw structured-file dumps.

It is intentionally only a compact viewer. It is not a search tool, data
transformation tool, validator, editor, or `jq` replacement. Use `jq` when you
need exact JSON querying or transformation; use this plugin when the goal is to
understand shape, sizes, keys, columns, and selected fields without raw dumps.

## Plugin layout

```text
structured-artifact-viewer/
  .codex-plugin/plugin.json
  skills/structured-artifact-inspection/SKILL.md
  scripts/codex_view.py
  tests/test_codex_view.py
  examples/marketplace.repo.example.json
```

Codex plugin packaging requires `.codex-plugin/plugin.json`; skills live under `skills/`.

## Quick test without installing as a plugin

```bash
python scripts/codex_view.py --help
python scripts/codex_view.py install-command --scope repo
python scripts/codex_view.py sniff some_file.json
python scripts/codex_view.py summary some_file.json
python scripts/codex_view.py summary some_file.jsonl
python scripts/codex_view.py select some_file.jsonl --fields name,status,score,path --limit 10
python -m unittest discover -s tests
```

`summary` automatically dispatches JSON, JSONL/NDJSON, CSV/TSV, and Parquet
metadata. `select` previews selected fields from JSON, JSONL/NDJSON, CSV, and
TSV. Older typed commands such as `json-summary`, `jsonl-summary`,
`jsonl-project`, `csv-summary`, and `csv-project` remain available for
compatibility.

## Install as a repo-local Codex plugin

From your repository root:

```bash
mkdir -p plugins
cp -R /path/to/structured-artifact-viewer ./plugins/structured-artifact-viewer
mkdir -p .agents/plugins
cp ./plugins/structured-artifact-viewer/examples/marketplace.repo.example.json .agents/plugins/marketplace.json
```

Then restart Codex and install/enable the plugin from the repo marketplace.

## Optional hook guard

The skill and CLI are the primary stable path. Hooks add enforcement but are version/config dependent. If your Codex version/config requires it, ensure `[features].codex_hooks = true`.

After copying the plugin into a repository, run:

```bash
python plugins/structured-artifact-viewer/scripts/codex_view.py install-command --scope repo
python plugins/structured-artifact-viewer/scripts/codex_view.py install-hook --scope repo --mode deny
```

`install-command` creates `.codex/bin/codex-view`. `install-hook` writes/updates `.codex/hooks.json` with an absolute command path to this script. The hook blocks high-risk raw-output commands such as:

```bash
cat artifact.json
head -20 huge.jsonl
sed -n '1,80p' candidate_scores.jsonl
python -m json.tool summary.json
```

The hook is deliberately narrow. It avoids intercepting normal `jq` queries or
general data-processing commands; it is meant to stop accidental raw dumps.

Bypass only when raw output is intentional:

```bash
CODEX_VIEW_ALLOW_RAW=1 cat small.json
# or add: # codex-view-allow-raw
```

## Why this is not MCP

The problem is local file inspection, not remote integration. A local CLI plus skill is lighter and avoids extra MCP server context and startup complexity. The optional hook provides deterministic enforcement when supported by your Codex version/config.
