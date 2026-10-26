# Development and acceptance environment

- Build Windows artifacts locally using MSVC; build/test Linux master and nodes
  on local `ssh wsl` with the static musl target. Do not compile on the low-memory
  remote host. Use `ssh txgz` for service deployment and real public acceptance.
- Remote project directory: `/opt/pmman-network-dev`; use only project-prefixed
  systemd services, gateway interfaces and reverse-proxy files. Preserve the
  host's existing WireGuard, BIRD, routing, DNS, and website configuration.
- Windows clients exchange login, configuration, heartbeat and revoke only with
  the public HTTPS master. Nodes expose WireGuard and challenge-echo UDP probes,
  have no HTTP server and poll the master with a node credential. Never forward
  user bearer tokens to nodes or measure node latency through HTTP. Loopback
  relays and WSL gateways do not establish public acceptance.
- Use `scripts/deploy-txgz.ps1` to deploy and `scripts/verify-txgz.ps1` for local
  Windows acceptance. Record results and limitations in `docs/PROGRESS.md`.
- Credentials and generated deployment metadata belong in ignored `artifacts/`.
  Never copy secrets into docs, logs, source, or source archives.
- The admin web application lives in `admin/`, uses Vue 3/TypeScript/Element Plus,
  and follows `docs/ADMIN-DEVELOPMENT.md`. Use library components for layouts,
  tables, forms and dialogs; keep API access in the typed API layer. Run admin
  lint/type checks/build locally before building the embedded Rust master.
- The desktop client follows `docs/CLIENT-DEVELOPMENT.md`, with typed IPC state,
  shared theme tokens and accessible pending/error states. Browser fixtures stay
  outside the application bundle and never establish real network acceptance.
