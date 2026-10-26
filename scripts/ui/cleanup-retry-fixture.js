// Browser-only registration-wait fixture. Never imported by the application.
(() => {
  if (!['localhost', '127.0.0.1'].includes(location.hostname)) return;
  let snapshot = {
    clientUpdate: { currentVersion: 'ui-fixture', releaseNotes: [], state: 'current',
      progress: 0, message: '', ready: false, mandatory: false },
    authRestorePending: false, authenticated: true, userId: 'fixture-user',
    username: 'UI 验收', role: 'user', state: 'disconnected', message: '未连接',
    p2pEnabled: true, p2pMessage: '', p2pPeers: [],
    nodes: [{ nodeId: 'fixture-node', name: '界面验收节点', region: '测试',
      endpoint: '127.0.0.1:51820', probeEndpoint: '127.0.0.1:51821',
      healthy: true, latencyMs: 10, rateLimit: { uploadMbps: 0, downloadMbps: 0 } }],
    nodeSelection: 'auto', ipv4Mode: 'internal', ipv6Mode: 'internal',
    allowIpv4Global: false, allowIpv6Global: false, allowIpv4Split: false,
    allowIpv6Split: false, allowedIps: [], nodeDnsServers: [], dnsServers: [],
    dnsSplitEnabled: false, splitIpv4PrefixCount: 0, splitIpv6PrefixCount: 0,
    splitRouteInstallMs: 0, nodeRateLimit: { uploadMbps: 0, downloadMbps: 0 },
    trafficStats: { nodeTxBytes: 0, nodeRxBytes: 0, p2pDirectTxBytes: 0,
      p2pDirectRxBytes: 0, p2pRelayTxBytes: 0, p2pRelayRxBytes: 0 },
    portForwardEntries: [], portForwards: [], portForwardMessage: '',
    portForwardQuota: { enabled: false, limit: 0, used: 0, available: 0 },
    diagnosticUploadState: 'idle', diagnosticUploadMessage: '', diagnosticUploadId: null,
  };
  const controls = { calls: [], set(value) { snapshot = { ...snapshot, ...value }; } };
  window.__PMMAN_UI_FIXTURE__ = controls;
  window.isTauri = true;
  let finish;
  let mode = 'success';
  controls.cleanup = {
    setMode(value) { mode = value; },
    finish() { finish?.(); },
  };
  window.__TAURI_INTERNALS__ = { invoke: async (command) => {
    controls.calls.push(command);
    if (command === 'connect') {
      controls.set({ state: 'connecting', connectionWaitingCleanup: true,
        message: '正在切换地址，等待旧连接清理…' });
      await new Promise(resolve => { finish = resolve; });
      if (mode === 'cancel') {
        controls.set({ state: 'disconnected', connectionWaitingCleanup: false,
          message: '已取消连接' });
        return structuredClone(snapshot);
      }
      if (mode === 'timeout') {
        controls.set({ state: 'error', connectionWaitingCleanup: false,
          message: '旧连接清理尚未完成，请稍后重试或联系管理员' });
        return structuredClone(snapshot);
      }
      controls.set({ state: 'connected', tunnelMode: 'wireguard',
        connectionWaitingCleanup: false, message: '界面夹具连接（非真实隧道）',
        selectedNodeId: 'fixture-node', selectedNodeName: '界面验收节点',
        ipv4: '192.0.2.1', ipv6: '2001:db8::1' });
      return structuredClone(snapshot);
    }
    if (command === 'cancel_connect') {
      mode = 'cancel';
      finish?.();
      return;
    }
    if (command === 'disconnect') controls.set({ state: 'disconnected', tunnelMode: null, message: '已断开' });
    return structuredClone(snapshot);
  } };
})();
