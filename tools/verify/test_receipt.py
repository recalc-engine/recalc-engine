import hashlib
import json
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import receipt


class ReceiptTests(unittest.TestCase):
    def test_manifest_is_deterministic_and_counts_decisions(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            corpus = root / "corpus"
            reports = root / "reports"
            out = root / "out"
            corpus.mkdir()
            reports.mkdir()
            (corpus / "demo.xlsx").write_bytes(b"xlsx-fixture")
            report = {
                "schema_version": "recalc.verify.report/v1",
                "decision": "fallback",
                "summary": {"unsupported": 2, "blocked": 1, "resource_limited": 0, "formula_errors": 1, "mismatches": 3},
            }
            (reports / "demo.json").write_text(json.dumps(report), encoding="utf-8")
            self.assertEqual(receipt.main(["--corpus", str(corpus), "--reports", str(reports), "--out", str(out), "--excel-build", "16.0.test", "--run-id", "run-1"]), 0)
            manifest = json.loads((out / "corpus-manifest.json").read_text())
            bundle = json.loads((out / "receipt-manifest.json").read_text())
            self.assertEqual(manifest["workbook_count"], 1)
            self.assertEqual(bundle["decisions"]["fallback"], 1)
            self.assertEqual(bundle["refusal_counts"]["unsupported"], 2)
            expected = hashlib.sha256(receipt.canonical(manifest)).hexdigest()
            self.assertEqual(bundle["corpus_manifest_sha256"], expected)


if __name__ == "__main__":
    unittest.main()
