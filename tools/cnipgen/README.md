# Built-in split routes

This directory contains the reproducible generator for the client-owned
overseas route snapshot. The normal client build reads only the checked-in
files under `service/resources/cnipgen`; it never downloads route data.

The generator uses the `nchnroutes` fallback produced by the upstream
`cnipgen` workflow for IPv4 and APNIC delegated data for IPv6. It emits only
CIDR prefixes. BIRD communities, next hops, regional exits, and route-table
metadata are intentionally discarded.

To refresh the snapshot deliberately from WSL:

```bash
python3 tools/cnipgen/generate_split_routes.py --refresh
```

The command writes temporary candidates, validates family, ordering,
deduplication, special-use exclusions, and non-default-route invariants, and
only then replaces the snapshot and metadata. A failed download or validation
never replaces the last valid snapshot.

The generated metadata records the generator version, source URLs, source
dates, prefix counts, and SHA-256 hashes. `service/src/split_routes.rs`
re-validates those hashes before a connection uses the snapshot.
