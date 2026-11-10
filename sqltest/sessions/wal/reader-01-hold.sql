-- Reader only. Hold this connection and transaction. NOT_RUN.
PRAGMA busy_timeout=50;
BEGIN DEFERRED;
SELECT id,balance FROM ledger ORDER BY id;
SELECT sum(balance) FROM ledger;
-- External orchestrator ACK READER_SNAPSHOT_OPEN after consuming results.
