# Rust Migration Audit

## Objective

Migrate Structured Artifact Viewer core code to Rust under:

```text
/home/xu/project/tools/structured-artifact-viewer/rust-version
```

The Rust implementation must preserve Python behavior and output for the
plugin's functional surface.

## Deliverable Checklist

| Requirement | Rust artifact | Evidence |
| --- | --- | --- |
| Rust work saved under `rust-version` | `Cargo.toml`, `src/`, `tests/`, `scripts/` | `cargo test` runs from this directory |
| CLI migrated | `src/main.rs`, `src/lib.rs` | `codex-view` binary built by Cargo |
| MCP migrated | `src/bin/structured_artifact_mcp_server.rs`, `handle_mcp` / `run_mcp_server` | MCP smoke test in `tests/cli.rs`; Python-vs-Rust JSON-RPC parity in `scripts/compare_python.py` |
| JSON summary/select migrated | `cmd_json_summary`, `cmd_json_select` | Rust tests and Python parity script |
| JSONL summary/project migrated | `cmd_jsonl_summary`, `cmd_jsonl_project` | Rust tests cover large fields and oversize records |
| CSV/TSV summary/project migrated | `cmd_csv_summary`, `cmd_csv_project` | Rust tests and Python parity script |
| Parquet metadata migrated | `cmd_parquet_summary` | Rust test covers valid Parquet metadata; parity script covers Python fallback case |
| Guard migrated | `dangerous_raw_structured_command`, `cmd_guard` | Rust tests and JSON-normalized Python parity |
| install-hook / install-command migrated | `cmd_install_hook`, `cmd_install_command` | Implemented in Rust CLI |
| External budget config migrated | config loader in `src/lib.rs` | Rust tests and Python parity script |
| Output budget behavior migrated | `OutputBudget` | Rust tests plus Python parity script |

## Verification Commands

Last verified with:

```text
cargo clippy --all-targets -- -D warnings
cargo test
cargo build
python3 scripts/compare_python.py
```

Observed result:

```text
cargo test: 7 passed
python3 scripts/compare_python.py: PY_RUST_COMPAT_OK
```

The root Python suite also remains green:

```text
rtk pytest -q: 18 passed
```

## Confidence Notes

The migration now covers all public Python commands and the MCP server surface.
The highest-risk differences were output formatting and JSONL scan-limit
semantics; the parity script caught and the implementation fixed the JSONL
`records_scanned: N+` mismatch. Parquet metadata was also reviewed as a gap and
implemented through the Rust `parquet` crate.

Remaining non-goal difference: the Rust CLI help text is compact rather than an
exact argparse help dump. Functional command behavior and bounded outputs are
the compatibility target.
