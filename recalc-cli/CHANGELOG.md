# recalc CLI changelog

## 0.1.1

- Load workbooks whose formula cells carry an empty cached-value element
  (`<v />`), the shape every openpyxl-saved workbook has. 0.1.0 rejected such
  files with "not a valid number", so it could not verify programmatically
  written workbooks.
- `SUMPRODUCT` evaluates computed argument expressions over ranges in array
  context. 0.1.0 applied implicit intersection instead, which produced a
  silently wrong number in the host row and `#VALUE!` elsewhere. Cases not
  pinned by an oracle experiment now refuse loudly with `#UNSUPPORTED!`.

## 0.1.0

- First standalone release: `recalc verify` with the Verify v1 contract,
  exit codes 0/1/2/64, `recalc.verify.report/v1` reports, demo workbooks.
