-- Fresh future verification after checkpoint. NOT_RUN.
SELECT id,balance FROM ledger ORDER BY id;
PRAGMA integrity_check;
