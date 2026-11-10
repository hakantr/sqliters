-- Alternate two-writer recipe: hold transaction until external release. NOT_RUN.
BEGIN IMMEDIATE;
UPDATE ledger SET balance=balance+1 WHERE id=1;
-- External WRITER_LOCK_HELD only after successful UPDATE; do not auto-commit.
