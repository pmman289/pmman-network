import { computed, onMounted, onUnmounted, ref } from "vue";
import { invoke, isTauri } from "@tauri-apps/api/core";
import type {
  ClientSnapshot,
  PortForwardProtocol,
  TrafficMode,
} from "../types/client";
import { APP_VERSION } from "../version";
import type { InvitationRequest, InvitationResult } from "../../shared/invitations";

type Command =
  | "invitation_request"
  | "check_update"
  | "upload_diagnostics"
  | "download_update"
  | "install_update"
  | "login"
  | "change_password"
  | "connect"
  | "disconnect"
  | "logout"
  | "list_nodes"
  | "select_node"
  | "set_traffic_mode"
  | "set_p2p"
  | "list_port_forward_entries"
  | "list_port_forward_quota"
  | "list_port_forwards"
  | "create_port_forward"
  | "release_port_forward";
// P2P can change while connected; it never changes the tunnel or virtual IP.

const initialSnapshot = (): ClientSnapshot => ({
  clientUpdate: {
    releaseNotes: [],
    currentVersion: APP_VERSION,
    availableVersion: null,
    state: "idle",
    progress: 0,
    message: "",
    ready: false,
    installerPath: null,
    sha256: null,
    mandatory: false,
  },
  authRestorePending: true,
  p2pEnabled: true,
  p2pMessage: "",
  p2pPeers: [],
  portForwardEntries: [],
  portForwardQuota: { enabled: false, limit: 0, used: 0, available: 0 },
  portForwards: [],
  portForwardMessage: "",
  portForwardFirewallWarning: "",
  diagnosticUploadState: "idle",
  diagnosticUploadMessage: "",
  diagnosticUploadId: null,
  state: "disconnected",
  connectionWaitingCleanup: false,
  authenticated: false,
  message: "未登录",
  nodes: [],
  nodeSelection: "auto",
  ipv4Mode: "internal",
  ipv6Mode: "internal",
  allowIpv4Global: false,
  allowIpv6Global: false,
  allowIpv4Split: false,
  allowIpv6Split: false,
  allowedIps: [],
  splitRouteSource: null,
  splitRouteVersion: null,
  splitRouteSnapshot: null,
  splitIpv4PrefixCount: 0,
  splitIpv6PrefixCount: 0,
  splitRouteInstallMs: 0,
  nodeDnsServers: [],
  dnsServers: [],
  dnsSplitEnabled: false,
  nodeRateLimit: { uploadMbps: 0, downloadMbps: 0 },
  trafficStats: {
    nodeTxBytes: 0,
    nodeRxBytes: 0,
    p2pDirectTxBytes: 0,
    p2pDirectRxBytes: 0,
    p2pRelayTxBytes: 0,
    p2pRelayRxBytes: 0,
  },
});

export function useClient() {
  const status = ref<ClientSnapshot>(initialSnapshot());
  const operation = ref<Command | null>(null);
  const starting = ref(true);
  const syncing = ref(false);
  const notice = ref<{ kind: "error" | "info"; text: string } | null>(null);
  const lastUpdated = ref<Date | null>(null);
  const connectedAt = ref<number | null>(null);
  const now = ref(Date.now());
  const cancellingConnection = ref(false);
  let version = 0;
  let disposed = false;
  let timer: ReturnType<typeof setInterval> | undefined;
  let nodesTimer: ReturnType<typeof setInterval> | undefined;
  let elapsedTimer: ReturnType<typeof setInterval> | undefined;
  let lastReportedError = "";

  const busy = computed(() => starting.value || operation.value !== null);
  const connected = computed(
    () =>
      status.value.state === "connected" &&
      status.value.tunnelMode === "wireguard",
  );
  const connecting = computed(() => operation.value === "connect");
  const disconnecting = computed(
    () => operation.value === "disconnect" || operation.value === "logout",
  );
  const locked = computed(() => busy.value || connected.value || syncing.value);
  const selectedNode = computed(() =>
    status.value.nodes.find(
      (node) => node.nodeId === status.value.selectedNodeId,
    ),
  );
  const availableNodes = computed(() =>
    status.value.nodes.filter((node) => node.healthy),
  );
  const elapsed = computed(() => {
    if (connectedAt.value === null) return "—";
    const seconds = Math.max(
      0,
      Math.floor((now.value - connectedAt.value) / 1000),
    );
    return [
      Math.floor(seconds / 3600),
      Math.floor(seconds / 60) % 60,
      seconds % 60,
    ]
      .map((value) => String(value).padStart(2, "0"))
      .join(":");
  });

  function apply(snapshot: ClientSnapshot) {
    const previouslyConnected = connected.value;
    const defaults = initialSnapshot();
    status.value = {
      ...snapshot,
      connectionWaitingCleanup: snapshot.connectionWaitingCleanup ?? false,
      portForwardFirewallWarning: snapshot.portForwardFirewallWarning ?? "",
      trafficStats: snapshot.trafficStats ?? defaults.trafficStats,
      portForwardQuota: snapshot.portForwardQuota ?? defaults.portForwardQuota,
      diagnosticUploadState:
        snapshot.diagnosticUploadState ?? defaults.diagnosticUploadState,
      diagnosticUploadMessage:
        snapshot.diagnosticUploadMessage ?? defaults.diagnosticUploadMessage,
      diagnosticUploadId:
        snapshot.diagnosticUploadId ?? defaults.diagnosticUploadId,
    };
    lastUpdated.value = new Date();
    if (connected.value && !previouslyConnected) connectedAt.value = Date.now();
    if (!connected.value) connectedAt.value = null;
    if (snapshot.state === "error" && snapshot.message !== lastReportedError) {
      notice.value = { kind: "error", text: snapshot.message };
      lastReportedError = snapshot.message;
    } else if (snapshot.state !== "error") {
      lastReportedError = "";
    }
    if (
      previouslyConnected &&
      !connected.value &&
      snapshot.message.includes("撤销")
    ) {
      notice.value = { kind: "info", text: snapshot.message };
    }
  }

  function errorText(error: unknown): string {
    if (typeof error === "string") return error;
    if (error instanceof Error) return error.message;
    return "操作失败，请稍后重试。";
  }

  async function request(command: string, args?: Record<string, unknown>) {
    if (!isTauri()) throw new Error("请在 Pmman Network 中使用此功能。");
    return invoke<ClientSnapshot>(command, args);
  }

  async function refresh() {
    if ((operation.value && operation.value !== "connect") || syncing.value || disposed) return;
    syncing.value = true;
    const currentVersion = version;
    let received = false;
    try {
      const snapshot = await request("get_status");
      if (!disposed && currentVersion === version) {
        received = true;
        // Ignore the pre-command/final snapshot while connect owns the result.
        // Only read its shared progress; an older poll cannot replace completion.
        if (operation.value !== "connect" || snapshot.state === "connecting") apply(snapshot);
      }
    } catch (error) {
      if (!disposed && currentVersion === version && operation.value !== "connect")
        notice.value = { kind: "error", text: errorText(error) };
    } finally {
      syncing.value = false;
      if (received) starting.value = status.value.authRestorePending;
      else starting.value = false;
    }
  }

  async function run(
    command: Command,
    args?: Record<string, unknown>,
    accepted?: (value: ClientSnapshot) => boolean,
  ) {
    if (busy.value || disposed) return false;
    operation.value = command;
    version++;
    notice.value = null;
    try {
      const snapshot = await request(command, args);
      if (disposed) return false;
      apply(snapshot);
      if (accepted && !accepted(snapshot)) {
        notice.value = {
          kind: "error",
          text: snapshot.message || "操作失败，请重试。",
        };
        return false;
      }
      return true;
    } catch (error) {
      if (!disposed) notice.value = { kind: "error", text: errorText(error) };
      return false;
    } finally {
      version++;
      cancellingConnection.value = false;
      operation.value = null;
    }
  }

  function login(username: string, password: string) {
    return run(
      "login",
      { username: username.trim(), password },
      (value) => value.authenticated,
    );
  }
  async function invitationRequest(request: InvitationRequest): Promise<InvitationResult> {
    if (busy.value || disposed) throw new Error("请等待当前操作完成");
    operation.value = "invitation_request";
    version++;
    try {
      if (!isTauri()) throw new Error("请在 Pmman Network 中使用此功能");
      const result = await invoke<InvitationResult>("invitation_request", { request });
      if (disposed) throw new Error("窗口已关闭");
      return result;
    } finally { operation.value = null; }
  }
  function changePassword(currentPassword: string, newPassword: string) {
    return run("change_password", { currentPassword, newPassword });
  }
  function toggleConnection() {
    if (connected.value)
      return run(
        "disconnect",
        undefined,
        (value) => value.state === "disconnected",
      );
    return run(
      "connect",
      undefined,
      (value) =>
        (value.state === "connected" && value.tunnelMode === "wireguard") ||
        (value.state === "disconnected" && value.message === "已取消连接"),
    );
  }
  async function cancelConnection() {
    if (!connecting.value || !status.value.connectionWaitingCleanup || cancellingConnection.value) return;
    cancellingConnection.value = true;
    try {
      await invoke<void>("cancel_connect");
    } catch (error) {
      if (!disposed) notice.value = { kind: "error", text: errorText(error) };
      cancellingConnection.value = false;
    }
  }
  function selectNode(nodeId: string | null) {
    if (locked.value) return Promise.resolve(false);
    return run("select_node", { nodeId }, (value) =>
      nodeId === null
        ? value.nodeSelection === "auto"
        : value.nodeSelection === "manual" && value.selectedNodeId === nodeId,
    );
  }
  function setMode(family: "ipv4" | "ipv6", mode: TrafficMode) {
    if (locked.value) return Promise.resolve(false);
    const ipv4Mode = family === "ipv4" ? mode : status.value.ipv4Mode;
    const ipv6Mode = family === "ipv6" ? mode : status.value.ipv6Mode;
    return run(
      "set_traffic_mode",
      { ipv4Mode, ipv6Mode },
      (value) => value.ipv4Mode === ipv4Mode && value.ipv6Mode === ipv6Mode,
    );
  }
  function refreshNodes() {
    if (locked.value) return Promise.resolve(false);
    return run("list_nodes");
  }
  function logout() {
    return run("logout", undefined, (value) => !value.authenticated);
  }
  function setP2p(enabled: boolean) {
    return run("set_p2p", { enabled }, (value) => value.p2pEnabled === enabled);
  }
  function refreshPortForwardEntries() {
    return run("list_port_forward_entries");
  }
  function refreshPortForwardQuota() {
    return run("list_port_forward_quota");
  }
  function refreshPortForwards() {
    return run("list_port_forwards");
  }
  async function createPortForward(
    entryId: string | null,
    protocols: PortForwardProtocol[],
    targetPort: number,
  ) {
    const created = await run(
      "create_port_forward",
      { entryId, protocols, targetPort },
      (value) => value.portForwards.some((item) => item.state === "active"),
    );
    const failure = notice.value;
    await refreshPortForwardQuota();
    if (!created) notice.value = failure;
    return created;
  }
  async function releasePortForward(forwardId: string) {
    const released = await run(
      "release_port_forward",
      { forwardId },
      (value) =>
        !value.portForwards.some((item) => item.forward_id === forwardId),
    );
    const failure = notice.value;
    await refreshPortForwardQuota();
    if (!released) notice.value = failure;
    return released;
  }

  async function checkUpdate(channel = "stable") {
    if (disposed || operation.value || syncing.value) return false;
    try {
      apply(await request("check_update", { channel }));
      return true;
    } catch (error) {
      if (!disposed) notice.value = { kind: "error", text: errorText(error) };
      return false;
    }
  }

  async function uploadDiagnostics() {
    if (disposed || operation.value || !status.value.authenticated) return false;
    operation.value = "upload_diagnostics";
    notice.value = null;
    try {
      const snapshot = await request("upload_diagnostics");
      if (disposed) return false;
      apply(snapshot);
      return snapshot.diagnosticUploadState === "uploaded";
    } catch (error) {
      if (!disposed) notice.value = { kind: "error", text: errorText(error) };
      return false;
    } finally {
      operation.value = null;
    }
  }

  function downloadUpdate(channel = "stable") {
    return run(
      "download_update",
      { channel },
      (value) =>
        value.clientUpdate.state === "downloading" || value.clientUpdate.ready,
    );
  }

  async function installUpdate() {
    if (
      !status.value.clientUpdate.ready ||
      busy.value ||
      disposed ||
      connected.value
    )
      return false;
    operation.value = "install_update";
    try {
      await invoke("install_update");
      return true;
    } catch (error) {
      if (!disposed) notice.value = { kind: "error", text: errorText(error) };
      return false;
    } finally {
      operation.value = null;
    }
  }

  onMounted(async () => {
    await refresh();
    void checkUpdate();
    if (disposed) return;
    timer = setInterval(() => {
      void refresh();
    }, 5000);
    // Status polling is intentionally local and cheap.  The node-list request
    // is the control-plane refresh point: it also lets the service recover a
    // bearer token after a master restart and receive the latest IPv4/IPv6
    // authorization flags.  Never overlap it with a user operation or an
    // active tunnel.
    nodesTimer = setInterval(() => {
      if (
        status.value.authenticated &&
        !connected.value &&
        !operation.value &&
        !syncing.value
      ) {
        void refreshNodes();
      }
    }, 15000);
    elapsedTimer = setInterval(() => {
      now.value = Date.now();
    }, 1000);
  });
  onUnmounted(() => {
    disposed = true;
    version++;
    if (timer) clearInterval(timer);
    if (nodesTimer) clearInterval(nodesTimer);
    if (elapsedTimer) clearInterval(elapsedTimer);
  });

  return {
    status,
    operation,
    starting,
    syncing,
    notice,
    lastUpdated,
    busy,
    connected,
    connecting,
    cancellingConnection,
    cancelConnection,
    disconnecting,
    locked,
    selectedNode,
    availableNodes,
    elapsed,
    refresh,
    login,
    invitationRequest,
    changePassword,
    toggleConnection,
    selectNode,
    setMode,
    refreshNodes,
    logout,
    setP2p,
    refreshPortForwardEntries,
    refreshPortForwardQuota,
    refreshPortForwards,
    createPortForward,
    releasePortForward,
    checkUpdate,
    uploadDiagnostics,
    downloadUpdate,
    installUpdate,
  };
}
