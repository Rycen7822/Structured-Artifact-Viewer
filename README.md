<div align="center">

# Structured Artifact Viewer

**Low-context JSON / JSONL / CSV artifact viewer for Codex that avoids raw dumps and context waste.**

Inspect unknown-size, large, generated, or high-output structured artifacts
before `cat`, `head`, `sed`, or pretty-printers flood the model context.

</div>

<br/>

<p align="center">
  <a href="README.md"><img src="https://img.shields.io/badge/Docs-README-f5c542?style=for-the-badge" alt="Docs"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-2ea44f?style=for-the-badge" alt="License"></a>
  <a href="https://github.com/Rycen7822/Structured-Artifact-Viewer/releases"><img src="https://img.shields.io/badge/Download-Releases-0969da?style=for-the-badge" alt="Releases"></a>
  <a href=".mcp.json"><img src="https://img.shields.io/badge/MCP-single%20tool-2ea44f?style=for-the-badge" alt="MCP"></a>
  <a href="rust-version/README.md"><img src="https://img.shields.io/badge/Runtime-Python%20%7C%20Rust-blue?style=for-the-badge" alt="Runtime"></a>
</p>

> Structured Artifact Viewer is meant for unknown-size, large, generated, or
> high-output structured artifacts. For small known config files, direct bounded
> reads are usually simpler. For exact JSON queries or transformations, use
> `jq`; this plugin is a compact inspection layer, not a `jq` replacement.

## Why

Codex often wastes context when it inspects structured artifacts with raw shell
commands:

- `cat verification.json` can dump tens of thousands of lines.
- `head -20 candidate_scores.jsonl` can still print huge records.
- `python -m json.tool status.json` can expand compact JSON into repeated large output.
- CSV, TSV, and generated tool artifacts often need shape and selected fields,
  not the full payload.

This plugin keeps inspection staged and bounded: identify the file shape, inspect
metadata and previews, then select only the fields or records that matter.

## Features

- Single MCP tool, `structured_artifact_viewer`, to keep Codex tool context small.
- Compact CLI fallback in `scripts/codex_view.py`.
- Python implementation for easy local use and plugin packaging.
- Rust implementation for native binaries and lower runtime overhead.
- Budgeted summaries for JSON, JSONL / NDJSON, CSV / TSV, and Parquet metadata.
- Field selection previews for JSON, JSONL / NDJSON, CSV, and TSV.
- External budget config for prebuilt or copied installations.
- Optional hook guard to block accidental raw dumps of high-risk structured files.

## Install

| Variant | Best for | Runtime requirements |
| --- | --- | --- |
| Rust prebuilt package | Normal use and distribution on supported Linux hosts | Linux x86_64 |
| Python version | Editing or debugging the source implementation | Python 3.10+ |
| Rust from source | Development and platform-specific builds | Rust toolchain |

### Agent Self-Install Prompts

Copy the matching prompt into a Codex agent when you want it to install the
plugin for itself.

Rust prebuilt package, recommended for normal Linux x86_64 use:

```text
Install Structured Artifact Viewer for yourself globally from repo `https://github.com/Rycen7822/Structured-Artifact-Viewer`. Use the Rust prebuilt asset `https://github.com/Rycen7822/Structured-Artifact-Viewer/releases/download/v0.2.1/structured-artifact-viewer-v0.2.1-linux-x86_64.zip`, install it under `~/.codex/plugins/structured-artifact-viewer`, enable `structured-artifact-viewer@local-personal`, register MCP `structured-artifact-viewer` to `~/.codex/plugins/structured-artifact-viewer/bin/structured-artifact-mcp-server`, and verify with `codex-view --help`, `codex mcp list`, and MCP `self_check`.
```

Python version, for editing or debugging the source implementation:

```text
Install the Python/source version of Structured Artifact Viewer for yourself from repo `https://github.com/Rycen7822/Structured-Artifact-Viewer`, not the Rust prebuilt package. Put the source tree under `~/.codex/plugins/structured-artifact-viewer`, register MCP `structured-artifact-viewer` with `python3 ~/.codex/plugins/structured-artifact-viewer/scripts/structured_artifact_mcp.py`, and verify the Python CLI/MCP path.
```

Rust from source, for platform-specific builds:

```text
Build Structured Artifact Viewer from source for your platform by cloning `https://github.com/Rycen7822/Structured-Artifact-Viewer`, then running `cd rust-version && cargo build --release --bins`. Install the resulting `codex-view` and `structured-artifact-mcp-server` under `~/.codex/plugins/structured-artifact-viewer/bin`, register the Rust MCP binary, and verify.
```

### Rust Prebuilt Package

Download the current Linux x86_64 package from the GitHub release:

```bash
mkdir -p plugins
curl -L -o structured-artifact-viewer-v0.2.1-linux-x86_64.zip \
  https://github.com/Rycen7822/Structured-Artifact-Viewer/releases/download/v0.2.1/structured-artifact-viewer-v0.2.1-linux-x86_64.zip
unzip structured-artifact-viewer-v0.2.1-linux-x86_64.zip -d plugins
mkdir -p .agents/plugins
cp plugins/structured-artifact-viewer/examples/marketplace.repo.example.json .agents/plugins/marketplace.json
```

Then restart Codex and enable the plugin from the repo marketplace.

The prebuilt archive contains the Rust CLI and MCP server:

```bash
plugins/structured-artifact-viewer/bin/codex-view --help
plugins/structured-artifact-viewer/bin/structured-artifact-mcp-server
```

Fallback direct MCP registration:

```bash
codex mcp add structured-artifact-viewer -- /absolute/path/to/plugins/structured-artifact-viewer/bin/structured-artifact-mcp-server
```

Optional shell tools:

```bash
export PATH="/absolute/path/to/plugins/structured-artifact-viewer/bin:$PATH"
```

### Python Version

Use this path when you want the editable Python implementation or want to inspect
the source plugin directly.

```bash
git clone https://github.com/Rycen7822/Structured-Artifact-Viewer.git
cd Structured-Artifact-Viewer
python3 scripts/codex_view.py --help
```

Install the source plugin into a project marketplace:

```bash
mkdir -p plugins
cp -R /path/to/Structured-Artifact-Viewer plugins/structured-artifact-viewer
mkdir -p .agents/plugins
cp plugins/structured-artifact-viewer/examples/marketplace.repo.example.json .agents/plugins/marketplace.json
```

Fallback direct MCP registration:

```bash
codex mcp add structured-artifact-viewer -- python3 /absolute/path/to/structured-artifact-viewer/scripts/structured_artifact_mcp.py
```

Quick local checks:

```bash
python3 scripts/codex_view.py summary some_file.jsonl
python3 scripts/codex_view.py select some_file.jsonl --fields name,status,score --limit 10
printf '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}\n' | python3 scripts/structured_artifact_mcp.py
```

### Rust From Source

Build the Rust binaries locally:

```bash
cd rust-version
cargo build --release --bins
target/release/codex-view --help
target/release/codex-view summary ../some_file.jsonl
target/release/structured-artifact-mcp-server
```

See [rust-version/README.md](rust-version/README.md) for Rust parity and
verification details.

## How It Works

Use the plugin as a staged inspection flow:

1. `sniff` detects file type, size, and risk signals.
2. `summary` reports bounded shape, keys, columns, sample records, and truncation notes.
3. `select` previews specific fields or columns from bounded records.
4. Use direct reads, `jq`, or domain tools only after the relevant path or field is known.

The goal is to spend terminal output on decisions, not on full artifact payloads.

## MCP Operations

The plugin exposes one MCP tool:

```text
structured_artifact_viewer
```

Input shape:

```json
{"op": "sniff|summary|select|self_check", "args": {"path": "FILE"}}
```

| Operation | Purpose |
| --- | --- |
| `sniff` | Detect type, size, extension, and whether raw output is risky. |
| `summary` | Return bounded metadata, keys, columns, examples, and truncation status. |
| `select` | Preview selected JSON fields or table columns with a record limit. |
| `self_check` | Verify the tool can run in the current environment. |

Examples:

```json
{"op": "summary", "args": {"path": "candidate_scores.jsonl"}}
{"op": "select", "args": {"path": "candidate_scores.jsonl", "fields": ["name", "status", "score"], "limit": 10}}
```

The schema intentionally stays compact instead of listing every CLI flag. The
stable agent path is `sniff` -> `summary` -> `select`.

## CLI

The generic commands automatically dispatch across JSON, JSONL / NDJSON, CSV /
TSV, and Parquet metadata:

```bash
python scripts/codex_view.py sniff some_file.json
python scripts/codex_view.py summary some_file.json
python scripts/codex_view.py summary some_file.jsonl
python scripts/codex_view.py select some_file.jsonl --fields name,status,score,path --limit 10
```

Compatibility commands remain available:

```text
json-summary
jsonl-summary
jsonl-project
csv-summary
csv-project
parquet-summary
guard
install-hook
install-command
```

## Budget Configuration

The CLI and MCP server can load default limit values from external config files.
Command-line flags still have the highest priority.

Priority:

```text
CLI flags > --config FILE / MCP args.config > STRUCTURED_ARTIFACT_VIEWER_CONFIG > .codex/structured-artifact-viewer.toml > ~/.config/structured-artifact-viewer/config.toml > built-in defaults
```

Example:

```toml
[budget]
max_lines = 80
max_line_chars = 240
max_preview = 160
max_file_bytes = 52428800
scan = 1000
sample = 5
line_check = 20
limit = 10
max_line_probe_bytes = 1048576
max_record_bytes = 5242880
max_keys = 60
max_columns = 40

[guard]
allow_small_bytes = 16384
```

The config is intentionally limited to budget values. It does not enable
`--force` or other behavior-changing flags. JSON config files are also accepted;
TOML is the recommended format.

## Optional Hook Guard

The MCP tool and CLI are the primary stable paths. Hooks add enforcement when
your Codex version/config supports them. If required, enable:

```toml
[features]
codex_hooks = true
```

After copying the plugin into a repository, run:

```bash
plugins/structured-artifact-viewer/bin/codex-view install-command --scope repo
plugins/structured-artifact-viewer/bin/codex-view install-hook --scope repo --mode deny
```

The hook blocks high-risk raw-output commands such as:

```bash
cat artifact.json
head -20 huge.jsonl
sed -n '1,80p' candidate_scores.jsonl
python -m json.tool summary.json
```

The hook is deliberately narrow. It avoids intercepting normal `jq` queries or
general data-processing commands. Bypass only when raw output is intentional:

```bash
CODEX_VIEW_ALLOW_RAW=1 cat small.json
# or add: # codex-view-allow-raw
```

## Project Layout

| Path | Purpose |
| --- | --- |
| `.codex-plugin/plugin.json` | Codex plugin manifest and user-facing metadata. |
| `.mcp.json` | MCP server registration for the single tool. The source plugin uses Python; the prebuilt package points to the Rust binary. |
| `bin/codex-view` | CLI entrypoint wrapper in source installs, Rust binary in prebuilt packages. |
| `bin/structured-artifact-mcp-server` | MCP entrypoint wrapper in source installs, Rust binary in prebuilt packages. |
| `skills/structured-artifact-inspection/SKILL.md` | Short Codex usage guidance. |
| `scripts/codex_view.py` | Python CLI and core inspection logic. |
| `scripts/structured_artifact_mcp.py` | Python MCP stdio server. |
| `rust-version/` | Rust CLI / MCP implementation and parity checks. |
| `tests/` | Python tests. |
| `examples/` | Repo marketplace example. |

## Development

Python checks:

```bash
python -m unittest discover -s tests
```

Rust checks:

```bash
cd rust-version
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build
python3 scripts/compare_python.py
```

## Notes

- Use this plugin before reading structured artifacts whose size or record shape
  is unknown.
- Keep using `jq` for exact JSON queries, filters, and transformations.
- Small known files do not need the plugin by default.
- The hook is a guardrail against accidental output floods, not a security boundary.
