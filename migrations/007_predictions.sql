-- Predictions the bot made in replies, checked back on once due.
CREATE TABLE IF NOT EXISTS prediction (
    id BIGSERIAL PRIMARY KEY,
    channel_id BIGINT NOT NULL,
    -- The message the bot was answering when it made the prediction; the follow-up replies to it.
    message_id BIGINT NOT NULL,
    user_id BIGINT NOT NULL,
    claim TEXT NOT NULL,
    made_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    due_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS prediction_due_at ON prediction (due_at);
