BEGIN;
CREATE TABLE IF NOT EXISTS address_pools (
    pool_id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, description TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS address_pool_segments (
    pool_id TEXT NOT NULL REFERENCES address_pools ON DELETE CASCADE,
    segment CIDR NOT NULL, PRIMARY KEY(pool_id, segment)
);
CREATE TABLE IF NOT EXISTS address_pool_exclusions (
    pool_id TEXT NOT NULL REFERENCES address_pools ON DELETE CASCADE,
    segment CIDR NOT NULL, PRIMARY KEY(pool_id, segment)
);
CREATE TABLE IF NOT EXISTS address_pool_defaults (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK(singleton),
    ipv4_pool_id TEXT NOT NULL REFERENCES address_pools,
    ipv6_pool_id TEXT NOT NULL REFERENCES address_pools, revision BIGINT NOT NULL
);
CREATE TABLE IF NOT EXISTS user_address_policies (
    user_id TEXT PRIMARY KEY,
    ipv4_mode TEXT NOT NULL CHECK(ipv4_mode IN ('automatic','pool_fixed','manual')),
    ipv4_pool_id TEXT REFERENCES address_pools, ipv4_address INET,
    ipv6_mode TEXT NOT NULL CHECK(ipv6_mode IN ('automatic','pool_fixed','manual')),
    ipv6_pool_id TEXT REFERENCES address_pools, ipv6_address INET
);
CREATE TABLE IF NOT EXISTS address_leases (
    lease_id UUID PRIMARY KEY, user_id TEXT NOT NULL,
    address INET NOT NULL CHECK(masklen(address)=CASE family(address) WHEN 4 THEN 32 ELSE 128 END),
    pool_id TEXT REFERENCES address_pools ON DELETE SET NULL,
    state TEXT NOT NULL CHECK(state IN ('allocated','reserved','releasing','released')),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE UNIQUE INDEX IF NOT EXISTS address_leases_live_address ON address_leases(address) WHERE state <> 'released';
CREATE INDEX IF NOT EXISTS address_leases_owner ON address_leases(user_id);
-- A released/expired forwarding lease alone is not proof that an offline
-- entrance node has removed its old rule. Reuse requires its explicit ACK.
CREATE TABLE IF NOT EXISTS address_forward_cleanup (
    device_id UUID NOT NULL, forward_id TEXT NOT NULL, node_id TEXT NOT NULL,
    acknowledged BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY(device_id,forward_id)
);
INSERT INTO schema_migrations(version) VALUES(5) ON CONFLICT(version) DO NOTHING;
COMMIT;
