# Structured Artifact Viewer Rust Version

This directory contains the Rust migration of the Python Structured Artifact
Viewer implementation.

## Binaries

```bash
cargo build --release
target/release/codex-view --help
target/release/codex-view summary FILE
target/release/codex-view select FILE --fields name,status,score
target/release/structured-artifact-mcp-server
```

The Rust CLI keeps the Python command surface:

- `sniff`
- `summary`
- `select`
- `json-summary`
- `jsonl-summary`
- `jsonl-project`
- `csv-summary`
- `csv-project`
- `parquet-summary`
- `guard`
- `install-hook`
- `install-command`

The MCP server exposes the same single tool:

```text
structured_artifact_viewer
```

with `op = sniff | summary | select | self_check`.

## Config

The Rust version reads the same numeric budget config as the Python version:

```text
CLI flags > --config FILE / MCP args.config > STRUCTURED_ARTIFACT_VIEWER_CONFIG > .codex/structured-artifact-viewer.toml > ~/.config/structured-artifact-viewer/config.toml > built-in defaults
```

Config remains limited to budget fields. It does not enable `--force`.

## Verification

Run:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build
python3 scripts/compare_python.py
```

`scripts/compare_python.py` runs representative Python-vs-Rust parity checks
for JSON, JSONL, CSV, Parquet fallback, config, guard, MCP `tools/list`, MCP
`tools/call`, and generic dispatch.
