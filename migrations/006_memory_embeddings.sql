-- Content embeddings for recall. Rows are re-embedded by a background worker whenever these are NULL or the
-- configured embedding model changes.
ALTER TABLE memory ADD COLUMN IF NOT EXISTS embedding real[];
ALTER TABLE memory ADD COLUMN IF NOT EXISTS embedding_model text;
