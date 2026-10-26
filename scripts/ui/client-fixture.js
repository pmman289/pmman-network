// Browser-only UI acceptance fixture. Never imported by the application or shipped.
// This verifies rendering/interaction, not real WireGuard connectivity.
(() => {
  if (
    location.hostname !== "192.168.8.198" &&
    location.hostname !== "localhost" &&
    location.hostname !== "127.0.0.1"
  )
    return;
  const initial = () => ({
    p2pEnabled: true,
    p2pMessage: "按需直连已就绪，无法直连时自动中转",
    p2pPeers: [],
    state: "disconnected",
    authenticated: false,
    message: "尚未登录",
    nodes: [],
    nodeSelection: "auto",
    ipv4Mode: "internal",
    ipv6Mode: "internal",
    allowIpv4Global: false,
    allowIpv6Global: false,
    allowIpv4Split: false,
    allowIpv6Split: false,
    allowedIps: [],
    dnsServers: [],
  });
  const nodes = () => [
    {
      nodeId: "txgz-a",
      name: "TXGZ-A",
      region: "Guangzhou",
      wireguardEndpoint: "1.14.226.49:51830",
      probeEndpoint: "1.14.226.49:51840",
      healthy: true,
      latencyMs: 8,
    },
    {
      nodeId: "txgz-b",
      name: "TXGZ-B",
      region: "Guangzhou",
      wireguardEndpoint: "1.14.226.49:51831",
      probeEndpoint: "1.14.226.49:51841",
      healthy: true,
      latencyMs: 12,
    },
  ];
  let snapshot = initial();
  const controls = {
    calls: [],
    failNext: "",
    delay: 250,
    statusDelay: 0,
    set(value) {
      snapshot = { ...snapshot, ...value };
    },
    reset() {
      snapshot = initial();
      this.calls = [];
    },
  };
  window.__PMMAN_UI_FIXTURE__ = controls;
  window.isTauri = true;
  window.__TAURI_INTERNALS__ = {
    async invoke(command, args = {}) {
      controls.calls.push(command);
      const before = structuredClone(snapshot);
      await new Promise((resolve) =>
        setTimeout(
          resolve,
          command === "get_status" ? controls.statusDelay : controls.delay,
        ),
      );
      if (controls.failNext === command) {
        controls.failNext = "";
        throw new Error("验收夹具：请求超时，请重试。");
      }
      if (command === "get_status") return before;
      if (command === "login")
        snapshot = {
          ...initial(),
          authenticated: true,
          username: "pmman",
          role: "user",
          allowIpv4Global: true,
          allowIpv6Global: true,
          allowIpv4Split: true,
          allowIpv6Split: true,
          nodes: nodes(),
          message: "登录成功",
        };
      if (command === "list_nodes") snapshot.nodes = nodes();
      if (command === "set_p2p")
        snapshot = { ...snapshot, p2pEnabled: args.enabled };
      if (command === "select_node")
        snapshot = {
          ...snapshot,
          nodeSelection: args.nodeId ? "manual" : "auto",
          selectedNodeId: args.nodeId,
          selectedNodeName: nodes().find((node) => node.nodeId === args.nodeId)
            ?.name,
          message: "节点已更新",
        };
      if (command === "set_traffic_mode")
        snapshot = {
          ...snapshot,
          ipv4Mode: args.ipv4Mode,
          ipv6Mode: args.ipv6Mode,
          message: "流量模式已更新",
        };
      if (command === "connect")
        snapshot = {
          ...snapshot,
          state: "connected",
          tunnelMode: "wireguard",
          selectedNodeId: snapshot.selectedNodeId || "txgz-a",
          selectedNodeName: snapshot.selectedNodeName || "TXGZ-A",
          ipv4: "10.88.0.10/32",
          ipv6: "fd88:1000::a/128",
          allowedIps: ["10.88.0.0/24", "fd88:1000::/64"],
          dnsServers: [],
          message: "UI 夹具连接状态（不代表真实握手）",
        };
      if (command === "disconnect")
        snapshot = {
          ...snapshot,
          state: "disconnected",
          tunnelMode: null,
          ipv4: null,
          ipv6: null,
          allowedIps: [],
          dnsServers: [],
          message: "已断开",
        };
      if (command === "logout") snapshot = initial();
      return structuredClone(snapshot);
    },
  };
})();
