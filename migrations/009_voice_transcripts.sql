-- Claim before calling STT so duplicate gateway events and process restarts do not rebill the same clip.
CREATE TABLE IF NOT EXISTS voice_transcript (
    message_id BIGINT PRIMARY KEY,
    attachment_id BIGINT NOT NULL,
    channel_id BIGINT NOT NULL,
    model TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('processing', 'ready', 'failed')),
    transcript TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS voice_transcript_channel ON voice_transcript (channel_id, message_id DESC);
