-- Initial schema matching the original SQLite structure.
-- Keeps memory.user_id as TEXT to match SQLite's original layout.
-- Run 002_improve_types.sql after loading to upgrade to native PG types.

CREATE TABLE IF NOT EXISTS "user" (
    id             BIGINT PRIMARY KEY,
    level          INT    NOT NULL DEFAULT 0,
    xp             INT    NOT NULL DEFAULT 0,
    social_credit  BIGINT NOT NULL DEFAULT 0,
    name           TEXT   NOT NULL DEFAULT '',
    relationship   TEXT   NOT NULL DEFAULT '',
    example_input  TEXT   NOT NULL DEFAULT '',
    example_output TEXT   NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS memory (
    id      BIGSERIAL PRIMARY KEY,
    user_id TEXT      NOT NULL,
    content TEXT      NOT NULL,
    key     TEXT      NOT NULL,
    UNIQUE (user_id, key)
);

CREATE TABLE IF NOT EXISTS mathquestion (
    id       SERIAL           PRIMARY KEY,
    question TEXT             NOT NULL,
    answer   DOUBLE PRECISION NOT NULL
);
