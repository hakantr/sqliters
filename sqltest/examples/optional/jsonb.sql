-- Optional JSONB fixture; requires json.sql in same future DB. NOT_RUN.
CREATE TABLE jsonb_docs(id INTEGER PRIMARY KEY,jb BLOB);
INSERT INTO jsonb_docs SELECT id,jsonb(j) FROM json_docs;
SELECT id,typeof(jb),json(jb),json_extract(jb,'$.name'),jsonb_extract(jb,'$.tags') FROM jsonb_docs ORDER BY id;
