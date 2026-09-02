# Verify receipt bundle

`receipt.py` creates deterministic, local-only manifests for a farm or
customer-approved Verify run. It hashes exact workbook/report bytes, records
the caller-supplied Excel build label, and totals decisions/refusals without
claiming that a cached value is an oracle.

```sh
python3 tools/verify/receipt.py \
  --corpus /path/to/corpus --reports /path/to/reports \
  --excel-build 16.0.20131.20154 --run-id 2026-09-02-farm-01 \
  --out /tmp/recalc-receipt
```

The output is `corpus-manifest.json` and `receipt-manifest.json`, both
canonical UTF-8 JSON with a trailing newline. A malformed report returns exit
code 2. The tool performs no network or workbook upload.
