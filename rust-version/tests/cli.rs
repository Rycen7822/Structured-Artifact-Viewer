use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::parser::parse_message_type;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use structured_artifact_viewer::{dangerous_raw_structured_command, handle_mcp, run_cli, VERSION};
use tempfile::tempdir;

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[test]
fn json_summary_truncates_large_scalar() {
    let td = tempdir().unwrap();
    let p = td.path().join("status.json");
    fs::write(
        &p,
        serde_json::to_string(&json!({"stage": "x", "payload": "A".repeat(2000), "runs": [{"name": "a", "score": 1}]})).unwrap(),
    )
    .unwrap();
    let r = run_cli(
        &args(&[
            "json-summary",
            p.to_str().unwrap(),
            "--max-lines",
            "30",
            "--max-line-chars",
            "220",
        ]),
        "",
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("top_keys"));
    assert!(r.stdout.contains("runs: list len=1"));
    assert!(r.stdout.len() < 2500);
    assert!(!r.stdout.contains(&"A".repeat(500)));
}

#[test]
fn jsonl_summary_and_project_skip_large_fields() {
    let td = tempdir().unwrap();
    let p = td.path().join("candidate_scores.jsonl");
    let mut body = String::new();
    for i in 0..3 {
        body.push_str(
            &serde_json::to_string(
                &json!({"name": format!("n{}", i), "score": i, "payload": "B".repeat(5000)}),
            )
            .unwrap(),
        );
        body.push('\n');
    }
    fs::write(&p, body).unwrap();
    let r = run_cli(
        &args(&["jsonl-summary", p.to_str().unwrap(), "--scan", "3"]),
        "",
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("payload"));
    assert!(r.stdout.contains("LARGE_FIELD_NAME"));
    assert!(!r.stdout.contains(&"B".repeat(300)));

    let r = run_cli(
        &args(&[
            "jsonl-project",
            p.to_str().unwrap(),
            "--fields",
            "name,payload,score",
            "--limit",
            "1",
        ]),
        "",
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("skipped_large_fields"));
    assert!(!r.stdout.contains(&"B".repeat(100)));
}

#[test]
fn csv_summary_project_and_guard() {
    let td = tempdir().unwrap();
    let p = td.path().join("results.csv");
    fs::write(
        &p,
        format!("name,status,score,logs\na,ok,0.9,{}\n", "D".repeat(1000)),
    )
    .unwrap();
    let r = run_cli(
        &args(&["csv-summary", p.to_str().unwrap(), "--scan", "1"]),
        "",
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("columns(4)"));
    assert!(r.stdout.contains("logs"));
    assert!(r.stdout.contains("LARGE_FIELD_NAME"));

    let r = run_cli(
        &args(&[
            "csv-project",
            p.to_str().unwrap(),
            "--fields",
            "name,status,score,logs",
        ]),
        "",
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("skipped_large_fields"));
    assert!(!r.stdout.contains(&"D".repeat(100)));

    assert!(
        dangerous_raw_structured_command("python -m json.tool summary.json", None, 16 * 1024).0
    );
    assert!(dangerous_raw_structured_command("bash -lc 'head -20 huge.jsonl'", None, 16 * 1024).0);
    assert!(
        !dangerous_raw_structured_command("CODEX_VIEW_ALLOW_RAW=1 cat data.jsonl", None, 16 * 1024)
            .0
    );
}

#[test]
fn oversize_jsonl_config_and_parquet_footer() {
    let td = tempdir().unwrap();
    let p = td.path().join("huge.jsonl");
    fs::write(
        &p,
        format!(
            "{}\n{}\n",
            serde_json::to_string(&json!({"payload": "Z".repeat(20000)})).unwrap(),
            serde_json::to_string(&json!({"name": "ok", "score": 1})).unwrap()
        ),
    )
    .unwrap();
    let r = run_cli(
        &args(&[
            "jsonl-summary",
            p.to_str().unwrap(),
            "--max-record-bytes",
            "1000",
            "--line-check",
            "2",
            "--max-line-probe-bytes",
            "1000",
        ]),
        "",
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("oversize_records_skipped"));
    assert!(r.stdout.contains("name"));
    assert!(!r.stdout.contains(&"Z".repeat(100)));

    let config = td.path().join("viewer.toml");
    fs::write(&config, "[budget]\nscan = 1\nmax_lines = 12\n").unwrap();
    let rows = td.path().join("rows.jsonl");
    fs::write(&rows, "{\"name\":\"a\"}\n{\"name\":\"b\"}\n").unwrap();
    let r = run_cli(
        &args(&[
            "--config",
            config.to_str().unwrap(),
            "jsonl-summary",
            rows.to_str().unwrap(),
        ]),
        "",
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("records_scanned: 1+"));

    let pq = td.path().join("sample.parquet");
    fs::write(
        &pq,
        [b"PAR1".as_slice(), &0u32.to_le_bytes(), b"PAR1"].concat(),
    )
    .unwrap();
    let r = run_cli(&args(&["parquet-summary", pq.to_str().unwrap()]), "");
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("parquet_magic_ok: true"));
    assert!(r.stdout.contains("footer_length_bytes: 0"));
}

#[test]
fn mcp_smoke() {
    let response =
        handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}))
            .unwrap();
    let tools = response["result"]["tools"].as_array().unwrap();
    assert_eq!(tools[0]["name"], "structured_artifact_viewer");
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["op"]));

    let response =
        handle_mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "initialize", "params": {}}))
            .unwrap();
    assert_eq!(response["result"]["serverInfo"]["version"], VERSION);
}

#[test]
fn small_existing_json_is_not_guarded() {
    let td = tempdir().unwrap();
    fs::write(td.path().join("package.json"), "{\"name\":\"demo\"}").unwrap();
    let cwd = td.path().to_str().unwrap();
    assert!(!dangerous_raw_structured_command("cat package.json", Some(cwd), 1024).0);
    fs::write(
        td.path().join("big.json"),
        format!("{{\"payload\":\"{}\"}}", "X".repeat(5000)),
    )
    .unwrap();
    assert!(dangerous_raw_structured_command("cat big.json", Some(cwd), 1024).0);
    assert!(!dangerous_raw_structured_command("cat config.yml", None, 1024).0);
    assert!(!dangerous_raw_structured_command("cat config.yaml", None, 1024).0);
    assert!(Path::new(cwd).exists());
}

#[test]
fn valid_parquet_metadata_is_reported() {
    let td = tempdir().unwrap();
    let pq = td.path().join("valid.parquet");
    let schema = Arc::new(
        parse_message_type(
            "
            message schema {
              REQUIRED INT32 id;
              OPTIONAL BYTE_ARRAY name (STRING);
            }
            ",
        )
        .unwrap(),
    );
    let file = fs::File::create(&pq).unwrap();
    let props = Arc::new(WriterProperties::builder().build());
    let writer = SerializedFileWriter::new(file, schema, props).unwrap();
    writer.close().unwrap();

    let r = run_cli(&args(&["parquet-summary", pq.to_str().unwrap()]), "");
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("rows: 0"));
    assert!(r.stdout.contains("row_groups: 0"));
    assert!(r.stdout.contains("columns: 2"));
    assert!(r.stdout.contains("column_names(2): ['id', 'name']"));
}
