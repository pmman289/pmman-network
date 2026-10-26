export type TrafficMode = "internal" | "split" | "global";
export type ClientPage = "connection" | "nodes" | "peers" | "forwards" | "settings";
export type PortForwardProtocol = "tcp" | "udp";
export type ThemePreference = "system" | "light" | "dark";

export interface NodeRateLimit {
  uploadMbps: number;
  downloadMbps: number;
}

export interface TrafficStats {
  nodeTxBytes: number;
  nodeRxBytes: number;
  p2pDirectTxBytes: number;
  p2pDirectRxBytes: number;
  p2pRelayTxBytes: number;
  p2pRelayRxBytes: number;
}

export interface PortForwardQuota {
  enabled: boolean;
  limit: number;
  used: number;
  available: number;
}

export interface NodeInfo {
  nodeId: string;
  name: string;
  wireguardEndpoint: string;
  probeEndpoint: string;
  region?: string | null;
  healthy: boolean;
  latencyMs?: number | null;
  rateLimit: NodeRateLimit;
}

export interface ClientUpdate {
  releaseNotes: string[];
  currentVersion: string;
  availableVersion?: string | null;
  state:
    | "idle"
    | "checking"
    | "current"
    | "available"
    | "downloading"
    | "ready"
    | "error";
  progress: number;
  message: string;
  ready: boolean;
  installerPath?: string | null;
  sha256?: string | null;
  mandatory: boolean;
}

export interface ClientSnapshot {
  clientUpdate: ClientUpdate;
  authRestorePending: boolean;
  p2pEnabled: boolean;
  p2pMessage: string;
  p2pPeers: PeerStatus[];
  state: string;
  connectionWaitingCleanup: boolean;
  authenticated: boolean;
  userId?: string | null;
  username?: string | null;
  role?: string | null;
  deviceId?: string | null;
  ipv4?: string | null;
  ipv6?: string | null;
  message: string;
  tunnelMode?: string | null;
  nodes: NodeInfo[];
  selectedNodeId?: string | null;
  selectedNodeName?: string | null;
  nodeSelection: string;
  ipv4Mode: TrafficMode;
  ipv6Mode: TrafficMode;
  allowIpv4Global: boolean;
  allowIpv6Global: boolean;
  allowIpv4Split: boolean;
  allowIpv6Split: boolean;
  allowedIps: string[];
  splitRouteSource?: string | null;
  splitRouteVersion?: string | null;
  splitRouteSnapshot?: string | null;
  splitIpv4PrefixCount: number;
  splitIpv6PrefixCount: number;
  splitRouteInstallMs: number;
  nodeDnsServers: string[];
  dnsServers: string[];
  dnsSplitEnabled: boolean;
  nodeRateLimit: NodeRateLimit;
  trafficStats: TrafficStats;
  portForwardEntries: PortForwardEntry[];
  portForwardQuota: PortForwardQuota;
  portForwards: PortForward[];
  portForwardMessage: string;
  portForwardFirewallWarning: string;
  diagnosticUploadState: string;
  diagnosticUploadMessage: string;
  diagnosticUploadId: string | null;
}

export interface PortForwardRange {
  start: number;
  end: number;
}
export interface PortForwardEntry {
  entry_id: string;
  node_id: string;
  name: string;
  public_address: string;
  hostname?: string | null;
  family: "ipv4" | "ipv6";
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
  public_host?: string | null;
  public_port: number;
  target_address: string;
  target_port: number;
  state: string;
  config_version: number;
  created_at: string;
  updated_at: string;
  expires_at?: string | null;
  error_message?: string | null;
}

export interface PeerStatus {
  deviceId: string;
  username: string;
  publicKey: string;
  ipv4: string;
  ipv6: string;
  path: "idle" | "probing" | "direct" | "relay";
  directEndpoint?: string | null;
  latencyMs?: number | null;
  directRxBytes: number;
  directTxBytes: number;
  relayRxBytes: number;
  relayTxBytes: number;
  fallbackCount: number;
  lastActivityMs?: number | null;
}

export const modeLabels: Record<TrafficMode, string> = {
  internal: "仅内部网络",
  split: "境外分流",
  global: "全局网络",
};

export const modeDescriptions: Record<TrafficMode, string> = {
  internal: "内网流量通过节点，公网保持直连。",
  split: "内网和指定境外网段通过节点，其他流量直连。",
  global: "该地址族的所有流量通过节点。",
};
