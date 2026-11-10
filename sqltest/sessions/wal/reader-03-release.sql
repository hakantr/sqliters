-- Same reader; finish held transaction, then fresh snapshot. NOT_RUN.
COMMIT;
SELECT id,balance FROM ledger ORDER BY id;
SELECT sum(balance) FROM ledger;
