-- Where each recording's files are in R2: rec/<device>/<when>_<id>/ (see
-- worker/index.ts).
ALTER TABLE shares ADD COLUMN folder TEXT;
