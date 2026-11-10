-- Same reader connection/transaction; after WRITER_COMMITTED. NOT_RUN.
SELECT id,balance FROM ledger ORDER BY id;
SELECT sum(balance) FROM ledger;
