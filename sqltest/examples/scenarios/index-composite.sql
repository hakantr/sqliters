-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE INDEX customer_city_name ON customers(city,name); SELECT id FROM customers INDEXED BY customer_city_name WHERE city IS NULL ORDER BY id; DROP INDEX customer_city_name;
