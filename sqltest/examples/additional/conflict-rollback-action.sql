-- INTENTIONALLY NEGATIVE UNIQUE conflict; NOT_RUN. Continue to inspect file afterward.
INSERT OR ROLLBACK INTO conflict_demo VALUES(3,'c'),(4,'a'),(5,'e');
