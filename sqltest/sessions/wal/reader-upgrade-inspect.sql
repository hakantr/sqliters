-- Same held reader connection after expected upgrade error. NOT_RUN.
SELECT sum(balance) FROM ledger;
ROLLBACK;
SELECT sum(balance) FROM ledger;
