-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE TABLE "odd table"("select" INTEGER,"a""b" TEXT); INSERT INTO "odd table" VALUES(1,'semi;colon -- literal'); SELECT "select","a""b" FROM "odd table";
