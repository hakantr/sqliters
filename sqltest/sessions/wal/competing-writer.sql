-- INTENTIONALLY NEGATIVE while first writer holds write lock. NOT_RUN.
PRAGMA busy_timeout=50;
BEGIN IMMEDIATE;
-- Expected BUSY after bounded busy handling, not successful BEGIN.
