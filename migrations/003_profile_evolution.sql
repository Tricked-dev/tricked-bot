CREATE TABLE IF NOT EXISTS profile_candidate (
    id             BIGSERIAL PRIMARY KEY,
    user_id        BIGINT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    field          TEXT NOT NULL CHECK (field IN ('relationship', 'example')),
    proposed_value TEXT NOT NULL,
    confirmations  INT NOT NULL DEFAULT 1 CHECK (confirmations > 0),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, field)
);
