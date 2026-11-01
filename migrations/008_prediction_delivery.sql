-- A claim is a lease, so interrupted or failed follow-ups remain recoverable.
ALTER TABLE prediction ADD COLUMN IF NOT EXISTS retry_at TIMESTAMPTZ NOT NULL DEFAULT now();
ALTER TABLE prediction ADD COLUMN IF NOT EXISTS claim_version BIGINT NOT NULL DEFAULT 0;
