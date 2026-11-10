-- Writer only, after READER_SNAPSHOT_OPEN. NOT_RUN.
BEGIN IMMEDIATE;
UPDATE ledger SET balance=balance+25 WHERE id=1;
COMMIT;
SELECT sum(balance) FROM ledger;
-- External ACK WRITER_COMMITTED only after COMMIT success and result consumption.
