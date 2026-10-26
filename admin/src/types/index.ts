export type TrafficMode = 'internal' | 'split' | 'global';
export interface Capabilities {
  allow_ipv4_global: boolean;
  allow_ipv6_global: boolean;
  allow_ipv4_split: boolean;
  allow_ipv6_split: boolean;
}
export interface Session extends Capabilities {
  token: string;
  user_id: string;
  username: string;
  role: 'admin' | 'user';
  expires_at: string;
  must_change_password: boolean;
}
export interface User extends Capabilities {
  user_id: string;
  username: string;
  role: 'admin' | 'user';
  enabled: boolean;
  created_at: string;
  address_mode: 'automatic' | 'fixed';
  reserved_ipv4: string | null;
  reserved_ipv6: string | null;
  leased_ipv4: string | null;
  leased_ipv6: string | null;
  address_policy?: {
    ipv4: import('../api/addressPools').FamilyPolicy;
    ipv6: import('../api/addressPools').FamilyPolicy;
  } | null;
}
export interface CreateUser extends Capabilities {
  username: string;
  password: string;
  role: 'admin' | 'user';
}
export type AddressMode = 'automatic' | 'fixed';
export interface AddressLease {
  address_mode: AddressMode;
  reserved_ipv4: string | null;
  reserved_ipv6: string | null;
}
export interface Node {
  nodeId: string;
  name: string;
  probeEndpoint: string;
  region: string | null;
  serverPublicKey: string;
  wireguardInterface: string | null;
  routeTable: string | null;
  enabled: boolean;
  connectable: boolean;
  healthy: boolean;
  lastSeenAt: string;
  registered: boolean;
  wireguardEndpoint: string;
  rateLimit: NodeRateLimit;
  resourceVersion: string | null;
  desiredResourceVersion: string | null;
  network: NodeNetworkConfig;
}
export interface NodeNetworkConfig {
  pmmandns_geosite_enabled?: boolean;
  ipv4_split_routes?: string[] | null;
  ipv6_split_routes?: string[] | null;
  dns_servers: string[];
  pmmandns_upstreams: string[];
  pmmandns_domestic_upstreams: string[];
  pmmandns_foreign_upstreams: string[];
  pmmandns_domestic_rules: string[];
  pmmandns_foreign_rules: string[];
  pmmandns_default_group: string;
  pmmandns_rules: Array<{ suffix: string; upstreams: string[] }>;
  pmmandns_geosite?: {
    version: string;
    sha256: string;
    size: number;
    domestic: string;
    foreign: string;
  } | null;
  gateway_ipv4?: string | null;
  gateway_ipv6?: string | null;
}
export interface GeositeInfo {
  version: string;
  sha256: string;
  size: number;
  domestic: string;
  foreign: string;
  categories: string[];
}
export interface NodeSettings {
  name?: string;
  region?: string;
  wireguard_endpoint?: string;
  probe_endpoint?: string;
  route_table: string | null;
  enabled: boolean;
  connectable: boolean;
  rate_limit: NodeRateLimit;
  network?: NodeNetworkConfig;
}
export interface NodeBirdConfig {
  nodeId: string;
  interface: string | null;
  routeTable: string | null;
  ready: boolean;
  warnings: string[];
  fileName: string;
  config: string;
  ospfIpv4FilterSnippet: string;
  ospfIpv6FilterSnippet: string;
  installationSteps: string[];
  verificationCommands: string[];
}
export interface NodeRelease {
  version: string;
  fileName: string;
  sha256: string;
  size: number;
  publishedAt: string;
}
export interface ClientRelease {
  product: string;
  channel: 'stable' | 'beta';
  platform: 'windows';
  arch: 'x64';
  version: string;
  minSupportedVersion: string;
  mandatoryAfter: string | null;
  installerFile: string;
  size: number;
  sha256: string;
  signature: string;
  publishedAt: string;
  releaseNotes: string[];
  active: boolean;
}
export interface NodeRateLimit {
  uploadMbps: number;
  downloadMbps: number;
}
export interface NodeEnrollment {
  node: Node;
  node_token: string;
}
export interface CreateNode {
  node_id: string;
  name: string;
  region: string | null;
  route_table: string | null;
  network?: NodeNetworkConfig;
  connectable?: boolean;
  wireguard_interface?: string | null;
}
export interface Device {
  device_id: string;
  username: string;
  user_id: string;
  public_key: string;
  node_id: string;
  ipv4: string;
  ipv6: string;
  ipv4_mode: TrafficMode;
  ipv6_mode: TrafficMode;
  status: 'active' | 'revoked' | 'released';
  config_version: number;
  last_seen_at: string;
  traffic: TrafficStats;
}
export interface TrafficStats {
  nodeTxBytes: number;
  nodeRxBytes: number;
  p2pDirectTxBytes: number;
  p2pDirectRxBytes: number;
  p2pRelayTxBytes: number;
  p2pRelayRxBytes: number;
}
export type PortForwardProtocol = 'tcp' | 'udp';
export interface PortForwardRange {
  start: number;
  end: number;
}
export interface PortForwardEntry {
  entry_id: string;
  node_id: string;
  name: string;
  public_address: string;
  hostname: string | null;
  family: 'ipv4' | 'ipv6';
  protocols: PortForwardProtocol[];
  port_ranges: PortForwardRange[];
  reserved_ports: number[];
  enabled: boolean;
  node_healthy: boolean;
  total_ports: number;
  allocated_ports: number;
  available_ports: number;
}
export interface PortForward {
  forward_id: string;
  user_id: string;
  username: string;
  device_id: string;
  entry_id: string;
  node_id: string;
  protocols: PortForwardProtocol[];
  public_address: string;
  public_host: string | null;
  public_port: number;
  target_address: string;
  target_port: number;
  state: string;
  config_version: number;
  created_at: string;
  updated_at: string;
  expires_at: string | null;
  error_message: string | null;
}
export interface PortForwardPolicy {
  enabled: boolean;
  max_active_forwards: number;
  allowed_entry_ids: string[];
  allowed_protocols: PortForwardProtocol[];
  allowed_target_ports: number[] | null;
  allowed_families: ('ipv4' | 'ipv6')[];
}
export interface Pool {
  ipv4_pool: string;
  ipv6_pool: string;
  internal_cidrs: string[];
  capacity: number;
  allocated: number;
  available: number;
}
export const capabilityOptions = [
  { key: 'allow_ipv4_global', label: 'IPv4 全局' },
  { key: 'allow_ipv6_global', label: 'IPv6 全局' },
  { key: 'allow_ipv4_split', label: 'IPv4 分流' },
  { key: 'allow_ipv6_split', label: 'IPv6 分流' },
] as const;
export const emptyCapabilities = (): Capabilities => ({
  allow_ipv4_global: false,
  allow_ipv6_global: false,
  allow_ipv4_split: false,
  allow_ipv6_split: false,
});
export interface DiagnosticReport {
  report_id: string;
  user_id: string;
  username: string;
  uploaded_at: string;
  size: number;
}
