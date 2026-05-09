import csv
import io
import json
import os
import subprocess
import struct
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "codex_view.py"

sys.path.insert(0, str(ROOT / "scripts"))
import codex_view  # noqa: E402


class CodexViewTests(unittest.TestCase):
    def run_cli(self, *args):
        return subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            cwd=str(ROOT),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )

    def test_json_summary_truncates_large_scalar(self):
        with tempfile.TemporaryDirectory() as td:
            p = Path(td) / "status.json"
            p.write_text(json.dumps({"stage": "x", "payload": "A" * 2000, "runs": [{"name": "a", "score": 1}]}), encoding="utf-8")
            r = self.run_cli("json-summary", str(p), "--max-lines", "30", "--max-line-chars", "220")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("top_keys", r.stdout)
            self.assertIn("runs: list len=1", r.stdout)
            self.assertLess(len(r.stdout), 2500)
            self.assertNotIn("A" * 500, r.stdout)

    def test_jsonl_summary_detects_large_fields_without_dumping(self):
        with tempfile.TemporaryDirectory() as td:
            p = Path(td) / "candidate_scores.jsonl"
            with p.open("w", encoding="utf-8") as f:
                for i in range(3):
                    f.write(json.dumps({"name": f"n{i}", "score": i, "payload": "B" * 5000}) + "\n")
            r = self.run_cli("jsonl-summary", str(p), "--scan", "3")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("payload", r.stdout)
            self.assertIn("LARGE_FIELD_NAME", r.stdout)
            self.assertLess(len(r.stdout), 5000)
            self.assertNotIn("B" * 300, r.stdout)

    def test_jsonl_project_skips_large_fields_by_default(self):
        with tempfile.TemporaryDirectory() as td:
            p = Path(td) / "data.jsonl"
            p.write_text(json.dumps({"name": "a", "payload": "C" * 1000, "nested": {"score": 7}}) + "\n", encoding="utf-8")
            r = self.run_cli("jsonl-project", str(p), "--fields", "name,payload,nested.score", "--limit", "1")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("skipped_large_fields", r.stdout)
            self.assertIn("name", r.stdout)
            self.assertIn("nested.score", r.stdout)
            self.assertNotIn("C" * 100, r.stdout)

    def test_csv_summary_and_project(self):
        with tempfile.TemporaryDirectory() as td:
            p = Path(td) / "results.csv"
            with p.open("w", encoding="utf-8", newline="") as f:
                w = csv.DictWriter(f, fieldnames=["name", "status", "score", "logs"])
                w.writeheader()
                w.writerow({"name": "a", "status": "ok", "score": "0.9", "logs": "D" * 1000})
            r = self.run_cli("csv-summary", str(p), "--scan", "1")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("columns(4)", r.stdout)
            self.assertIn("logs", r.stdout)
            self.assertIn("LARGE_FIELD_NAME", r.stdout)
            r = self.run_cli("csv-project", str(p), "--fields", "name,status,score,logs", "--limit", "1")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("skipped_large_fields", r.stdout)
            self.assertNotIn("D" * 100, r.stdout)

    def test_guard_blocks_raw_structured_reads(self):
        payload = {"tool_input": {"command": "sed -n '1,80p' candidate_scores.jsonl"}}
        r = subprocess.run(
            [sys.executable, str(SCRIPT), "guard", "--mode", "deny"],
            input=json.dumps(payload),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        out = json.loads(r.stdout)
        self.assertEqual(out["hookSpecificOutput"]["permissionDecision"], "deny")
        self.assertIn("structured artifact", out["hookSpecificOutput"]["permissionDecisionReason"])

    def test_guard_allows_safe_summary_and_bypass(self):
        self.assertFalse(codex_view.dangerous_raw_structured_command("python scripts/codex_view.py jsonl-summary data.jsonl")[0])
        self.assertFalse(codex_view.dangerous_raw_structured_command("CODEX_VIEW_ALLOW_RAW=1 cat data.jsonl")[0])
        self.assertTrue(codex_view.dangerous_raw_structured_command("python -m json.tool summary.json")[0])
        self.assertTrue(codex_view.dangerous_raw_structured_command("bash -lc 'head -20 huge.jsonl'")[0])

    def test_install_hook_merges_without_duplicates(self):
        existing = {"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": []}]}}
        m1 = codex_view.merge_hooks(existing, str(SCRIPT), "deny")
        m2 = codex_view.merge_hooks(m1, str(SCRIPT), "deny")
        entries = m2["hooks"]["PreToolUse"]
        managed = [x for x in entries if isinstance(x, dict) and x.get("_managedBy") == "structured-artifact-viewer"]
        self.assertEqual(len(managed), 1)

    def test_install_command_wrapper_works(self):
        with tempfile.TemporaryDirectory() as td:
            (Path(td) / ".git").mkdir()
            r = subprocess.run(
                [sys.executable, str(SCRIPT), "install-command", "--scope", "repo"],
                cwd=td,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            self.assertEqual(r.returncode, 0, r.stderr)
            wrapper = Path(td) / ".codex" / "bin" / "codex-view"
            self.assertTrue(wrapper.exists())
            r2 = subprocess.run(
                [str(wrapper), "--version"],
                cwd=td,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            self.assertEqual(r2.returncode, 0, r2.stderr)
            self.assertIn(codex_view.VERSION, r2.stdout)

    def test_guard_allows_small_existing_json(self):
        with tempfile.TemporaryDirectory() as td:
            small = Path(td) / "package.json"
            small.write_text('{"name":"demo"}', encoding="utf-8")
            risky, reason = codex_view.dangerous_raw_structured_command(
                "cat package.json", cwd=td, allow_small_bytes=1024
            )
            self.assertFalse(risky, reason)
            big = Path(td) / "big.json"
            big.write_text("{\"payload\":\"" + "X" * 5000 + "\"}", encoding="utf-8")
            risky, reason = codex_view.dangerous_raw_structured_command(
                "cat big.json", cwd=td, allow_small_bytes=1024
            )
            self.assertTrue(risky)

    def test_jsonl_summary_skips_oversize_record_without_dumping(self):
        with tempfile.TemporaryDirectory() as td:
            p = Path(td) / "huge.jsonl"
            p.write_text(json.dumps({"payload": "Z" * 20000}) + "\n" + json.dumps({"name": "ok", "score": 1}) + "\n", encoding="utf-8")
            r = self.run_cli("jsonl-summary", str(p), "--max-record-bytes", "1000", "--line-check", "2", "--max-line-probe-bytes", "1000")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("oversize_records_skipped", r.stdout)
            self.assertIn("name", r.stdout)
            self.assertNotIn("Z" * 100, r.stdout)

    def test_generic_summary_and_select_dispatch(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            json_path = root / "summary.json"
            json_path.write_text(json.dumps({"name": "demo", "payload": "P" * 1000, "nested": {"score": 3}}), encoding="utf-8")
            jsonl_path = root / "rows.jsonl"
            jsonl_path.write_text(json.dumps({"name": "a", "score": 1, "source": "S" * 1000}) + "\n", encoding="utf-8")
            csv_path = root / "rows.csv"
            csv_path.write_text("name,score,logs\na,1," + "L" * 1000 + "\n", encoding="utf-8")

            r = self.run_cli("summary", str(jsonl_path))
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("records_scanned", r.stdout)
            self.assertNotIn("S" * 200, r.stdout)

            r = self.run_cli("select", str(json_path), "--fields", "name,payload,nested.score")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("skipped_large_fields", r.stdout)
            self.assertIn("nested.score", r.stdout)
            self.assertNotIn("P" * 100, r.stdout)

            r = self.run_cli("select", str(csv_path), "--fields", "name,score,logs")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("skipped_large_fields", r.stdout)
            self.assertNotIn("L" * 100, r.stdout)

    def test_yaml_is_not_guarded_as_structured_artifact(self):
        self.assertFalse(codex_view.dangerous_raw_structured_command("cat config.yml")[0])
        self.assertFalse(codex_view.dangerous_raw_structured_command("cat config.yaml")[0])

    def test_parquet_summary_reads_footer_only(self):
        with tempfile.TemporaryDirectory() as td:
            p = Path(td) / "sample.parquet"
            p.write_bytes(b"PAR1" + struct.pack("<I", 0) + b"PAR1")
            r = self.run_cli("parquet-summary", str(p))
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("parquet_magic_ok: true", r.stdout)
            self.assertIn("footer_length_bytes: 0", r.stdout)


if __name__ == "__main__":
    unittest.main()
