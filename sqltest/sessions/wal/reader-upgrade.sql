-- INTENTIONALLY NEGATIVE alternate branch: reader snapshot predates writer commit. NOT_RUN.
UPDATE ledger SET balance=balance+1 WHERE id=2;
-- Expected primary BUSY / extended BUSY_SNAPSHOT; real result must precede external ACK.
