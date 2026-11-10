-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
-- Semicolons inside literal/comment must not split SQL.
SELECT ';' AS punctuation; /* comment ; */ SELECT 'it''s valid text' AS escaped;
