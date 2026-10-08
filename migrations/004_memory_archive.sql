CREATE TABLE IF NOT EXISTS memory_archive (
    archive_id BIGSERIAL PRIMARY KEY,
    memory_id BIGINT NOT NULL,
    user_id BIGINT NOT NULL,
    key TEXT NOT NULL,
    content TEXT NOT NULL,
    archived_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    reason TEXT NOT NULL
);
