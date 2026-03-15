# SQLite → PostgreSQL Migration

## Overview

The bot uses PostgreSQL with `tokio-postgres` + `deadpool-postgres`. On every startup
`run_migrations` runs both SQL files in order — idempotent, so safe to run repeatedly.

```
migrations/
  001_init.sql          — base schema (mirrors SQLite layout, used for fresh installs)
  002_improve_types.sql — upgrades to PG-native types (runs after pgloader or after 001)
  pgloader.load         — pgloader script for one-time SQLite data migration
```

---

## Fresh install (no existing data)

1. Create a PostgreSQL database.
2. Set `DATABASE_URL` and start the bot — migrations run automatically on startup.

```bash
DATABASE_URL=postgres://user:pass@localhost/trickedbot cargo run
```

---

## Migrating from existing SQLite (`database.db`)

### 1. Install pgloader

```bash
nix-shell -p pgloader   # or: apt install pgloader
```

### 2. Edit `migrations/pgloader.load`

Fill in the real paths:

```
FROM sqlite:///absolute/path/to/database.db
INTO postgresql://user:pass@localhost/trickedbot
```

### 3. Run pgloader

```bash
pgloader migrations/pgloader.load
```

pgloader creates the tables and loads all data. The CAST rules handle:
- `user.id` — TEXT in SQLite (rusqlite stored u64 as strings) → BIGINT
- `memory.user_id` — stored as integer in SQLite → BIGINT
- all other integer columns → BIGINT (002 downcasts level/xp back to INT)

### 4. Start the bot

```bash
DATABASE_URL=postgres://user:pass@localhost/trickedbot cargo run
```

On startup `run_migrations` runs:
- **001** — no-op (tables already exist from pgloader)
- **002** — upgrades types in-place:
  - `user.id` TEXT → BIGINT (if not already done by pgloader CAST)
  - `user.level`, `user.xp` BIGINT → INT (pgloader over-widens all integers)
  - `memory.user_id` TEXT → BIGINT (if not already done)
  - `math_question.answer` real → double precision
  - adds FK `memory.user_id → user.id ON DELETE CASCADE` (drops orphaned memories first)

All 002 steps check current column types before acting — safe to re-run.

---

## Notes

- `math_question` — pgloader snake_cases the SQLite table name `MathQuestion`; the bot queries `math_question` accordingly.
- Orphaned memories (referencing users who left) are automatically deleted before the FK is added.
- `DATABASE_FILE` / `--database-file` no longer exists; use `DATABASE_URL` / `--database-url`.
