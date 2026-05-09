use csv::{ReaderBuilder, StringRecord};
use indexmap::IndexMap;
use parquet::file::reader::{FileReader, SerializedFileReader};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const VERSION: &str = "0.2.1";
const DEFAULT_MAX_LINES: usize = 80;
const DEFAULT_MAX_LINE_CHARS: usize = 240;
const DEFAULT_MAX_PREVIEW: usize = 160;
const DEFAULT_MAX_JSON_BYTES: u64 = 50 * 1024 * 1024;
const DEFAULT_JSONL_SCAN: usize = 1000;
const DEFAULT_JSONL_LINE_CHECK: usize = 20;
const DEFAULT_SAMPLE_LIMIT: usize = 10;
const DEFAULT_MAX_LINE_PROBE_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_JSONL_RECORD_BYTES: usize = 5 * 1024 * 1024;
const DEFAULT_GUARD_ALLOW_SMALL_BYTES: u64 = 16 * 1024;
const CONFIG_ENV: &str = "STRUCTURED_ARTIFACT_VIEWER_CONFIG";
const CONFIG_ENV_COMPAT: &str = "CODEX_VIEW_CONFIG";

const STRUCTURED_SUFFIXES: &[&str] = &["json", "jsonl", "ndjson", "csv", "tsv", "parquet"];
const STRUCTURED_BASENAME_HINTS: &[&str] = &[
    "summary.json",
    "verification.json",
    "manifest.json",
    "metadata.json",
    "metrics.json",
    "results.json",
    "report.json",
    "status.json",
    "candidate_scores.jsonl",
];
const LARGE_FIELD_NAMES: &[&str] = &[
    "payload",
    "source",
    "content",
    "prompt",
    "completion",
    "trace",
    "traces",
    "logs",
    "log",
    "stdout",
    "stderr",
    "guidance",
    "messages",
    "history",
    "response",
    "request",
    "raw",
    "blob",
    "html",
    "text",
];
const RAW_TOOLS: &[&str] = &["cat", "head", "tail", "sed", "nl"];
const BYPASS_MARKERS: &[&str] = &["CODEX_VIEW_ALLOW_RAW=1", "codex-view-allow-raw"];

#[derive(Debug, Clone)]
pub struct RunResult {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl RunResult {
    fn ok(stdout: String) -> Self {
        Self {
            code: 0,
            stdout,
            stderr: String::new(),
        }
    }

    fn err(code: i32, stderr: impl Into<String>) -> Self {
        Self {
            code,
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }

    fn out_err(code: i32, stdout: String, stderr: String) -> Self {
        Self {
            code,
            stdout,
            stderr,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Config {
    values: HashMap<String, u64>,
}

impl Config {
    fn get_usize(&self, key: &str, fallback: usize) -> usize {
        self.values
            .get(key)
            .copied()
            .map(|v| v as usize)
            .unwrap_or(fallback)
    }

    fn get_u64(&self, key: &str, fallback: u64) -> u64 {
        self.values.get(key).copied().unwrap_or(fallback)
    }
}

#[derive(Debug)]
struct ConfigError(String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

struct OutputBudget {
    max_lines: usize,
    max_line_chars: usize,
    lines: usize,
    truncated: bool,
    out: String,
}

impl OutputBudget {
    fn new(max_lines: usize, max_line_chars: usize) -> Self {
        Self {
            max_lines,
            max_line_chars,
            lines: 0,
            truncated: false,
            out: String::new(),
        }
    }

    fn emit(&mut self, text: impl ToString) {
        if self.lines >= self.max_lines {
            self.truncated = true;
            return;
        }
        let s = text.to_string();
        let mut emitted_any = false;
        for raw in s.lines() {
            emitted_any = true;
            if self.lines >= self.max_lines {
                self.truncated = true;
                return;
            }
            let line = truncate_chars(raw, self.max_line_chars);
            self.out.push_str(&line);
            self.out.push('\n');
            self.lines += 1;
        }
        if !emitted_any {
            self.out.push('\n');
            self.lines += 1;
        }
    }

    fn close(mut self) -> String {
        if self.truncated && self.lines < self.max_lines + 1 {
            self.out.push_str(&format!(
                "... output truncated by codex_view after {} lines\n",
                self.max_lines
            ));
        }
        self.out
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let keep = max_chars - 3;
    let mut out = s.chars().take(keep).collect::<String>();
    out.push_str("...");
    out
}

fn config_keys() -> HashSet<&'static str> {
    [
        "allow_small_bytes",
        "limit",
        "line_check",
        "max_chars",
        "max_columns",
        "max_file_bytes",
        "max_keys",
        "max_line_chars",
        "max_line_probe_bytes",
        "max_lines",
        "max_preview",
        "max_record_bytes",
        "sample",
        "scan",
    ]
    .into_iter()
    .collect()
}

fn normalize_config_key(key: &str) -> String {
    let normalized = key.trim().replace('-', "_");
    match normalized.as_str() {
        "guard_allow_small_bytes" => "allow_small_bytes",
        "jsonl_line_check" => "line_check",
        "jsonl_scan" => "scan",
        "max_json_bytes" => "max_file_bytes",
        "max_jsonl_record_bytes" => "max_record_bytes",
        "sample_limit" => "limit",
        _ => normalized.as_str(),
    }
    .to_string()
}

fn extract_config_arg(argv: &[String]) -> Result<(Option<String>, Vec<String>), ConfigError> {
    let mut explicit = None;
    let mut cleaned = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let item = &argv[i];
        if item == "--config" {
            if i + 1 >= argv.len() {
                return Err(ConfigError("--config requires a path".into()));
            }
            explicit = Some(argv[i + 1].clone());
            i += 2;
            continue;
        }
        if let Some(rest) = item.strip_prefix("--config=") {
            if rest.is_empty() {
                return Err(ConfigError("--config requires a path".into()));
            }
            explicit = Some(rest.to_string());
            i += 1;
            continue;
        }
        cleaned.push(item.clone());
        i += 1;
    }
    Ok((explicit, cleaned))
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn find_repo_root(start: &Path) -> PathBuf {
    let mut cur = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    loop {
        if cur.join(".git").exists() {
            return cur;
        }
        if !cur.pop() {
            break;
        }
    }
    start.to_path_buf()
}

fn candidate_config_paths(explicit: Option<&str>) -> Vec<(PathBuf, bool)> {
    let mut paths = Vec::new();
    if let Some(home) = home_dir() {
        paths.push((
            home.join(".config/structured-artifact-viewer/config.toml"),
            false,
        ));
    }
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    paths.push((
        find_repo_root(&cwd).join(".codex/structured-artifact-viewer.toml"),
        false,
    ));
    if let Ok(env_path) = env::var(CONFIG_ENV).or_else(|_| env::var(CONFIG_ENV_COMPAT)) {
        if !env_path.is_empty() {
            paths.push((PathBuf::from(env_path), true));
        }
    }
    if let Some(p) = explicit {
        paths.push((PathBuf::from(p), true));
    }
    paths
}

fn read_config_file(path: &Path) -> Result<Value, ConfigError> {
    let raw = fs::read_to_string(path)
        .map_err(|e| ConfigError(format!("cannot read config {}: {}", path.display(), e)))?;
    if path
        .extension()
        .and_then(|x| x.to_str())
        .map(|x| x.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
    {
        serde_json::from_str(&raw)
            .map_err(|e| ConfigError(format!("cannot parse config {}: {}", path.display(), e)))
    } else {
        let parsed: toml::Value = toml::from_str(&raw)
            .map_err(|e| ConfigError(format!("cannot parse config {}: {}", path.display(), e)))?;
        serde_json::to_value(parsed)
            .map_err(|e| ConfigError(format!("cannot parse config {}: {}", path.display(), e)))
    }
}

fn collect_config_values(raw: &Value, path: &Path) -> Result<HashMap<String, u64>, ConfigError> {
    let mut values = HashMap::new();
    let obj = raw.as_object().ok_or_else(|| {
        ConfigError(format!(
            "config {} must contain an object/table",
            path.display()
        ))
    })?;
    let keys = config_keys();
    for (section, body) in obj {
        let entries: Vec<(&String, &Value)> = if section == "budget" || section == "guard" {
            let table = body.as_object().ok_or_else(|| {
                ConfigError(format!(
                    "config {} section [{}] must be a table",
                    path.display(),
                    section
                ))
            })?;
            table.iter().collect()
        } else {
            vec![(section, body)]
        };
        for (raw_key, raw_value) in entries {
            let key = normalize_config_key(raw_key);
            if !keys.contains(key.as_str()) {
                return Err(ConfigError(format!(
                    "config {} has unsupported key: {}",
                    path.display(),
                    raw_key
                )));
            }
            let Some(num) = raw_value.as_i64() else {
                return Err(ConfigError(format!(
                    "config {} key {} must be an integer",
                    path.display(),
                    raw_key
                )));
            };
            if num < 0 {
                return Err(ConfigError(format!(
                    "config {} key {} must be >= 0",
                    path.display(),
                    raw_key
                )));
            }
            if key != "allow_small_bytes" && num == 0 {
                return Err(ConfigError(format!(
                    "config {} key {} must be > 0",
                    path.display(),
                    raw_key
                )));
            }
            values.insert(key, num as u64);
        }
    }
    Ok(values)
}

fn load_config_defaults(explicit: Option<&str>) -> Result<Config, ConfigError> {
    let mut cfg = Config::default();
    for (path, required) in candidate_config_paths(explicit) {
        if !path.exists() {
            if required {
                return Err(ConfigError(format!("config not found: {}", path.display())));
            }
            continue;
        }
        for (k, v) in collect_config_values(&read_config_file(&path)?, &path)? {
            cfg.values.insert(k, v);
        }
    }
    Ok(cfg)
}

fn py_string_repr(s: &str) -> String {
    let mut body = String::new();
    for ch in s.chars() {
        match ch {
            '\\' => body.push_str("\\\\"),
            '\'' => body.push_str("\\'"),
            '\n' => body.push_str("\\n"),
            '\r' => body.push_str("\\r"),
            '\t' => body.push_str("\\t"),
            _ => body.push(ch),
        }
    }
    format!("'{}'", body)
}

fn py_bool(b: bool) -> &'static str {
    if b {
        "True"
    } else {
        "False"
    }
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_i64() || n.is_u64() => "int",
        Value::Number(_) => "float",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn py_value_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(b) => py_bool(*b).to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => py_string_repr(s),
        Value::Array(items) => {
            let parts = items.iter().map(py_value_repr).collect::<Vec<_>>();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(map) => {
            let parts = map
                .iter()
                .map(|(k, v)| format!("{}: {}", py_string_repr(k), py_value_repr(v)))
                .collect::<Vec<_>>();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

fn py_list_str(items: &[String]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|s| py_string_repr(s))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn py_list_display(items: &[String]) -> String {
    format!("[{}]", items.join(", "))
}

fn py_ordered_counts(counts: &IndexMap<String, usize>) -> String {
    let parts = counts
        .iter()
        .map(|(k, v)| format!("{}: {}", py_string_repr(k), v))
        .collect::<Vec<_>>();
    format!("{{{}}}", parts.join(", "))
}

fn preview(value: &Value, max_chars: usize) -> String {
    let raw = match value {
        Value::String(s) => py_string_repr(s),
        Value::Object(map) => {
            let keys = map.keys().take(10).cloned().collect::<Vec<_>>();
            format!("<dict len={} keys={}>", map.len(), py_list_str(&keys))
        }
        Value::Array(items) => {
            if let Some(first) = items.first() {
                let child_limit = (max_chars / 2).clamp(40, 120);
                format!(
                    "<list len={} first={}>",
                    items.len(),
                    preview(first, child_limit)
                )
            } else {
                "<list len=0>".to_string()
            }
        }
        _ => py_value_repr(value),
    }
    .replace('\n', "\\n");
    truncate_chars(&raw, max_chars)
}

fn approx_repr_len(value: &Value, cap: usize, depth: usize) -> usize {
    match value {
        Value::Null => 4,
        Value::Bool(b) => py_bool(*b).len(),
        Value::Number(n) => n.to_string().len(),
        Value::String(s) => s.chars().count() + 2,
        Value::Object(map) => {
            let mut total = 2usize;
            for (i, (k, v)) in map.iter().enumerate() {
                if i >= 20 || depth >= 3 || total >= cap {
                    total += map.len().saturating_sub(i) * 8;
                    break;
                }
                total += approx_repr_len(
                    &Value::String(k.clone()),
                    cap.saturating_sub(total),
                    depth + 1,
                );
                total += approx_repr_len(v, cap.saturating_sub(total), depth + 1);
                total += 4;
            }
            total.min(cap)
        }
        Value::Array(items) => {
            let mut total = 2usize;
            for (i, item) in items.iter().enumerate() {
                if i >= 20 || depth >= 3 || total >= cap {
                    total += items.len().saturating_sub(i) * 4;
                    break;
                }
                total += approx_repr_len(item, cap.saturating_sub(total), depth + 1) + 2;
            }
            total.min(cap)
        }
    }
}

fn shorten_value(value: &Value, max_chars: usize) -> Value {
    match value {
        Value::String(s) => Value::String(truncate_chars(s, max_chars)),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        _ => Value::String(preview(value, max_chars)),
    }
}

fn parse_fields(raw: &str) -> Result<Vec<String>, String> {
    let fields = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if fields.is_empty() {
        Err("--fields must include at least one field".into())
    } else {
        Ok(fields)
    }
}

fn get_nested<'a>(record: &'a Value, dotted: &str) -> Option<&'a Value> {
    let mut cur = record;
    for part in dotted.split('.') {
        match cur {
            Value::Object(map) => {
                cur = map.get(part)?;
            }
            _ => return None,
        }
    }
    Some(cur)
}

fn is_large_field(field: &str) -> bool {
    let names = LARGE_FIELD_NAMES.iter().copied().collect::<HashSet<_>>();
    field
        .replace('[', ".")
        .replace(']', "")
        .split('.')
        .filter(|p| !p.is_empty())
        .any(|p| names.contains(p.to_lowercase().as_str()))
}

fn summarize_json_value(
    label: &str,
    value: &Value,
    out: &mut OutputBudget,
    max_preview: usize,
    max_keys: usize,
) {
    match value {
        Value::Object(map) => {
            let keys = map.keys().take(max_keys).cloned().collect::<Vec<_>>();
            let suffix = if map.len() > max_keys { " ..." } else { "" };
            out.emit(format!(
                "{}: dict len={} keys={}{}",
                label,
                map.len(),
                py_list_str(&keys),
                suffix
            ));
        }
        Value::Array(items) => {
            out.emit(format!("{}: list len={}", label, items.len()));
            if let Some(first) = items.first() {
                out.emit(format!("  first_type={}", json_type_name(first)));
                if let Value::Object(map) = first {
                    let keys = map.keys().take(max_keys).cloned().collect::<Vec<_>>();
                    let suffix = if map.len() > max_keys { " ..." } else { "" };
                    out.emit(format!("  first_keys={}{}", py_list_str(&keys), suffix));
                } else {
                    out.emit(format!("  first_preview={}", preview(first, max_preview)));
                }
            }
        }
        _ => out.emit(format!(
            "{}: {} {}",
            label,
            json_type_name(value),
            preview(value, max_preview)
        )),
    }
}

fn read_text_line_lengths(
    path: &Path,
    line_check: usize,
    max_probe_bytes: usize,
) -> io::Result<Vec<String>> {
    let mut lengths = Vec::new();
    let mut reader = BufReader::new(File::open(path)?);
    for _ in 0..line_check {
        let mut total = 0usize;
        let mut saw_any = false;
        loop {
            let remaining = max_probe_bytes.saturating_sub(total) + 1;
            let take = remaining.min(8192);
            let mut buf = Vec::new();
            let read = reader
                .by_ref()
                .take(take as u64)
                .read_until(b'\n', &mut buf)?;
            if read == 0 {
                if saw_any {
                    lengths.push(total.to_string());
                }
                return Ok(lengths);
            }
            saw_any = true;
            total += read;
            if buf.ends_with(b"\n") {
                lengths.push(total.to_string());
                break;
            }
            if total >= max_probe_bytes {
                lengths.push(format!(">={}", max_probe_bytes));
                return Ok(lengths);
            }
        }
    }
    Ok(lengths)
}

fn max_length_label(lengths: &[String]) -> String {
    if let Some(lb) = lengths.iter().find(|x| x.starts_with(">=")) {
        return lb.clone();
    }
    lengths
        .iter()
        .filter_map(|x| x.parse::<usize>().ok())
        .max()
        .unwrap_or(0)
        .to_string()
}

fn read_bounded_jsonl_line<R: BufRead>(
    reader: &mut R,
    max_record_bytes: usize,
) -> io::Result<Option<(Vec<u8>, bool)>> {
    let mut buf = Vec::new();
    let mut oversize = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if buf.is_empty() && !oversize {
                return Ok(None);
            }
            return Ok(Some((buf, oversize)));
        }
        let newline_pos = available.iter().position(|b| *b == b'\n');
        let take_len = newline_pos.map(|p| p + 1).unwrap_or(available.len());
        if !oversize {
            let remaining = max_record_bytes.saturating_add(1).saturating_sub(buf.len());
            if take_len <= remaining {
                buf.extend_from_slice(&available[..take_len]);
            } else {
                buf.extend_from_slice(&available[..remaining]);
                oversize = true;
            }
        }
        reader.consume(take_len);
        if newline_pos.is_some() {
            return Ok(Some((buf, oversize)));
        }
        if buf.len() > max_record_bytes {
            oversize = true;
        }
    }
}

fn decode_jsonl_line(line_no: usize, bytes: &[u8]) -> String {
    let mut s = String::from_utf8_lossy(bytes).to_string();
    if line_no == 1 && s.starts_with('\u{feff}') {
        s = s.trim_start_matches('\u{feff}').to_string();
    }
    s
}

fn path_suffix(path: &Path) -> String {
    path.extension()
        .and_then(|x| x.to_str())
        .map(|x| format!(".{}", x.to_lowercase()))
        .unwrap_or_default()
}

fn cmd_sniff(
    path: &Path,
    line_check: usize,
    max_line_probe_bytes: usize,
    max_lines: usize,
    max_line_chars: usize,
) -> RunResult {
    let mut out = OutputBudget::new(max_lines, max_line_chars);
    if !path.exists() {
        return RunResult::err(2, format!("not found: {}\n", path.display()));
    }
    out.emit(format!("path: {}", path.display()));
    out.emit(format!(
        "bytes: {}",
        fs::metadata(path).map(|m| m.len()).unwrap_or(0)
    ));
    let suffix = path_suffix(path);
    out.emit(format!(
        "suffix: {}",
        if suffix.is_empty() {
            "<none>".to_string()
        } else {
            suffix
        }
    ));
    if path.is_file() {
        match read_text_line_lengths(path, line_check.min(20), max_line_probe_bytes) {
            Ok(lengths) if !lengths.is_empty() => {
                out.emit(format!("first_line_lengths: {}", py_list_display(&lengths)));
                out.emit(format!(
                    "max_first_line_length: {}",
                    max_length_label(&lengths)
                ));
            }
            Ok(_) => {}
            Err(e) => return RunResult::err(1, format!("sniff read error: {}\n", e)),
        }
    }
    RunResult::ok(out.close())
}

fn read_json(path: &Path) -> Result<Value, String> {
    let mut raw = fs::read(path).map_err(|e| format!("json parse error: {}", e))?;
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        raw.drain(..3);
    }
    let text = String::from_utf8_lossy(&raw);
    serde_json::from_str(&text).map_err(|e| format!("json parse error: {}", e))
}

fn cmd_json_summary(
    path: &Path,
    max_preview: usize,
    max_keys: usize,
    max_file_bytes: u64,
    force: bool,
    max_lines: usize,
    max_line_chars: usize,
) -> RunResult {
    let mut out = OutputBudget::new(max_lines, max_line_chars);
    if !path.exists() {
        return RunResult::err(2, format!("not found: {}\n", path.display()));
    }
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    out.emit(format!("path: {}", path.display()));
    out.emit(format!("bytes: {}", size));
    if size > max_file_bytes && !force {
        out.emit(format!(
            "refused_to_load: file exceeds --max-file-bytes={}; pass --force if intentional",
            max_file_bytes
        ));
        return RunResult::out_err(1, out.close(), String::new());
    }
    let obj = match read_json(path) {
        Ok(v) => v,
        Err(e) => return RunResult::err(1, format!("{}\n", e)),
    };
    out.emit(format!("type: {}", json_type_name(&obj)));
    match &obj {
        Value::Object(map) => {
            let keys = map.keys().take(max_keys).cloned().collect::<Vec<_>>();
            let suffix = if map.len() > max_keys { " ..." } else { "" };
            out.emit(format!("top_keys: {}{}", py_list_str(&keys), suffix));
            for (k, v) in map {
                summarize_json_value(k, v, &mut out, max_preview, max_keys);
            }
        }
        Value::Array(items) => {
            out.emit(format!("list_len: {}", items.len()));
            if let Some(first) = items.first() {
                summarize_json_value("first", first, &mut out, max_preview, max_keys);
            }
        }
        _ => out.emit(preview(&obj, max_preview)),
    }
    RunResult::ok(out.close())
}

fn skipped_and_effective(
    fields: &[String],
    include_large_fields: bool,
) -> (Vec<String>, Vec<String>) {
    if include_large_fields {
        return (Vec::new(), fields.to_vec());
    }
    let skipped = fields
        .iter()
        .filter(|f| is_large_field(f))
        .cloned()
        .collect::<Vec<_>>();
    let effective = fields
        .iter()
        .filter(|f| !skipped.iter().any(|s| s == *f))
        .cloned()
        .collect::<Vec<_>>();
    (skipped, effective)
}

fn emit_skipped(out: &mut OutputBudget, skipped: &[String]) {
    if !skipped.is_empty() {
        out.emit(format!(
            "skipped_large_fields: {}; pass --include-large-fields if intentional",
            py_list_str(skipped)
        ));
    }
}

fn json_sorted_line(pairs: Vec<(String, Value)>) -> String {
    let mut sorted = BTreeMap::new();
    for (k, v) in pairs {
        sorted.insert(k, v);
    }
    serde_json::to_string(&sorted)
        .unwrap_or_else(|_| "{}".to_string())
        .replace("\":", "\": ")
        .replace(",", ", ")
}

fn py_pairs_dict(pairs: &[(String, Value)]) -> String {
    let parts = pairs
        .iter()
        .map(|(k, v)| format!("{}: {}", py_string_repr(k), py_value_repr(v)))
        .collect::<Vec<_>>();
    format!("{{{}}}", parts.join(", "))
}

#[allow(clippy::too_many_arguments)]
fn cmd_json_select(
    path: &Path,
    fields_raw: &str,
    limit: usize,
    max_chars: usize,
    max_file_bytes: u64,
    force: bool,
    include_large_fields: bool,
    include_empty: bool,
    as_json: bool,
    max_lines: usize,
    max_line_chars: usize,
) -> RunResult {
    let fields = match parse_fields(fields_raw) {
        Ok(f) => f,
        Err(e) => return RunResult::err(2, format!("{}\n", e)),
    };
    let mut out = OutputBudget::new(max_lines, max_line_chars);
    if !path.exists() {
        return RunResult::err(2, format!("not found: {}\n", path.display()));
    }
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size > max_file_bytes && !force {
        out.emit(format!(
            "refused_to_load: file exceeds --max-file-bytes={}; pass --force if intentional",
            max_file_bytes
        ));
        return RunResult::out_err(1, out.close(), String::new());
    }
    let (skipped, effective) = skipped_and_effective(&fields, include_large_fields);
    emit_skipped(&mut out, &skipped);
    if effective.is_empty() {
        out.emit("no_effective_fields: all requested fields are known large fields");
        return RunResult::out_err(1, out.close(), String::new());
    }
    let obj = match read_json(path) {
        Ok(v) => v,
        Err(e) => return RunResult::err(1, format!("{}\n", e)),
    };

    let emit_row = |label: String, record: &Value, out: &mut OutputBudget| -> bool {
        let mut pairs = Vec::new();
        for field in &effective {
            if let Some(value) = get_nested(record, field) {
                pairs.push((field.clone(), shorten_value(value, max_chars)));
            }
        }
        if pairs.is_empty() && !include_empty {
            return false;
        }
        if as_json {
            let mut json_pairs = vec![("item".to_string(), Value::String(label.clone()))];
            json_pairs.extend(pairs.clone());
            out.emit(json_sorted_line(json_pairs));
        } else {
            out.emit(format!("{}: {}", label, py_pairs_dict(&pairs)));
        }
        true
    };

    match &obj {
        Value::Object(_) => {
            emit_row("root".to_string(), &obj, &mut out);
        }
        Value::Array(items) => {
            let mut emitted = 0usize;
            for (i, item) in items.iter().enumerate() {
                if emitted >= limit {
                    break;
                }
                if item.is_object() {
                    if emit_row(i.to_string(), item, &mut out) {
                        emitted += 1;
                    }
                } else if include_empty {
                    out.emit(format!("{}: {}", i, preview(item, max_chars)));
                    emitted += 1;
                }
            }
        }
        _ => {
            out.emit(format!(
                "unsupported_json_type: {}; select expects an object or list of objects",
                json_type_name(&obj)
            ));
            return RunResult::out_err(1, out.close(), String::new());
        }
    }
    RunResult::ok(out.close())
}

fn update_count(map: &mut IndexMap<String, usize>, key: String) {
    let entry = map.entry(key).or_insert(0);
    *entry += 1;
}

#[allow(clippy::too_many_arguments)]
fn cmd_jsonl_summary(
    path: &Path,
    scan: usize,
    line_check: usize,
    max_line_probe_bytes: usize,
    max_record_bytes: usize,
    max_preview: usize,
    max_keys: usize,
    max_lines: usize,
    max_line_chars: usize,
) -> RunResult {
    let mut out = OutputBudget::new(max_lines, max_line_chars);
    if !path.exists() {
        return RunResult::err(2, format!("not found: {}\n", path.display()));
    }
    out.emit(format!("path: {}", path.display()));
    out.emit(format!(
        "bytes: {}",
        fs::metadata(path).map(|m| m.len()).unwrap_or(0)
    ));
    let lengths = match read_text_line_lengths(path, line_check, max_line_probe_bytes) {
        Ok(v) => v,
        Err(e) => return RunResult::err(1, format!("jsonl read error: {}\n", e)),
    };
    out.emit(format!("first_line_lengths: {}", py_list_display(&lengths)));
    if !lengths.is_empty() {
        out.emit(format!(
            "max_first_line_length: {}",
            max_length_label(&lengths)
        ));
    }

    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => return RunResult::err(1, format!("jsonl read error: {}\n", e)),
    };
    let mut reader = BufReader::new(file);
    let mut key_counter: IndexMap<String, usize> = IndexMap::new();
    let mut type_counter: IndexMap<String, IndexMap<String, usize>> = IndexMap::new();
    let mut max_repr_len: IndexMap<String, usize> = IndexMap::new();
    let mut examples: IndexMap<String, String> = IndexMap::new();
    let mut record_type_counter: IndexMap<String, usize> = IndexMap::new();
    let mut scanned = 0usize;
    let mut bad = 0usize;
    let mut oversize = 0usize;
    let mut total_estimated = false;
    let mut line_no = 0usize;

    loop {
        if scanned >= scan {
            match read_bounded_jsonl_line(&mut reader, max_record_bytes) {
                Ok(Some(_)) => total_estimated = true,
                Ok(None) => {}
                Err(e) => return RunResult::err(1, format!("jsonl read error: {}\n", e)),
            }
            break;
        }
        let Some((bytes, is_oversize)) =
            (match read_bounded_jsonl_line(&mut reader, max_record_bytes) {
                Ok(v) => v,
                Err(e) => return RunResult::err(1, format!("jsonl read error: {}\n", e)),
            })
        else {
            break;
        };
        line_no += 1;
        scanned += 1;
        if is_oversize {
            oversize += 1;
            continue;
        }
        let line = decode_jsonl_line(line_no, &bytes);
        if line.trim().is_empty() {
            continue;
        }
        let rec: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                bad += 1;
                continue;
            }
        };
        update_count(&mut record_type_counter, json_type_name(&rec).to_string());
        let Value::Object(map) = rec else {
            continue;
        };
        for (k, v) in map {
            update_count(&mut key_counter, k.clone());
            update_count(
                type_counter.entry(k.clone()).or_default(),
                json_type_name(&v).to_string(),
            );
            let rlen = approx_repr_len(&v, 1_000_000, 0);
            if rlen > *max_repr_len.get(&k).unwrap_or(&0) {
                max_repr_len.insert(k.clone(), rlen);
                examples.insert(k, preview(&v, max_preview));
            }
        }
    }
    out.emit(format!(
        "records_scanned: {}{}",
        scanned,
        if total_estimated { "+" } else { "" }
    ));
    out.emit(format!("bad_json_scanned: {}", bad));
    if oversize > 0 {
        out.emit(format!(
            "oversize_records_skipped: {} (>{} bytes each)",
            oversize, max_record_bytes
        ));
    }
    out.emit(format!(
        "record_types: {}",
        py_ordered_counts(&record_type_counter)
    ));
    out.emit("top_keys:");
    let mut keys = key_counter.iter().collect::<Vec<_>>();
    keys.sort_by(|a, b| b.1.cmp(a.1));
    for (k, count) in keys.into_iter().take(max_keys) {
        let marker = if is_large_field(k) {
            " LARGE_FIELD_NAME"
        } else {
            ""
        };
        let types = type_counter
            .get(k)
            .map(py_ordered_counts)
            .unwrap_or_else(|| "{}".to_string());
        let rlen = max_repr_len.get(k).copied().unwrap_or(0);
        let sample = examples.get(k).cloned().unwrap_or_default();
        out.emit(format!(
            "  {}: count={} types={} max_repr_len={} sample={}{}",
            k, count, types, rlen, sample, marker
        ));
    }
    RunResult::ok(out.close())
}

#[allow(clippy::too_many_arguments)]
fn cmd_jsonl_project(
    path: &Path,
    fields_raw: &str,
    limit: usize,
    max_chars: usize,
    max_record_bytes: usize,
    include_large_fields: bool,
    include_empty: bool,
    as_json: bool,
    max_lines: usize,
    max_line_chars: usize,
) -> RunResult {
    let fields = match parse_fields(fields_raw) {
        Ok(f) => f,
        Err(e) => return RunResult::err(2, format!("{}\n", e)),
    };
    let mut out = OutputBudget::new(max_lines, max_line_chars);
    if !path.exists() {
        return RunResult::err(2, format!("not found: {}\n", path.display()));
    }
    let (skipped, effective) = skipped_and_effective(&fields, include_large_fields);
    emit_skipped(&mut out, &skipped);
    if effective.is_empty() {
        out.emit("no_effective_fields: all requested fields are known large fields");
        return RunResult::out_err(1, out.close(), String::new());
    }
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => return RunResult::err(1, format!("jsonl project error: {}\n", e)),
    };
    let mut reader = BufReader::new(file);
    let mut emitted = 0usize;
    let mut bad = 0usize;
    let mut oversize = 0usize;
    let mut line_no = 0usize;
    loop {
        if emitted >= limit {
            break;
        }
        let Some((bytes, is_oversize)) =
            (match read_bounded_jsonl_line(&mut reader, max_record_bytes) {
                Ok(v) => v,
                Err(e) => return RunResult::err(1, format!("jsonl project error: {}\n", e)),
            })
        else {
            break;
        };
        line_no += 1;
        if is_oversize {
            oversize += 1;
            continue;
        }
        let line = decode_jsonl_line(line_no, &bytes);
        if line.trim().is_empty() {
            continue;
        }
        let rec: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                bad += 1;
                continue;
            }
        };
        if !rec.is_object() {
            out.emit(format!("{}: {}", line_no, preview(&rec, max_chars)));
            emitted += 1;
            continue;
        }
        let mut pairs = Vec::new();
        for field in &effective {
            if let Some(value) = get_nested(&rec, field) {
                pairs.push((field.clone(), shorten_value(value, max_chars)));
            }
        }
        if !pairs.is_empty() || include_empty {
            if as_json {
                let mut json_pairs = vec![("line".to_string(), json!(line_no))];
                json_pairs.extend(pairs.clone());
                out.emit(json_sorted_line(json_pairs));
            } else {
                out.emit(format!("{}: {}", line_no, py_pairs_dict(&pairs)));
            }
            emitted += 1;
        }
    }
    if bad > 0 {
        out.emit(format!("bad_json_skipped: {}", bad));
    }
    if oversize > 0 {
        out.emit(format!(
            "oversize_records_skipped: {} (>{} bytes each)",
            oversize, max_record_bytes
        ));
    }
    RunResult::ok(out.close())
}

fn detect_delimiter(path: &Path, explicit: Option<&str>) -> u8 {
    if let Some(d) = explicit {
        if d == "\\t" {
            return b'\t';
        }
        return d.as_bytes().first().copied().unwrap_or(b',');
    }
    if path
        .extension()
        .and_then(|x| x.to_str())
        .map(|x| x.eq_ignore_ascii_case("tsv"))
        .unwrap_or(false)
    {
        return b'\t';
    }
    let sample = fs::read(path).unwrap_or_default();
    let sample = &sample[..sample.len().min(4096)];
    let candidates = [b',', b'\t', b';', b'|'];
    candidates
        .into_iter()
        .max_by_key(|c| sample.iter().filter(|b| *b == c).count())
        .unwrap_or(b',')
}

fn delim_repr(delim: u8) -> String {
    if delim == b'\t' {
        "'\\t'".to_string()
    } else {
        py_string_repr(&(delim as char).to_string())
    }
}

fn make_csv_reader(path: &Path, delim: u8) -> Result<csv::Reader<File>, csv::Error> {
    ReaderBuilder::new()
        .delimiter(delim)
        .flexible(true)
        .from_path(path)
}

fn record_value(record: &StringRecord, idx: usize) -> String {
    record.get(idx).unwrap_or("").to_string()
}

#[allow(clippy::too_many_arguments)]
fn cmd_csv_summary(
    path: &Path,
    delimiter: Option<&str>,
    scan: usize,
    sample: usize,
    max_preview: usize,
    max_columns: usize,
    max_lines: usize,
    max_line_chars: usize,
) -> RunResult {
    let mut out = OutputBudget::new(max_lines, max_line_chars);
    if !path.exists() {
        return RunResult::err(2, format!("not found: {}\n", path.display()));
    }
    let delim = detect_delimiter(path, delimiter);
    out.emit(format!("path: {}", path.display()));
    out.emit(format!(
        "bytes: {}",
        fs::metadata(path).map(|m| m.len()).unwrap_or(0)
    ));
    out.emit(format!("delimiter: {}", delim_repr(delim)));
    let mut reader = match make_csv_reader(path, delim) {
        Ok(r) => r,
        Err(e) => return RunResult::err(1, format!("csv summary error: {}\n", e)),
    };
    let headers = match reader.headers() {
        Ok(h) => h.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        Err(e) => return RunResult::err(1, format!("csv summary error: {}\n", e)),
    };
    let head_preview = headers
        .iter()
        .take(max_columns)
        .cloned()
        .collect::<Vec<_>>();
    out.emit(format!(
        "columns({}): {}{}",
        headers.len(),
        py_list_str(&head_preview),
        if headers.len() > max_columns {
            " ..."
        } else {
            ""
        }
    ));
    let mut rows_scanned = 0usize;
    let mut nonempty_counter: IndexMap<String, usize> = IndexMap::new();
    let mut max_len: IndexMap<String, usize> = IndexMap::new();
    let mut examples: IndexMap<String, String> = IndexMap::new();
    let mut samples: Vec<Vec<(String, Value)>> = Vec::new();
    for rec in reader.records() {
        let rec = match rec {
            Ok(r) => r,
            Err(e) => return RunResult::err(1, format!("csv summary error: {}\n", e)),
        };
        rows_scanned += 1;
        if samples.len() < sample {
            let row = headers
                .iter()
                .enumerate()
                .take(max_columns)
                .map(|(i, k)| {
                    (
                        k.clone(),
                        shorten_value(&Value::String(record_value(&rec, i)), max_preview),
                    )
                })
                .collect::<Vec<_>>();
            samples.push(row);
        }
        for (i, k) in headers.iter().enumerate() {
            let v = record_value(&rec, i);
            if !v.is_empty() {
                update_count(&mut nonempty_counter, k.clone());
                let lv = v.chars().count();
                if lv > *max_len.get(k).unwrap_or(&0) {
                    max_len.insert(k.clone(), lv);
                    examples.insert(k.clone(), preview(&Value::String(v), max_preview));
                }
            }
        }
        if rows_scanned >= scan {
            break;
        }
    }
    out.emit(format!(
        "rows_scanned: {}{}",
        rows_scanned,
        if rows_scanned >= scan { "+" } else { "" }
    ));
    out.emit("column_stats:");
    for k in headers.iter().take(max_columns) {
        let marker = if is_large_field(k) {
            " LARGE_FIELD_NAME"
        } else {
            ""
        };
        out.emit(format!(
            "  {}: nonempty={} max_len={} sample={}{}",
            k,
            nonempty_counter.get(k).copied().unwrap_or(0),
            max_len.get(k).copied().unwrap_or(0),
            examples.get(k).cloned().unwrap_or_default(),
            marker
        ));
    }
    if !samples.is_empty() {
        out.emit("samples:");
        for (i, row) in samples.iter().enumerate() {
            out.emit(format!("  {}: {}", i, py_pairs_dict(row)));
        }
    }
    RunResult::ok(out.close())
}

#[allow(clippy::too_many_arguments)]
fn cmd_csv_project(
    path: &Path,
    fields_raw: &str,
    delimiter: Option<&str>,
    limit: usize,
    max_chars: usize,
    include_large_fields: bool,
    as_json: bool,
    max_lines: usize,
    max_line_chars: usize,
) -> RunResult {
    let fields = match parse_fields(fields_raw) {
        Ok(f) => f,
        Err(e) => return RunResult::err(2, format!("{}\n", e)),
    };
    let mut out = OutputBudget::new(max_lines, max_line_chars);
    if !path.exists() {
        return RunResult::err(2, format!("not found: {}\n", path.display()));
    }
    let delim = detect_delimiter(path, delimiter);
    let (skipped, effective) = skipped_and_effective(&fields, include_large_fields);
    emit_skipped(&mut out, &skipped);
    if effective.is_empty() {
        out.emit("no_effective_fields: all requested fields are known large fields");
        return RunResult::out_err(1, out.close(), String::new());
    }
    let mut reader = match make_csv_reader(path, delim) {
        Ok(r) => r,
        Err(e) => return RunResult::err(1, format!("csv project error: {}\n", e)),
    };
    let headers = match reader.headers() {
        Ok(h) => h.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        Err(e) => return RunResult::err(1, format!("csv project error: {}\n", e)),
    };
    let index = headers
        .iter()
        .enumerate()
        .map(|(i, h)| (h.clone(), i))
        .collect::<HashMap<_, _>>();
    for (i, rec) in reader.records().enumerate() {
        if i >= limit {
            break;
        }
        let rec = match rec {
            Ok(r) => r,
            Err(e) => return RunResult::err(1, format!("csv project error: {}\n", e)),
        };
        let row = effective
            .iter()
            .map(|field| {
                let value = index
                    .get(field)
                    .map(|idx| record_value(&rec, *idx))
                    .unwrap_or_default();
                (
                    field.clone(),
                    shorten_value(&Value::String(value), max_chars),
                )
            })
            .collect::<Vec<_>>();
        if as_json {
            let mut json_pairs = vec![("row".to_string(), json!(i + 1))];
            json_pairs.extend(row.clone());
            out.emit(json_sorted_line(json_pairs));
        } else {
            out.emit(format!("{}: {}", i + 1, py_pairs_dict(&row)));
        }
    }
    RunResult::ok(out.close())
}

fn cmd_parquet_summary(
    path: &Path,
    max_preview: usize,
    max_columns: usize,
    max_lines: usize,
    max_line_chars: usize,
) -> RunResult {
    let mut out = OutputBudget::new(max_lines, max_line_chars);
    if !path.exists() {
        return RunResult::err(2, format!("not found: {}\n", path.display()));
    }
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    out.emit(format!("path: {}", path.display()));
    out.emit(format!("bytes: {}", size));
    if size < 12 {
        out.emit("parquet_magic_ok: false");
        return RunResult::out_err(1, out.close(), String::new());
    }
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) => return RunResult::err(1, format!("parquet read error: {}\n", e)),
    };
    let mut head = [0u8; 4];
    if let Err(e) = file.read_exact(&mut head) {
        return RunResult::err(1, format!("parquet read error: {}\n", e));
    }
    if let Err(e) = file.seek(SeekFrom::End(-8)) {
        return RunResult::err(1, format!("parquet read error: {}\n", e));
    }
    let mut footer = [0u8; 8];
    if let Err(e) = file.read_exact(&mut footer) {
        return RunResult::err(1, format!("parquet read error: {}\n", e));
    }
    let footer_len = u32::from_le_bytes([footer[0], footer[1], footer[2], footer[3]]);
    let magic_ok = head == *b"PAR1" && footer[4..] == *b"PAR1";
    out.emit(format!(
        "parquet_magic_ok: {}",
        if magic_ok { "true" } else { "false" }
    ));
    out.emit(format!("footer_length_bytes: {}", footer_len));
    if !magic_ok {
        return RunResult::out_err(1, out.close(), String::new());
    }
    if footer_len == 0 {
        out.emit("column_metadata: unavailable (install pyarrow for schema/row-group metadata)");
        return RunResult::ok(out.close());
    }
    match File::open(path)
        .map_err(|e| e.to_string())
        .and_then(|f| SerializedFileReader::new(f).map_err(|e| e.to_string()))
    {
        Ok(reader) => {
            let metadata = reader.metadata();
            let file_meta = metadata.file_metadata();
            out.emit(format!("rows: {}", file_meta.num_rows()));
            out.emit(format!("row_groups: {}", metadata.num_row_groups()));
            let schema = file_meta.schema_descr();
            out.emit(format!("columns: {}", schema.num_columns()));
            if let Some(created_by) = file_meta.created_by() {
                out.emit(format!("created_by: {}", created_by));
            }
            let names = schema
                .columns()
                .iter()
                .map(|col| col.name().to_string())
                .collect::<Vec<_>>();
            let shown = names.iter().take(max_columns).cloned().collect::<Vec<_>>();
            out.emit(format!(
                "column_names({}): {}{}",
                names.len(),
                py_list_str(&shown),
                if names.len() > max_columns {
                    " ..."
                } else {
                    ""
                }
            ));
        }
        Err(e) => out.emit(format!(
            "column_metadata_error: RuntimeError: {}",
            preview(&Value::String(e), max_preview)
        )),
    }
    RunResult::ok(out.close())
}

fn cmd_summary(opts: &CommandOptions) -> RunResult {
    let path = Path::new(opts.file.as_deref().unwrap_or(""));
    match path_suffix(path).as_str() {
        ".json" => cmd_json_summary(
            path,
            opts.max_preview,
            opts.max_keys,
            opts.max_file_bytes,
            opts.force,
            opts.max_lines,
            opts.max_line_chars,
        ),
        ".jsonl" | ".ndjson" => cmd_jsonl_summary(
            path,
            opts.scan,
            opts.line_check,
            opts.max_line_probe_bytes,
            opts.max_record_bytes,
            opts.max_preview,
            opts.max_keys,
            opts.max_lines,
            opts.max_line_chars,
        ),
        ".csv" | ".tsv" => cmd_csv_summary(
            path,
            opts.delimiter.as_deref(),
            opts.scan,
            opts.sample,
            opts.max_preview,
            opts.max_columns,
            opts.max_lines,
            opts.max_line_chars,
        ),
        ".parquet" => cmd_parquet_summary(
            path,
            opts.max_preview,
            opts.max_columns,
            opts.max_lines,
            opts.max_line_chars,
        ),
        _ => RunResult::err(
            2,
            "unsupported structured summary type; use sniff for a bounded file probe\n",
        ),
    }
}

fn cmd_select(opts: &CommandOptions) -> RunResult {
    let path = Path::new(opts.file.as_deref().unwrap_or(""));
    let fields = opts.fields.as_deref().unwrap_or("");
    match path_suffix(path).as_str() {
        ".json" => cmd_json_select(
            path,
            fields,
            opts.limit,
            opts.max_chars,
            opts.max_file_bytes,
            opts.force,
            opts.include_large_fields,
            opts.include_empty,
            opts.as_json,
            opts.max_lines,
            opts.max_line_chars,
        ),
        ".jsonl" | ".ndjson" => cmd_jsonl_project(
            path,
            fields,
            opts.limit,
            opts.max_chars,
            opts.max_record_bytes,
            opts.include_large_fields,
            opts.include_empty,
            opts.as_json,
            opts.max_lines,
            opts.max_line_chars,
        ),
        ".csv" | ".tsv" => cmd_csv_project(
            path,
            fields,
            opts.delimiter.as_deref(),
            opts.limit,
            opts.max_chars,
            opts.include_large_fields,
            opts.as_json,
            opts.max_lines,
            opts.max_line_chars,
        ),
        _ => RunResult::err(2, "unsupported structured select type; select supports JSON, JSONL/NDJSON, CSV, and TSV\n"),
    }
}

fn looks_structured_path(token: &str) -> bool {
    let mut t = token.trim().trim_matches(&['\'', '"'][..]).to_string();
    if t.is_empty() || t.starts_with('-') {
        return false;
    }
    t = t.trim_end_matches(&[';', ',', ')'][..]).to_string();
    if t.contains('*') || t.contains('$') || t.contains('?') || t.contains('{') || t.contains('}') {
        let re = Regex::new(r#"(?i)\.(jsonl?|ndjson|csv|tsv|parquet)(\b|$|['";,)])"#).unwrap();
        if !re.is_match(&t) {
            return false;
        }
    }
    let lower_base = Path::new(&t)
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_lowercase();
    if STRUCTURED_BASENAME_HINTS.contains(&lower_base.as_str()) {
        return true;
    }
    if let Some(ext) = Path::new(&t).extension().and_then(|x| x.to_str()) {
        if STRUCTURED_SUFFIXES.contains(&ext.to_lowercase().as_str()) {
            return true;
        }
    }
    Regex::new(r#"(?i)\.(jsonl?|ndjson|csv|tsv|parquet)(\b|$|['";,)])"#)
        .unwrap()
        .is_match(&t)
}

fn tokenize_shell(command: &str) -> Vec<String> {
    shlex::split(command)
        .unwrap_or_else(|| command.split_whitespace().map(ToOwned::to_owned).collect())
}

fn structured_target_is_risky(token: &str, cwd: Option<&str>, allow_small_bytes: u64) -> bool {
    if !looks_structured_path(token) {
        return false;
    }
    let t = token
        .trim()
        .trim_matches(&['\'', '"'][..])
        .trim_end_matches(&[';', ',', ')'][..])
        .to_string();
    if t.contains('*') || t.contains('$') || t.contains('?') || t.contains('{') || t.contains('}') {
        return true;
    }
    let mut p = PathBuf::from(&t);
    if !p.is_absolute() {
        if let Some(cwd) = cwd {
            p = Path::new(cwd).join(p);
        }
    }
    if let Ok(meta) = fs::metadata(&p) {
        if meta.is_file() && meta.len() <= allow_small_bytes {
            return false;
        }
    }
    true
}

pub fn dangerous_raw_structured_command(
    command: &str,
    cwd: Option<&str>,
    allow_small_bytes: u64,
) -> (bool, String) {
    if command.trim().is_empty() || BYPASS_MARKERS.iter().any(|m| command.contains(m)) {
        return (false, String::new());
    }
    let tokens = tokenize_shell(command);
    if tokens.is_empty() {
        return (false, String::new());
    }
    for i in 0..tokens.len().saturating_sub(2) {
        let base = Path::new(&tokens[i])
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("");
        if (base == "python" || base == "python3" || base.starts_with("python"))
            && tokens.get(i + 1).map(|s| s.as_str()) == Some("-m")
            && tokens.get(i + 2).map(|s| s.as_str()) == Some("json.tool")
        {
            for target in tokens.iter().skip(i + 3) {
                if structured_target_is_risky(target, cwd, allow_small_bytes) {
                    return (
                        true,
                        format!("raw JSON pretty-print on structured artifact: {}", target),
                    );
                }
            }
        }
    }

    let operators = ["|", "||", "&&", ";"];
    let mut i = 0usize;
    while i < tokens.len() {
        if operators.contains(&tokens[i].as_str()) {
            i += 1;
            continue;
        }
        let base = Path::new(&tokens[i])
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("");
        if RAW_TOOLS.contains(&base) {
            let mut j = i + 1;
            while j < tokens.len() && !operators.contains(&tokens[j].as_str()) {
                if structured_target_is_risky(&tokens[j], cwd, allow_small_bytes) {
                    return (
                        true,
                        format!("raw `{}` on structured artifact: {}", base, tokens[j]),
                    );
                }
                j += 1;
            }
            i = j;
        } else {
            i += 1;
        }
    }

    for i in 0..tokens.len().saturating_sub(2) {
        let base = Path::new(&tokens[i])
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("");
        if ["bash", "sh", "zsh"].contains(&base)
            && matches!(
                tokens.get(i + 1).map(|s| s.as_str()),
                Some("-c") | Some("-lc")
            )
        {
            let (risky, reason) =
                dangerous_raw_structured_command(&tokens[i + 2], cwd, allow_small_bytes);
            if risky {
                return (true, reason);
            }
        }
    }
    (false, String::new())
}

fn cmd_guard(mode: &str, allow_small_bytes: u64, debug: bool, stdin: &str) -> RunResult {
    let payload: Value = match serde_json::from_str(stdin) {
        Ok(v) => v,
        Err(e) => {
            if debug {
                return RunResult::err(0, format!("guard: failed to parse stdin JSON: {}\n", e));
            }
            return RunResult::ok(String::new());
        }
    };
    let tool_input = payload.get("tool_input").unwrap_or(&Value::Null);
    let command = match tool_input {
        Value::Object(map) => match map.get("command").or_else(|| map.get("cmd")) {
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    _ => py_value_repr(v),
                })
                .collect::<Vec<_>>()
                .join(" "),
            Some(Value::String(s)) => s.clone(),
            Some(v) => py_value_repr(v),
            None => String::new(),
        },
        _ => String::new(),
    };
    let cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let (risky, reason) = dangerous_raw_structured_command(&command, cwd, allow_small_bytes);
    if !risky {
        return RunResult::ok(String::new());
    }
    let message = format!(
        "Structured Artifact Viewer blocked {}. Use MCP `structured_artifact_viewer` with `summary`/`select`, or CLI `codex-view summary FILE`. Bypass only when intentional with CODEX_VIEW_ALLOW_RAW=1 or # codex-view-allow-raw.",
        reason
    );
    if mode == "warn" {
        return RunResult::ok(format!(
            "{}\n",
            serde_json::to_string(&json!({"systemMessage": message})).unwrap()
        ));
    }
    RunResult::ok(format!(
        "{}\n",
        serde_json::to_string(&json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": message,
            }
        }))
        .unwrap()
    ))
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

fn merge_hooks(existing: &mut Value, command: &str, mode: &str) -> Result<(), String> {
    if !existing.is_object() {
        *existing = json!({"hooks": {}});
    }
    let root = existing.as_object_mut().unwrap();
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        return Err("existing hooks is not an object; refusing to modify".into());
    }
    let hooks_obj = hooks.as_object_mut().unwrap();
    let pre = hooks_obj.entry("PreToolUse").or_insert_with(|| json!([]));
    let Some(pre_arr) = pre.as_array_mut() else {
        return Err("existing hooks.PreToolUse is not a list; refusing to modify".into());
    };
    let hook_cmd = format!(
        "{} {} guard --mode {}",
        shell_quote(&env::current_exe().unwrap_or_default().display().to_string()),
        shell_quote(command),
        shell_quote(mode)
    );
    pre_arr.retain(|item| {
        if item.get("_managedBy").and_then(|v| v.as_str()) == Some("structured-artifact-viewer") {
            return false;
        }
        let text = serde_json::to_string(item).unwrap_or_default();
        !(text.contains("codex_view.py") && text.contains(" guard"))
    });
    pre_arr.push(json!({
        "matcher": "Bash",
        "hooks": [{
            "type": "command",
            "command": hook_cmd,
            "timeout": 5,
            "statusMessage": "Checking structured artifact output budget"
        }],
        "_managedBy": "structured-artifact-viewer"
    }));
    Ok(())
}

fn cmd_install_hook(scope: &str, mode: &str) -> RunResult {
    let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("codex-view"));
    let target_dir = if scope == "repo" {
        let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        find_repo_root(&cwd).join(".codex")
    } else {
        home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".codex")
    };
    let target = target_dir.join("hooks.json");
    if let Err(e) = fs::create_dir_all(&target_dir) {
        return RunResult::err(
            1,
            format!("cannot create {}: {}\n", target_dir.display(), e),
        );
    }
    let mut existing = if target.exists() {
        match fs::read_to_string(&target)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
        {
            Some(v) => v,
            None => {
                return RunResult::err(1, format!("cannot parse existing {}\n", target.display()))
            }
        }
    } else {
        json!({"hooks": {}})
    };
    if let Err(e) = merge_hooks(&mut existing, &exe.display().to_string(), mode) {
        return RunResult::err(1, format!("{}\n", e));
    }
    let rendered = match serde_json::to_string_pretty(&existing) {
        Ok(s) => s + "\n",
        Err(e) => return RunResult::err(1, format!("cannot render hooks: {}\n", e)),
    };
    let tmp = target.with_extension(format!(
        "{}tmp",
        target.extension().and_then(|x| x.to_str()).unwrap_or("")
    ));
    if let Err(e) = fs::write(&tmp, rendered) {
        return RunResult::err(1, format!("cannot write {}: {}\n", tmp.display(), e));
    }
    if let Err(e) = fs::rename(&tmp, &target) {
        return RunResult::err(1, format!("cannot replace {}: {}\n", target.display(), e));
    }
    RunResult::ok(format!(
        "installed {} hook: {}\nscript: {}\nIf your Codex version/config requires it, enable [features].codex_hooks = true.\n",
        mode,
        target.display(),
        exe.display()
    ))
}

fn cmd_install_command(scope: &str) -> RunResult {
    let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("codex-view"));
    let target_dir = if scope == "repo" {
        let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        find_repo_root(&cwd).join(".codex/bin")
    } else {
        home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local/bin")
    };
    if let Err(e) = fs::create_dir_all(&target_dir) {
        return RunResult::err(
            1,
            format!("cannot create {}: {}\n", target_dir.display(), e),
        );
    }
    let sh_target = target_dir.join("codex-view");
    let sh_content = format!(
        "#!/usr/bin/env sh\nset -eu\nexec {} \"$@\"\n",
        shell_quote(&exe.display().to_string())
    );
    if let Err(e) = fs::write(&sh_target, sh_content) {
        return RunResult::err(1, format!("cannot write {}: {}\n", sh_target.display(), e));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(&sh_target) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = fs::set_permissions(&sh_target, perms);
        }
    }
    let cmd_target = target_dir.join("codex-view.cmd");
    let cmd_content = format!("@echo off\r\n\"{}\" %*\r\n", exe.display());
    if let Err(e) = fs::write(&cmd_target, cmd_content) {
        return RunResult::err(1, format!("cannot write {}: {}\n", cmd_target.display(), e));
    }
    let usage = if scope == "repo" {
        "Use: .codex/bin/codex-view summary FILE"
    } else {
        "Ensure ~/.local/bin is on PATH, then use: codex-view summary FILE"
    };
    RunResult::ok(format!(
        "installed command: {}\ninstalled command: {}\n{}\n",
        sh_target.display(),
        cmd_target.display(),
        usage
    ))
}

#[derive(Clone)]
struct CommandOptions {
    file: Option<String>,
    fields: Option<String>,
    delimiter: Option<String>,
    scan: usize,
    sample: usize,
    line_check: usize,
    max_line_probe_bytes: usize,
    max_record_bytes: usize,
    max_preview: usize,
    max_keys: usize,
    max_columns: usize,
    max_file_bytes: u64,
    force: bool,
    limit: usize,
    max_chars: usize,
    include_large_fields: bool,
    include_empty: bool,
    as_json: bool,
    max_lines: usize,
    max_line_chars: usize,
    mode: String,
    scope: String,
    allow_small_bytes: u64,
    debug: bool,
}

impl CommandOptions {
    fn defaults(config: &Config) -> Self {
        Self {
            file: None,
            fields: None,
            delimiter: None,
            scan: config.get_usize("scan", DEFAULT_JSONL_SCAN),
            sample: config.get_usize("sample", 5),
            line_check: config.get_usize("line_check", DEFAULT_JSONL_LINE_CHECK),
            max_line_probe_bytes: config
                .get_usize("max_line_probe_bytes", DEFAULT_MAX_LINE_PROBE_BYTES),
            max_record_bytes: config.get_usize("max_record_bytes", DEFAULT_MAX_JSONL_RECORD_BYTES),
            max_preview: config.get_usize("max_preview", DEFAULT_MAX_PREVIEW),
            max_keys: config.get_usize("max_keys", 60),
            max_columns: config.get_usize("max_columns", 40),
            max_file_bytes: config.get_u64("max_file_bytes", DEFAULT_MAX_JSON_BYTES),
            force: false,
            limit: config.get_usize("limit", DEFAULT_SAMPLE_LIMIT),
            max_chars: config.get_usize("max_chars", DEFAULT_MAX_PREVIEW),
            include_large_fields: false,
            include_empty: false,
            as_json: false,
            max_lines: config.get_usize("max_lines", DEFAULT_MAX_LINES),
            max_line_chars: config.get_usize("max_line_chars", DEFAULT_MAX_LINE_CHARS),
            mode: "deny".to_string(),
            scope: "repo".to_string(),
            allow_small_bytes: config.get_u64("allow_small_bytes", DEFAULT_GUARD_ALLOW_SMALL_BYTES),
            debug: false,
        }
    }
}

fn split_option(arg: &str) -> (&str, Option<String>) {
    if let Some((k, v)) = arg.split_once('=') {
        (k, Some(v.to_string()))
    } else {
        (arg, None)
    }
}

fn take_value(
    args: &[String],
    i: &mut usize,
    inline: Option<String>,
    flag: &str,
) -> Result<String, String> {
    if let Some(v) = inline {
        return Ok(v);
    }
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("{} requires a value", flag))
}

fn parse_usize_value(raw: String, flag: &str) -> Result<usize, String> {
    raw.parse::<usize>()
        .map_err(|_| format!("{} expects an integer", flag))
}

fn parse_u64_value(raw: String, flag: &str) -> Result<u64, String> {
    raw.parse::<u64>()
        .map_err(|_| format!("{} expects an integer", flag))
}

fn parse_command_options(
    command: &str,
    args: &[String],
    config: &Config,
) -> Result<CommandOptions, String> {
    let mut opts = CommandOptions::defaults(config);
    if command == "json-summary" {
        opts.max_keys = config.get_usize("max_keys", 40);
    }
    if command == "jsonl-summary" {
        opts.max_preview = config.get_usize("max_preview", 120);
    }
    let mut positionals = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = &args[i];
        if arg == "-h" || arg == "--help" {
            return Err(help_text().trim_end().to_string());
        }
        if arg.starts_with("--") {
            let (flag, inline) = split_option(arg);
            match flag {
                "--delimiter" => opts.delimiter = Some(take_value(args, &mut i, inline, flag)?),
                "--scan" => {
                    opts.scan = parse_usize_value(take_value(args, &mut i, inline, flag)?, flag)?
                }
                "--sample" => {
                    opts.sample = parse_usize_value(take_value(args, &mut i, inline, flag)?, flag)?
                }
                "--line-check" => {
                    opts.line_check =
                        parse_usize_value(take_value(args, &mut i, inline, flag)?, flag)?
                }
                "--max-line-probe-bytes" => {
                    opts.max_line_probe_bytes =
                        parse_usize_value(take_value(args, &mut i, inline, flag)?, flag)?
                }
                "--max-record-bytes" => {
                    opts.max_record_bytes =
                        parse_usize_value(take_value(args, &mut i, inline, flag)?, flag)?
                }
                "--max-preview" => {
                    opts.max_preview =
                        parse_usize_value(take_value(args, &mut i, inline, flag)?, flag)?
                }
                "--max-keys" => {
                    opts.max_keys =
                        parse_usize_value(take_value(args, &mut i, inline, flag)?, flag)?
                }
                "--max-columns" => {
                    opts.max_columns =
                        parse_usize_value(take_value(args, &mut i, inline, flag)?, flag)?
                }
                "--max-file-bytes" => {
                    opts.max_file_bytes =
                        parse_u64_value(take_value(args, &mut i, inline, flag)?, flag)?
                }
                "--force" => opts.force = true,
                "--fields" => opts.fields = Some(take_value(args, &mut i, inline, flag)?),
                "--limit" => {
                    opts.limit = parse_usize_value(take_value(args, &mut i, inline, flag)?, flag)?
                }
                "--max-chars" => {
                    opts.max_chars =
                        parse_usize_value(take_value(args, &mut i, inline, flag)?, flag)?
                }
                "--include-large-fields" => opts.include_large_fields = true,
                "--include-empty" => opts.include_empty = true,
                "--as-json" => opts.as_json = true,
                "--max-lines" => {
                    opts.max_lines =
                        parse_usize_value(take_value(args, &mut i, inline, flag)?, flag)?
                }
                "--max-line-chars" => {
                    opts.max_line_chars =
                        parse_usize_value(take_value(args, &mut i, inline, flag)?, flag)?
                }
                "--mode" => opts.mode = take_value(args, &mut i, inline, flag)?,
                "--scope" => opts.scope = take_value(args, &mut i, inline, flag)?,
                "--allow-small-bytes" => {
                    opts.allow_small_bytes =
                        parse_u64_value(take_value(args, &mut i, inline, flag)?, flag)?
                }
                "--debug" => opts.debug = true,
                _ => return Err(format!("unrecognized arguments: {}", flag)),
            }
        } else {
            positionals.push(arg.clone());
        }
        i += 1;
    }
    if matches!(
        command,
        "sniff"
            | "summary"
            | "select"
            | "json-summary"
            | "jsonl-summary"
            | "jsonl-project"
            | "csv-summary"
            | "csv-project"
            | "parquet-summary"
    ) {
        opts.file = positionals.first().cloned();
        if opts.file.is_none() {
            return Err("missing file".into());
        }
    }
    if matches!(command, "select" | "jsonl-project" | "csv-project") && opts.fields.is_none() {
        return Err("--fields must include at least one field".into());
    }
    Ok(opts)
}

fn help_text() -> String {
    format!(
        "Compact structured artifact inspection for Codex\n\ncommands: sniff, summary, select, json-summary, jsonl-summary, jsonl-project, csv-summary, csv-project, parquet-summary, guard, install-hook, install-command\nversion: {}\n",
        VERSION
    )
}

pub fn run_cli(args: &[String], stdin: &str) -> RunResult {
    let (explicit_config, cleaned) = match extract_config_arg(args) {
        Ok(v) => v,
        Err(e) => return RunResult::err(2, format!("config error: {}\n", e)),
    };
    if cleaned.iter().any(|a| a == "--version") {
        return RunResult::ok(format!("{}\n", VERSION));
    }
    if cleaned.is_empty() || cleaned.iter().any(|a| a == "-h" || a == "--help") {
        return RunResult::ok(help_text());
    }
    let config = match load_config_defaults(explicit_config.as_deref()) {
        Ok(c) => c,
        Err(e) => return RunResult::err(2, format!("config error: {}\n", e)),
    };
    let command = &cleaned[0];
    let opts = match parse_command_options(command, &cleaned[1..], &config) {
        Ok(o) => o,
        Err(e) if e.starts_with("Compact structured") => return RunResult::ok(format!("{}\n", e)),
        Err(e) => return RunResult::err(2, format!("{}\n", e)),
    };
    let file_path = || Path::new(opts.file.as_deref().unwrap_or(""));
    match command.as_str() {
        "sniff" => cmd_sniff(
            file_path(),
            opts.line_check,
            opts.max_line_probe_bytes,
            opts.max_lines,
            opts.max_line_chars,
        ),
        "summary" => cmd_summary(&opts),
        "select" => cmd_select(&opts),
        "json-summary" => cmd_json_summary(
            file_path(),
            opts.max_preview,
            opts.max_keys,
            opts.max_file_bytes,
            opts.force,
            opts.max_lines,
            opts.max_line_chars,
        ),
        "jsonl-summary" => cmd_jsonl_summary(
            file_path(),
            opts.scan,
            opts.line_check,
            opts.max_line_probe_bytes,
            opts.max_record_bytes,
            opts.max_preview,
            opts.max_keys,
            opts.max_lines,
            opts.max_line_chars,
        ),
        "jsonl-project" => cmd_jsonl_project(
            file_path(),
            opts.fields.as_deref().unwrap_or(""),
            opts.limit,
            opts.max_chars,
            opts.max_record_bytes,
            opts.include_large_fields,
            opts.include_empty,
            opts.as_json,
            opts.max_lines,
            opts.max_line_chars,
        ),
        "csv-summary" => cmd_csv_summary(
            file_path(),
            opts.delimiter.as_deref(),
            opts.scan,
            opts.sample,
            opts.max_preview,
            opts.max_columns,
            opts.max_lines,
            opts.max_line_chars,
        ),
        "csv-project" => cmd_csv_project(
            file_path(),
            opts.fields.as_deref().unwrap_or(""),
            opts.delimiter.as_deref(),
            opts.limit,
            opts.max_chars,
            opts.include_large_fields,
            opts.as_json,
            opts.max_lines,
            opts.max_line_chars,
        ),
        "parquet-summary" => cmd_parquet_summary(
            file_path(),
            opts.max_preview,
            opts.max_columns,
            opts.max_lines,
            opts.max_line_chars,
        ),
        "guard" => cmd_guard(&opts.mode, opts.allow_small_bytes, opts.debug, stdin),
        "install-hook" => cmd_install_hook(&opts.scope, &opts.mode),
        "install-command" => cmd_install_command(&opts.scope),
        other => RunResult::err(2, format!("unknown command: {}\n", other)),
    }
}

pub fn run_cli_from_env() -> i32 {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let needs_stdin = args.iter().any(|a| a == "guard");
    let mut stdin = String::new();
    if needs_stdin {
        let _ = io::stdin().read_to_string(&mut stdin);
    }
    let result = run_cli(&args, &stdin);
    let _ = io::stdout().write_all(result.stdout.as_bytes());
    let _ = io::stderr().write_all(result.stderr.as_bytes());
    result.code
}

const PROTOCOL_VERSION: &str = "2025-06-18";
const TOOL_NAME: &str = "structured_artifact_viewer";
const OPS: &[&str] = &["select", "self_check", "sniff", "summary"];

fn compact_json(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string())
}

fn tool_defs() -> Value {
    json!([{
        "name": TOOL_NAME,
        "description": "Bounded inspection for unknown-size, large, generated, or high-output structured artifacts.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "op": {"type": "string", "enum": ["sniff", "summary", "select", "self_check"]},
                "args": {"type": "object"}
            },
            "required": ["op"],
            "additionalProperties": false
        }
    }])
}

fn result(req_id: Value, data: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": req_id, "result": data})
}

fn error(req_id: Value, code: i64, msg: &str, data: Option<Value>) -> Value {
    let mut body = Map::new();
    body.insert("code".to_string(), json!(code));
    body.insert("message".to_string(), json!(msg));
    if let Some(data) = data {
        body.insert("data".to_string(), data);
    }
    json!({"jsonrpc": "2.0", "id": req_id, "error": Value::Object(body)})
}

fn normalize_fields_mcp(value: &Value) -> Result<String, String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Array(items) if items.iter().all(|x| x.is_string()) => Ok(items
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect::<Vec<_>>()
            .join(",")),
        _ => Err("fields must be a comma-separated string or list of strings".into()),
    }
}

fn add_mcp_flag(argv: &mut Vec<String>, flag: &str, value: &Value) {
    if let Some(b) = value.as_bool() {
        if b {
            argv.push(flag.to_string());
        }
    } else if let Some(s) = value.as_str() {
        argv.push(flag.to_string());
        argv.push(s.to_string());
    } else if value.is_number() {
        argv.push(flag.to_string());
        argv.push(value.to_string());
    }
}

fn mcp_cli_argv(op: &str, args: &Map<String, Value>) -> Result<Vec<String>, String> {
    let common = ["path", "file", "config", "max_lines", "max_line_chars"];
    let op_args: &[&str] = match op {
        "sniff" => &["line_check", "max_line_probe_bytes"],
        "summary" => &[
            "delimiter",
            "scan",
            "sample",
            "line_check",
            "max_line_probe_bytes",
            "max_record_bytes",
            "max_preview",
            "max_keys",
            "max_columns",
            "max_file_bytes",
            "force",
        ],
        "select" => &[
            "fields",
            "delimiter",
            "limit",
            "max_chars",
            "max_record_bytes",
            "max_file_bytes",
            "force",
            "include_large_fields",
            "include_empty",
            "as_json",
        ],
        _ => &[],
    };
    let allowed = common
        .iter()
        .chain(op_args.iter())
        .copied()
        .collect::<HashSet<_>>();
    let unknown = args
        .keys()
        .filter(|k| !allowed.contains(k.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(format!(
            "unsupported args for {}: {}",
            op,
            py_list_str(&unknown)
        ));
    }
    let path = args
        .get("path")
        .or_else(|| args.get("file"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if path.is_empty() {
        return Err(format!("{} requires args.path", op));
    }
    let mut argv = vec![op.to_string(), path.to_string()];
    let mut keys = allowed
        .into_iter()
        .filter(|k| !matches!(*k, "path" | "file" | "fields"))
        .collect::<Vec<_>>();
    keys.sort_unstable();
    for key in keys {
        if let Some(value) = args.get(key) {
            add_mcp_flag(&mut argv, &format!("--{}", key.replace('_', "-")), value);
        }
    }
    if op == "select" {
        let fields = args
            .get("fields")
            .ok_or_else(|| "select requires args.fields".to_string())?;
        argv.push("--fields".to_string());
        argv.push(normalize_fields_mcp(fields)?);
    }
    Ok(argv)
}

fn next_steps(op: &str, args: &Map<String, Value>) -> Value {
    let path = args
        .get("path")
        .or_else(|| args.get("file"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if path.is_empty() {
        return json!([]);
    }
    if op == "sniff" {
        return json!([{"op": "summary", "args": {"path": path}}]);
    }
    if op == "summary" {
        let suffix = Path::new(path)
            .extension()
            .and_then(|x| x.to_str())
            .map(|x| format!(".{}", x.to_lowercase()))
            .unwrap_or_default();
        if [".json", ".jsonl", ".ndjson", ".csv", ".tsv"].contains(&suffix.as_str()) {
            return json!([{"op": "select", "args": {"path": path, "fields": ["name", "status", "score", "path"], "limit": 10}}]);
        }
    }
    json!([])
}

fn self_check() -> Value {
    let exe =
        env::current_exe().unwrap_or_else(|_| PathBuf::from("structured-artifact-mcp-server"));
    let root = exe
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or_else(|| Path::new("."));
    json!({
        "status": "ok",
        "version": VERSION,
        "server": "structured-artifact-viewer",
        "root": root.display().to_string(),
        "cli": root.join("bin").join("codex-view").display().to_string(),
        "ops": OPS
    })
}

fn call_tool(name: &str, arguments: &Value) -> Value {
    if name != TOOL_NAME {
        return json!({"status": "error", "error": "unknown_tool", "tool": name});
    }
    let op = arguments.get("op").and_then(|v| v.as_str()).unwrap_or("");
    if !["sniff", "summary", "select", "self_check"].contains(&op) {
        return json!({"status": "error", "error": "bad_op", "valid_ops": ["select", "self_check", "sniff", "summary"]});
    }
    let args = arguments.get("args").cloned().unwrap_or_else(|| json!({}));
    let Some(args_obj) = args.as_object() else {
        return json!({"status": "error", "error": "args_must_be_object"});
    };
    if op == "self_check" {
        return self_check();
    }
    let argv = match mcp_cli_argv(op, args_obj) {
        Ok(v) => v,
        Err(e) => return json!({"status": "error", "error": "ValueError", "message": e}),
    };
    let result = run_cli(&argv, "");
    let mut data = Map::new();
    data.insert(
        "status".into(),
        json!(if result.code == 0 { "ok" } else { "error" }),
    );
    data.insert("op".into(), json!(op));
    data.insert(
        "path".into(),
        args_obj
            .get("path")
            .or_else(|| args_obj.get("file"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    data.insert("exit_code".into(), json!(result.code));
    data.insert("output".into(), json!(result.stdout.trim_end_matches('\n')));
    if !result.stderr.is_empty() {
        data.insert("stderr".into(), json!(result.stderr.trim_end_matches('\n')));
    }
    if result.code == 0 {
        data.insert("next".into(), next_steps(op, args_obj));
    }
    Value::Object(data)
}

pub fn handle_mcp(msg: &Value) -> Option<Value> {
    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let req_id = msg.get("id").cloned().unwrap_or(Value::Null);
    let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
    match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL_VERSION);
            let proto = if ["2025-06-18", "2025-03-26", "2024-11-05"].contains(&requested) {
                requested
            } else {
                PROTOCOL_VERSION
            };
            Some(result(
                req_id,
                json!({
                    "protocolVersion": proto,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "structured-artifact-viewer", "version": VERSION},
                    "instructions": "Use for unknown-size, large, generated, or high-output structured artifacts. Prefer summary, then select. Avoid for small known config files or jq-style queries."
                }),
            ))
        }
        "notifications/initialized" => None,
        "ping" => Some(result(req_id, json!({}))),
        "tools/list" => Some(result(req_id, json!({"tools": tool_defs()}))),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let data = call_tool(name, &args);
            Some(result(
                req_id,
                json!({
                    "content": [{"type": "text", "text": compact_json(&data)}],
                    "isError": data.get("status").and_then(|v| v.as_str()) == Some("error")
                }),
            ))
        }
        _ if msg.get("id").is_none() => None,
        _ => Some(error(
            req_id,
            -32601,
            &format!("method not found: {}", method),
            None,
        )),
    }
}

pub fn run_mcp_server() -> i32 {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(v) => v.trim().to_string(),
            Err(_) => continue,
        };
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(
                    stdout,
                    "{}",
                    compact_json(&error(
                        Value::Null,
                        -32700,
                        "parse error",
                        Some(json!(e.to_string()))
                    ))
                );
                continue;
            }
        };
        if let Some(resp) = handle_mcp(&msg) {
            let _ = writeln!(stdout, "{}", compact_json(&resp));
            let _ = stdout.flush();
        }
    }
    0
}
