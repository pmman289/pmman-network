BEGIN;
CREATE TABLE IF NOT EXISTS invitation_policies (
    user_id TEXT PRIMARY KEY,
    payload TEXT NOT NULL,
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE IF NOT EXISTS invitations (
    id TEXT PRIMARY KEY,
    code_hash TEXT NOT NULL UNIQUE,
    prefix TEXT NOT NULL,
    inviter_user_id TEXT NOT NULL,
    inviter_username TEXT NOT NULL,
    creator_username TEXT NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('user','admin')),
    kind TEXT NOT NULL CHECK (kind IN ('single','unlimited')),
    note TEXT NOT NULL DEFAULT '',
    grant_payload TEXT,
    grant_revision BIGINT NOT NULL DEFAULT 1 CHECK (grant_revision > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    expires_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    use_count BIGINT NOT NULL DEFAULT 0 CHECK (use_count >= 0),
    CHECK (kind <> 'single' OR use_count <= 1),
    CHECK (source <> 'user' OR (kind = 'single' AND grant_payload IS NULL))
);
CREATE INDEX IF NOT EXISTS invitations_owner_created ON invitations(inviter_user_id,created_at DESC);
CREATE TABLE IF NOT EXISTS invitation_registrations (
    id TEXT PRIMARY KEY,
    invitation_id TEXT NOT NULL REFERENCES invitations(id),
    user_id TEXT NOT NULL UNIQUE,
    username TEXT NOT NULL UNIQUE,
    inviter_user_id TEXT NOT NULL,
    inviter_username TEXT NOT NULL,
    grant_payload TEXT NOT NULL,
    policy_revision BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS invitation_registrations_owner_created ON invitation_registrations(inviter_user_id,created_at DESC);
INSERT INTO schema_migrations(version) VALUES (3) ON CONFLICT(version) DO NOTHING;
COMMIT;
