-- Optional JSON capability fixture; NOT_RUN.
CREATE TABLE json_docs(id INTEGER PRIMARY KEY,j TEXT);
INSERT INTO json_docs VALUES(1,'{"name":"Ada","n":3,"tags":["a","b"],"nil":null}'),(2,'{"name":"東京","n":4,"tags":[]}');
SELECT id,json_valid(j),json_type(j,'$.nil'),json_extract(j,'$.name'),json_extract(j,'$.n') FROM json_docs ORDER BY id;
SELECT d.id,e.key,e.value,e.type FROM json_docs d,json_each(d.j,'$.tags') e ORDER BY d.id,e.key;
SELECT json_group_array(id) FROM(SELECT id FROM json_docs ORDER BY id);
