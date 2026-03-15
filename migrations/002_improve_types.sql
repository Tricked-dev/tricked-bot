-- Upgrade schema from SQLite-compatible types to PostgreSQL-native types.
-- Idempotent: safe to run on any state (fresh, post-pgloader, or already upgraded).

-- 1. Convert user.id from TEXT to BIGINT (it was stored as string by rusqlite)
DO $$ DECLARE
  pk_name text;
BEGIN
  IF (SELECT data_type FROM information_schema.columns
      WHERE table_name = 'user' AND column_name = 'id') = 'text' THEN

    -- Dynamically find and drop the PK so we can change the column type
    SELECT c.conname INTO pk_name
    FROM pg_constraint c
    JOIN pg_class t ON c.conrelid = t.oid
    WHERE t.relname = 'user' AND c.contype = 'p';

    IF pk_name IS NOT NULL THEN
      EXECUTE format('ALTER TABLE "user" DROP CONSTRAINT %I', pk_name);
    END IF;

    ALTER TABLE "user" ALTER COLUMN id TYPE BIGINT USING id::BIGINT;
    ALTER TABLE "user" ADD PRIMARY KEY (id);
  END IF;
END $$;

-- 2. Downcast level/xp from BIGINT (pgloader global cast) back to INT (Rust i32)
DO $$ BEGIN
  IF (SELECT data_type FROM information_schema.columns
      WHERE table_name = 'user' AND column_name = 'level') = 'bigint' THEN
    ALTER TABLE "user" ALTER COLUMN level TYPE INT USING level::INT;
  END IF;
END $$;

DO $$ BEGIN
  IF (SELECT data_type FROM information_schema.columns
      WHERE table_name = 'user' AND column_name = 'xp') = 'bigint' THEN
    ALTER TABLE "user" ALTER COLUMN xp TYPE INT USING xp::INT;
  END IF;
END $$;

-- 3. Convert memory.user_id from TEXT to BIGINT (if not already done by pgloader CAST)
DO $$ BEGIN
  IF (SELECT data_type FROM information_schema.columns
      WHERE table_name = 'memory' AND column_name = 'user_id') = 'text' THEN

    ALTER TABLE memory DROP CONSTRAINT IF EXISTS memory_user_id_key;
    ALTER TABLE memory DROP CONSTRAINT IF EXISTS memory_user_key_unique;

    ALTER TABLE memory ALTER COLUMN user_id TYPE BIGINT USING user_id::BIGINT;

    ALTER TABLE memory ADD CONSTRAINT memory_user_key_unique UNIQUE (user_id, key);
  END IF;
END $$;

-- 4. Fix math_question.answer from real (float4) to double precision (float8)
DO $$ BEGIN
  IF (SELECT data_type FROM information_schema.columns
      WHERE table_name = 'math_question' AND column_name = 'answer') = 'real' THEN
    ALTER TABLE math_question ALTER COLUMN answer TYPE DOUBLE PRECISION USING answer::DOUBLE PRECISION;
  END IF;
END $$;

-- 5. Add FK memory.user_id -> "user".id with CASCADE delete
DO $$ BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint c
    JOIN pg_class t ON c.conrelid = t.oid
    WHERE t.relname = 'memory' AND c.contype = 'f'
  ) THEN
    -- Remove orphaned memories (users who left/were deleted) before adding FK
    DELETE FROM memory WHERE user_id NOT IN (SELECT id FROM "user");
    ALTER TABLE memory ADD CONSTRAINT memory_user_id_fkey
      FOREIGN KEY (user_id) REFERENCES "user"(id) ON DELETE CASCADE;
  END IF;
END $$;
