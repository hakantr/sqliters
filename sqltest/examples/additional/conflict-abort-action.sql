-- INTENTIONALLY NEGATIVE UNIQUE conflict; NOT_RUN. Continue to inspect file afterward.
INSERT OR ABORT INTO conflict_demo VALUES(3,'c'),(4,'a'),(5,'e');
