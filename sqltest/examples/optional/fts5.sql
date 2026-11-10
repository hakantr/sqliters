-- Optional SQLITE_ENABLE_FTS5; NOT_RUN.
CREATE VIRTUAL TABLE articles USING fts5(title,body);
INSERT INTO articles(rowid,title,body) VALUES(1,'SQLite notes','SQLite stores local data'),(2,'Rust notes','Rust and SQLite interoperability'),(3,'Unicode','İstanbul 東京 Zoë');
SELECT rowid,title,bm25(articles) FROM articles WHERE articles MATCH 'SQLite OR Rust' ORDER BY bm25(articles),rowid;
SELECT rowid,highlight(articles,1,'[',']') FROM articles WHERE articles MATCH 'SQLite' ORDER BY rowid;
