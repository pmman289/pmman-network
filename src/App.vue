<script setup lang="ts">
import { computed, nextTick, onUnmounted, ref, watch } from "vue";
import {
  ArrowRight,
  Check,
  ChevronDown,
  ChevronRight,
  CircleHelp,
  Clock3,
  Copy,
  Globe2,
  KeyRound,
  Layers3,
  LoaderCircle,
  LogOut,
  Monitor,
  Moon,
  Network,
  Power,
  Radio,
  RefreshCw,
  Server,
  Settings2,
  ShieldCheck,
  SlidersHorizontal,
  Sparkles,
  Sun,
  UserRound,
  Upload,
  Wifi,
  X,
  Zap,
} from "@lucide/vue";
import { useClient } from "./composables/useClient";
import StatusNotice from "./components/StatusNotice.vue";
import RegistrationForm from "./components/RegistrationForm.vue";
import InvitationPanel from "./components/InvitationPanel.vue";
import {
  forwardStateLabel,
  p2pSummary,
  updateStateLabel,
} from "./utils/presentation";
import { useTheme } from "./composables/useTheme";
import { APP_VERSION } from "./version";
import { modeDescriptions, modeLabels } from "./types/client";
import type {
  ClientPage,
  PortForwardProtocol,
  ThemePreference,
  TrafficMode,
} from "./types/client";

const client = useClient();
const {
  status,
  operation,
  starting,
  busy,
  connected,
  connecting,
  cancellingConnection,
  disconnecting,
  locked,
  notice,
  selectedNode,
  availableNodes,
  elapsed,
  lastUpdated,
  checkUpdate,
  downloadUpdate,
  installUpdate,
  uploadDiagnostics,
} = client;
const { preference, dark } = useTheme();
const appVersion = APP_VERSION;
const page = ref<ClientPage>("connection");
const settingsSection = ref<"network" | "appearance" | "about">("network");
const username = ref("");
const password = ref("");
const registrationOpen = ref(false);
const invitationDialog = ref<HTMLDialogElement>();
const invitationsOpen = ref(false);
function registered(value: string) {
  username.value=value;password.value="";registrationOpen.value=false;
  notice.value={kind:"info",text:"注册成功，请登录"};
}
async function openInvitations() {
  account.value?.close();invitationsOpen.value=true;
  await nextTick();invitationDialog.value?.showModal();
}
const showPassword = ref(false);
const passwordChangeOpen = ref(false);
const currentPassword = ref("");
const newPassword = ref("");
const confirmPassword = ref("");
const passwordFeedback = ref("");
const details = ref<HTMLDialogElement>();
const account = ref<HTMLDialogElement>();
const updateDialog = ref<HTMLDialogElement>();
const pageHeading = ref<HTMLElement>();
const copyFeedback = ref("");
const modeRevision = ref(0);
const promptedUpdateVersion = ref("");
const forwardEntryId = ref<string | null>(null);
const forwardProtocols = ref<PortForwardProtocol[]>(["tcp"]);
const forwardTargetPort = ref<number | null>(null);
const forwardRefreshing = ref(false);
const forwardCreateOpen = ref(false);
const lastDiagnosticError = ref("");
const diagnosticUploadFeedback = ref("");
watch(notice, (value) => {
  if (value?.kind === "error") lastDiagnosticError.value = value.text;
});
const diagnosticText = computed(() =>
  JSON.stringify(
    {
      product: "Pmman Network",
      version: appVersion,
      capturedAt: new Date().toISOString(),
      state: status.value.state,
      node: status.value.selectedNodeName ?? null,
      deviceId: status.value.deviceId ?? null,
      ipv4: status.value.ipv4 ?? null,
      ipv6: status.value.ipv6 ?? null,
      allowedIps: status.value.allowedIps,
      splitRouteSource: status.value.splitRouteSource,
      splitRouteVersion: status.value.splitRouteVersion,
      splitRouteSnapshot: status.value.splitRouteSnapshot,
      splitIpv4PrefixCount: status.value.splitIpv4PrefixCount,
      splitIpv6PrefixCount: status.value.splitIpv6PrefixCount,
      splitRouteInstallMs: status.value.splitRouteInstallMs,
      nodeDns: status.value.nodeDnsServers,
      dns: status.value.dnsServers,
      p2pEnabled: status.value.p2pEnabled,
      serviceMessage: status.value.message,
      p2pMessage: status.value.p2pMessage,
      portForwardMessage: status.value.portForwardMessage,
      updateMessage: status.value.clientUpdate.message,
      lastError: lastDiagnosticError.value,
    },
    null,
    2,
  ),
);
let copyVersion = 0;

const forwardEntryLatency = (entry: { node_id: string }) =>
  status.value.nodes.find((node) => node.nodeId === entry.node_id)?.latencyMs ??
  null;
const sortedForwardEntries = computed(() =>
  [...status.value.portForwardEntries].sort(
    (left, right) =>
      (forwardEntryLatency(left) ?? Number.POSITIVE_INFINITY) -
        (forwardEntryLatency(right) ?? Number.POSITIVE_INFINITY) ||
      Number(right.node_healthy) - Number(left.node_healthy) ||
      left.name.localeCompare(right.name),
  ),
);
const automaticForwardEntryId = computed(
  () =>
    sortedForwardEntries.value.find(
      (entry) => entry.node_healthy && entry.available_ports > 0,
    )?.entry_id ?? null,
);

const navigation = [
  { id: "connection" as const, label: "网络连接", icon: ShieldCheck },
  { id: "nodes" as const, label: "连接节点", icon: Server },
  { id: "peers" as const, label: "设备互联", icon: Network },
  { id: "forwards" as const, label: "端口转发", icon: Layers3 },
  { id: "settings" as const, label: "偏好设置", icon: Settings2 },
];
const titles: Record<ClientPage, { title: string; description: string }> = {
  connection: {
    title: "网络连接",
    description: "安全访问内部网络。",
  },
  nodes: {
    title: "连接节点",
    description: "选择节点，或交给系统自动选择。",
  },
  peers: { title: "设备互联", description: "查看在线设备与连接方式。" },
  forwards: {
    title: "端口转发",
    description: "将公网入口端口转发到当前设备。",
  },
  settings: {
    title: "偏好设置",
    description: "连接、网络和外观设置。",
  },
};
const connectionLabel = computed(() =>
  connecting.value
    ? "正在连接"
    : disconnecting.value
      ? "正在断开"
      : connected.value
        ? "已连接"
        : status.value.state === "error"
          ? "连接失败"
          : "未连接",
);
const buttonLabel = computed(() =>
  connecting.value
    ? "连接中…"
    : disconnecting.value
      ? "断开中…"
      : connected.value
        ? "断开连接"
        : status.value.state === "error"
          ? "重新连接"
          : "连接内网",
);
const canConnect = computed(
  () => !busy.value && (connected.value || availableNodes.value.length > 0),
);
const automatic = computed(() => status.value.nodeSelection !== "manual");
const nodeLabel = computed(() =>
  connected.value
    ? status.value.selectedNodeName || selectedNode.value?.name || "已连接节点"
    : automatic.value
      ? "自动选择"
      : selectedNode.value?.name ||
        status.value.selectedNodeName ||
        "节点不可用",
);
const formatRateLimit = (limit: {
  uploadMbps: number;
  downloadMbps: number;
}) =>
  limit.uploadMbps === 0 && limit.downloadMbps === 0
    ? "不限速"
    : `上行 ${limit.uploadMbps || "不限"} / 下行 ${limit.downloadMbps || "不限"} Mbps`;
const sortedNodes = computed(() =>
  [...status.value.nodes].sort(
    (a, b) =>
      Number(b.healthy) - Number(a.healthy) ||
      (a.latencyMs ?? Infinity) - (b.latencyMs ?? Infinity) ||
      a.name.localeCompare(b.name),
  ),
);
const groupedNodes = computed(() => {
  const groups = new Map<string, typeof sortedNodes.value>();
  for (const node of sortedNodes.value) {
    const region = node.region || "未标注区域";
    const nodes = groups.get(region) ?? [];
    nodes.push(node);
    groups.set(region, nodes);
  }
  return [...groups.entries()].map(([region, nodes]) => ({ region, nodes }));
});
const themes: { id: ThemePreference; label: string; icon: typeof Sun }[] = [
  { id: "light", label: "浅色", icon: Sun },
  { id: "dark", label: "深色", icon: Moon },
  { id: "system", label: "跟随系统", icon: Monitor },
];
const families = ["ipv4", "ipv6"] as const;
const timeUpdated = computed(
  () =>
    lastUpdated.value?.toLocaleTimeString("zh-CN", { hour12: false }) ||
    "未同步",
);
const updateStage = computed(() => status.value.clientUpdate.state);
const updateNotes = computed(
  () => status.value.clientUpdate.releaseNotes ?? [],
);
const updateVersion = computed(
  () => status.value.clientUpdate.availableVersion || "",
);
const updateDialogTitle = computed(() => {
  if (updateStage.value === "downloading") return "正在下载更新";
  if (updateStage.value === "ready") return "更新包已准备好";
  if (updateStage.value === "error") return "更新失败";
  return "发现客户端更新";
});

function modes(family: "ipv4" | "ipv6"): TrafficMode[] {
  const items: TrafficMode[] = ["internal"];
  if (
    family === "ipv4"
      ? status.value.allowIpv4Split
      : status.value.allowIpv6Split
  )
    items.push("split");
  if (
    family === "ipv4"
      ? status.value.allowIpv4Global
      : status.value.allowIpv6Global
  )
    items.push("global");
  return items;
}
function modeOf(family: "ipv4" | "ipv6") {
  return family === "ipv4" ? status.value.ipv4Mode : status.value.ipv6Mode;
}
async function changeMode(family: "ipv4" | "ipv6", event: Event) {
  await client.setMode(
    family,
    (event.target as HTMLSelectElement).value as TrafficMode,
  );
  modeRevision.value++;
}
async function login() {
  if (busy.value || !username.value.trim() || !password.value) return;
  if (await client.login(username.value, password.value)) {
    password.value = "";
    showPassword.value = false;
    page.value = "connection";
  }
}
async function logout() {
  if (await client.logout()) {
    account.value?.close();
    details.value?.close();
    password.value = "";
    closePasswordChange();
    page.value = "connection";
  }
}
function openPasswordChange() {
  passwordFeedback.value = "";
  currentPassword.value = "";
  newPassword.value = "";
  confirmPassword.value = "";
  passwordChangeOpen.value = true;
}
function closePasswordChange() {
  passwordChangeOpen.value = false;
  currentPassword.value = "";
  newPassword.value = "";
  confirmPassword.value = "";
  passwordFeedback.value = "";
}
async function submitPasswordChange() {
  passwordFeedback.value = "";
  if (!currentPassword.value || !newPassword.value || !confirmPassword.value) {
    passwordFeedback.value = "请完整填写当前密码、新密码和确认密码。";
    return;
  }
  if (newPassword.value.length < 6 || newPassword.value.length > 256) {
    passwordFeedback.value = "新密码长度应为 6 至 256 位。";
    return;
  }
  if (newPassword.value !== confirmPassword.value) {
    passwordFeedback.value = "两次输入的新密码不一致。";
    return;
  }
  if (await client.changePassword(currentPassword.value, newPassword.value)) {
    passwordFeedback.value = "密码已修改。";
    currentPassword.value = "";
    newPassword.value = "";
    confirmPassword.value = "";
  }
}
function showDetails() {
  copyFeedback.value = "";
  details.value?.showModal();
}
function closeUpdateDialog() {
  if (updateVersion.value) promptedUpdateVersion.value = updateVersion.value;
  updateDialog.value?.close();
}
function openUpdateDialog(force = false) {
  if (
    !updateVersion.value ||
    (!force && promptedUpdateVersion.value === updateVersion.value)
  )
    return;
  promptedUpdateVersion.value = updateVersion.value;
  if (!updateDialog.value?.open) updateDialog.value?.showModal();
}
function checkForUpdate() {
  promptedUpdateVersion.value = "";
  void checkUpdate();
}
async function reportDiagnostics() {
  diagnosticUploadFeedback.value = "正在整理并上报最近日志…";
  const ok = await uploadDiagnostics();
  diagnosticUploadFeedback.value = ok
    ? status.value.diagnosticUploadMessage || "日志已上报"
    : status.value.diagnosticUploadMessage || "日志上报失败，请稍后重试";
}
function startUpdateDownload() {
  if (connected.value) {
    notice.value = { kind: "info", text: "安装更新前需断开连接。" };
  }
  void downloadUpdate();
  if (!updateDialog.value?.open) updateDialog.value?.showModal();
}
function installDownloadedUpdate() {
  if (connected.value) {
    notice.value = {
      kind: "error",
      text: "请先断开内部网络，再安装客户端更新。",
    };
    return;
  }
  void installUpdate();
}
async function copy(value: string, label: string) {
  const current = ++copyVersion;
  try {
    await navigator.clipboard.writeText(value);
    if (current === copyVersion) copyFeedback.value = `${label}已复制`;
  } catch {
    if (current === copyVersion)
      copyFeedback.value = "复制未完成，请选择文字手动复制。";
  }
}
async function navigate(nextPage: ClientPage) {
  page.value = nextPage;
  if (nextPage === "forwards") await refreshForwardPage();
  await nextTick();
  pageHeading.value?.focus();
}
async function refreshForwardPage() {
  if (
    status.value.authenticated &&
    !busy.value &&
    !forwardRefreshing.value
  ) {
    forwardRefreshing.value = true;
    try {
      await client.refreshPortForwardQuota();
      await client.refreshPortForwardEntries();
      await client.refreshPortForwards();
    } finally {
      forwardRefreshing.value = false;
    }
  }
}
function toggleForwardProtocol(protocol: PortForwardProtocol) {
  const current = forwardProtocols.value;
  if (current.includes(protocol)) {
    if (current.length > 1) current.splice(current.indexOf(protocol), 1);
  } else {
    current.push(protocol);
  }
}
function forwardAddress(forward: (typeof status.value.portForwards)[number]) {
  return `${forward.public_host || forward.public_address}:${forward.public_port}`;
}
function displayAddress(value?: string | null) {
  return value?.split("/", 1)[0] || "";
}
function formatBytes(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
}
async function submitForward() {
  if (
    !forwardTargetPort.value ||
    !forwardProtocols.value.length ||
    !connected.value ||
    !status.value.portForwardQuota.enabled ||
    status.value.portForwardQuota.available === 0
  )
    return;
  await client.createPortForward(
    forwardEntryId.value ?? automaticForwardEntryId.value,
    [...forwardProtocols.value],
    forwardTargetPort.value,
  );
}
watch(
  () => status.value.authenticated,
  (authenticated) => {
    if (!authenticated) {
      details.value?.close();
      account.value?.close();
      return;
    }
    // Authentication can be restored by the background service without a
    // user action. The service snapshot is intentionally lightweight and may
    // not contain the account's port-forward leases after a process restart,
    // so hydrate them immediately instead of showing an empty "我的映射"
    // page until the user navigates away and back.
    void nextTick().then(() => refreshForwardPage());
  },
);
const forwardRefreshTimer = setInterval(() => {
  if (page.value === "forwards" && status.value.authenticated && !busy.value && !forwardRefreshing.value) {
    void refreshForwardPage();
  }
}, 10000);
onUnmounted(() => clearInterval(forwardRefreshTimer));
watch(
  () =>
    [
      status.value.clientUpdate.state,
      status.value.clientUpdate.availableVersion,
    ] as const,
  ([state, version]) => {
    if ((state === "available" || state === "ready") && version)
      openUpdateDialog();
  },
);
</script>

<template>
  <div
    class="client"
    :class="{
      'is-connected': connected,
      'is-working': connecting || disconnecting,
    }"
  >
    <a class="skip-link" href="#main-content">跳到主要内容</a>

    <template v-if="starting">
      <main
        id="main-content"
        class="startup-screen"
        aria-busy="true"
        aria-live="polite"
      >
        <div class="startup-brand brand" aria-label="Pmman Network">
          <span class="brand-mark"><Network :size="21" /></span
          ><span>Pmman<span class="brand-caption">NETWORK</span></span>
        </div>
        <section class="startup-card" aria-labelledby="startup-title">
          <span class="startup-icon" aria-hidden="true"
            ><LoaderCircle :size="30" class="spin"
          /></span>
          <span class="section-kicker">正在启动</span>
          <h1 id="startup-title">正在准备 Pmman Network</h1>
          <p>正在准备连接环境，请稍候。</p>
          <span class="startup-status"
            ><LoaderCircle :size="15" class="spin" />正在登录</span
          >
        </section>
        <footer class="startup-footer">
          Pmman Network · Windows 客户端 · v{{ appVersion }}
        </footer>
      </main>
    </template>

    <template v-else-if="!status.authenticated">
      <main id="main-content" class="welcome">
        <section class="welcome-story" aria-label="Pmman Network">
          <div class="brand">
            <span class="brand-mark"><Network :size="21" /></span
            ><span>Pmman<span class="brand-caption">NETWORK</span></span>
          </div>
          <div class="welcome-art" aria-hidden="true">
            <div class="orbit orbit-one"></div>
            <div class="orbit orbit-two"></div>
            <span class="orbit-node node-one"><Monitor :size="23" /></span>
            <span class="orbit-node node-two"><Server :size="23" /></span>
            <span class="orbit-node node-three"><Globe2 :size="23" /></span>
            <div class="art-center">
              <ShieldCheck :size="55" :stroke-width="1.5" />
            </div>
          </div>
          <div class="welcome-copy">
            <h1>连接内网</h1>
            <p>登录后即可访问已授权的内部资源。</p>
          </div>
          <div class="welcome-features">
            <span><ShieldCheck :size="15" /> 加密连接</span
            ><span><Zap :size="15" /> 智能选点</span
            ><span><Globe2 :size="15" /> IPv4 / IPv6</span>
          </div>
        </section>
        <section class="login-panel" aria-labelledby="login-title">
          <button
            class="icon-button theme-toggle"
            :aria-label="dark ? '切换浅色主题' : '切换深色主题'"
            @click="preference = dark ? 'light' : 'dark'"
          >
            <Sun v-if="dark" :size="19" /><Moon v-else :size="19" />
          </button>
          <div class="login-content">
            <h2 id="login-title">{{registrationOpen?'使用邀请码注册':'登录'}}</h2>
            <p class="muted">{{registrationOpen?'创建网络账户。':'登录后访问已授权资源。'}}</p>
            <RegistrationForm v-if="registrationOpen" :busy="busy" :request="client.invitationRequest" @registered="registered" @cancel="registrationOpen=false" />
            <form v-else class="login-form" @submit.prevent="login">
              <label for="username">用户名</label>
              <div class="input-with-icon">
                <UserRound :size="18" /><input
                  id="username"
                  v-model="username"
                  name="username"
                  autocomplete="username"
                  placeholder="输入用户名"
                  required
                  :disabled="busy"
                />
              </div>
              <label for="password">密码</label>
              <div class="input-with-icon">
                <KeyRound :size="18" /><input
                  id="password"
                  v-model="password"
                  name="password"
                  :type="showPassword ? 'text' : 'password'"
                  autocomplete="current-password"
                  placeholder="输入密码"
                  required
                  :disabled="busy"
                /><button
                  type="button"
                  class="password-toggle"
                  :aria-pressed="showPassword"
                  :disabled="busy"
                  @click="showPassword = !showPassword"
                >
                  {{ showPassword ? "隐藏" : "显示" }}
                </button>
              </div>
              <StatusNotice
                v-if="notice"
                :text="notice.text"
                :kind="notice.kind"
                closable
                @close="notice = null"
              />
              <button
                type="submit"
                class="button primary login-button"
                :disabled="busy || !username.trim() || !password"
              >
                <LoaderCircle v-if="busy" class="spin" :size="18" /><span>{{
                  starting
                    ? "正在准备…"
                    : operation === "login"
                      ? "正在登录…"
                      : "登录"
                }}</span
                ><ArrowRight v-if="!busy" :size="18" />
              </button>
            </form>
            <p class="login-help">
              <button v-if="!registrationOpen" class="button secondary" :disabled="busy" @click="registrationOpen=true;notice=null">使用邀请码注册</button>
              <CircleHelp :size="15" /> 忘记密码请联系管理员。
            </p>
          </div>
          <footer class="login-footer">
            <span>Pmman Network</span
            ><span>Windows 客户端 · v{{ appVersion }}</span>
          </footer>
        </section>
      </main>
    </template>

    <template v-else>
      <aside class="sidebar">
        <div class="brand">
          <span class="brand-mark"><Network :size="21" /></span
          ><span>Pmman<span class="brand-caption">NETWORK</span></span>
        </div>
        <nav aria-label="主要导航">
          <button
            v-for="item in navigation"
            :key="item.id"
            :class="['nav-button', { active: page === item.id }]"
            :aria-current="page === item.id ? 'page' : undefined"
            @click="navigate(item.id)"
          >
            <component :is="item.icon" :size="19" /><span>{{ item.label }}</span>
          </button>
        </nav>
        <div class="sidebar-bottom">
          <div class="sidebar-note">
            <ShieldCheck :size="17" />
            <div><strong>安全访问内网</strong><span>加密连接</span></div>
          </div>
          <button
            class="account-button"
            aria-label="查看网络账户"
            :disabled="busy"
            @click="account?.showModal()"
          >
            <span class="avatar">{{
              status.username?.slice(0, 1).toUpperCase() || "U"
            }}</span
            ><span class="account-text"
              ><strong>{{ status.username }}</strong
              ><span>网络账户</span></span
            ><ChevronDown :size="15" />
          </button>
        </div>
      </aside>

      <div class="workspace">
        <header class="topbar">
          <span class="breadcrumb"
            >Pmman Network <ChevronRight :size="14" />
            <strong>{{ titles[page].title }}</strong></span
          >
          <div class="topbar-actions">
            <span
              :class="[
                'status-badge',
                { online: connected, pending: connecting || disconnecting },
              ]"
              ><span class="status-dot"></span>{{ connectionLabel }}</span
            ><button
              class="icon-button"
              :aria-label="dark ? '切换浅色主题' : '切换深色主题'"
              @click="preference = dark ? 'light' : 'dark'"
            >
              <Sun v-if="dark" :size="18" /><Moon v-else :size="18" />
            </button>
          </div>
        </header>
        <main id="main-content" class="main-content">
          <div class="page-heading">
            <div>
              <h1 ref="pageHeading" tabindex="-1">{{ titles[page].title }}</h1>
              <p>{{ titles[page].description }}</p>
            </div>
          </div>
          <StatusNotice
            v-if="notice"
            :text="notice.text"
            :kind="notice.kind"
            closable
            @close="notice = null"
          />

          <StatusNotice
            v-if="status.portForwardFirewallWarning"
            :text="status.portForwardFirewallWarning"
            kind="warning"
            summary-text="Windows 防火墙规则未同步，请检查端口映射的目标端口放行设置。"
          />

          <template v-if="page === 'connection'">
            <section
              class="connection-card"
              aria-labelledby="connection-title"
              :aria-busy="connecting || disconnecting"
            >
              <div class="connection-card-header">
                <span class="eyebrow"
                  ><ShieldCheck :size="15" />
                  {{ connected ? "安全连接已建立" : "内部网络" }}</span
                ><button class="text-button" @click="showDetails">
                  连接详情 <ArrowRight :size="14" />
                </button>
              </div>
              <div class="connection-body">
                <div class="power-ring">
                  <span class="power-ring-inner"></span
                  ><button
                    :class="[
                      'power-button',
                      { connected, pending: connecting || disconnecting },
                    ]"
                    :disabled="!canConnect"
                    :aria-label="buttonLabel"
                    @click="client.toggleConnection"
                  >
                    <LoaderCircle
                      v-if="connecting || disconnecting"
                      class="spin"
                      :size="36"
                    /><Power v-else :size="36" :stroke-width="1.8" />
                  </button>
                </div>
                <div class="connection-copy">
                  <h2 id="connection-title">
                    {{
                      connecting
                        ? "正在连接"
                        : disconnecting
                          ? "正在安全断开"
                          : connected
                            ? "内部网络已就绪"
                            : status.state === "error"
                              ? "连接失败"
                              : "未连接"
                    }}
                  </h2>
                  <p aria-live="polite">
                    {{
                      connecting
                        ? status.state === "connecting"
                          ? status.message
                          : "正在建立安全连接，请稍候。"
                        : disconnecting
                          ? "正在断开连接，请稍候。"
                          : connected
                            ? `已连接到 ${nodeLabel}`
                            : "点击连接，访问已授权的内部资源。"
                    }}
                  </p>
                </div>
                <button
                  class="button primary connect-button"
                  :disabled="!canConnect"
                  @click="client.toggleConnection"
                >
                  <LoaderCircle
                    v-if="connecting || disconnecting"
                    class="spin"
                    :size="17"
                  /><span>{{ buttonLabel }}</span
                  ><ArrowRight v-if="!connected && !connecting" :size="17" />
                </button>
                <button
                  v-if="connecting && status.connectionWaitingCleanup"
                  class="button secondary cancel-connect-button"
                  :disabled="cancellingConnection"
                  @click="client.cancelConnection"
                >
                  {{ cancellingConnection ? "正在取消…" : "取消连接" }}
                </button>
                <p
                  v-if="!availableNodes.length && !connected"
                  class="availability-hint"
                >
                  暂时没有可用节点，请刷新节点或联系管理员。
                </p>
              </div>
              <div class="connection-meta">
                <span
                  ><ShieldCheck :size="14" />
                  {{ connected ? "安全连接" : "加密传输" }}</span
                ><span
                  ><Clock3 :size="14" />
                  {{ connected ? elapsed : "连接后分配双栈地址" }}</span
                >
              </div>
            </section>

            <div class="connection-options">
              <button class="option-card" @click="navigate('nodes')">
                <span class="option-icon"><Server :size="20" /></span
                ><span class="option-content"
                  ><span class="small-label">连接节点</span
                  ><strong>{{ nodeLabel }}</strong
                  ><span class="muted">{{
                    connected
                      ? selectedNode?.region || "已连接节点"
                      : automatic
                        ? "按网络延迟自动选择"
                        : selectedNode?.region || "指定节点连接"
                  }}</span></span
                ><span
                  v-if="
                    selectedNode?.latencyMs != null && (connected || !automatic)
                  "
                  class="latency"
                  >{{ selectedNode.latencyMs }} ms</span
                ><ChevronRight :size="17" /></button
              ><button class="option-card" @click="navigate('settings')">
                <span class="option-icon"><SlidersHorizontal :size="20" /></span
                ><span class="option-content"
                  ><span class="small-label">流量模式</span
                  ><strong>{{
                    status.ipv4Mode === status.ipv6Mode
                      ? modeLabels[status.ipv4Mode]
                      : "双栈独立配置"
                  }}</strong
                  ><span class="muted"
                    >IPv4 / IPv6
                    {{
                      status.ipv4Mode === status.ipv6Mode
                        ? "使用相同策略"
                        : "使用独立策略"
                    }}</span
                  ></span
                ><ChevronRight :size="17" />
              </button>
            </div>
            <section class="address-panel" aria-label="网络地址">
              <div class="address-item">
                <span class="small-label">IPv4 地址</span
                ><span class="address-value">{{
                  displayAddress(status.ipv4) || "连接后分配"
                }}</span
                ><button
                  v-if="status.ipv4"
                  class="icon-button"
                  aria-label="复制 IPv4 地址"
                  @click="copy(displayAddress(status.ipv4), 'IPv4 地址')"
                >
                  <Copy :size="15" />
                </button>
              </div>
              <div class="address-item">
                <span class="small-label">IPv6 地址</span
                ><span class="address-value">{{
                  displayAddress(status.ipv6) || "连接后分配"
                }}</span
                ><button
                  v-if="status.ipv6"
                  class="icon-button"
                  aria-label="复制 IPv6 地址"
                  @click="copy(displayAddress(status.ipv6), 'IPv6 地址')"
                >
                  <Copy :size="15" />
                </button>
              </div>
            </section>
          </template>

          <template v-else-if="page === 'nodes'">
            <div class="section-toolbar">
              <span class="muted"
                >{{ availableNodes.length }} 个可用节点 · 显示网络延迟</span
              ><button
                class="button secondary compact"
                :disabled="locked"
                @click="client.refreshNodes"
              >
                <RefreshCw
                  :size="15"
                  :class="{ spin: operation === 'list_nodes' }"
                />{{ operation === "list_nodes" ? "刷新中…" : "刷新节点" }}
              </button>
            </div>
            <p v-if="connected" class="context-hint">
              <ShieldCheck :size="15" /> 当前连接正在使用
              {{ nodeLabel }}，断开后可切换节点。
            </p>
            <button
              :class="['automatic-card', { selected: automatic }]"
              :aria-pressed="automatic"
              :disabled="locked"
              @click="client.selectNode(null)"
            >
              <span class="auto-icon"><Sparkles :size="23" /></span
              ><span
                ><strong>自动选择</strong
                ><span>连接时自动选择延迟最低的可用节点</span></span
              ><span class="recommended">推荐</span
              ><Check v-if="automatic" :size="19" />
            </button>
            <div v-if="sortedNodes.length" class="node-groups">
              <section v-for="group in groupedNodes" :key="group.region" class="node-group">
                <div class="node-group-heading">
                  <span class="section-kicker">区域</span>
                  <h2>{{ group.region }}</h2>
                  <span class="muted">{{ group.nodes.length }} 个节点</span>
                </div>
                <div class="node-grid">
              <button
                v-for="node in group.nodes"
                :key="node.nodeId"
                :class="[
                  'node-card',
                  {
                    selected:
                      !automatic && status.selectedNodeId === node.nodeId,
                    unavailable: !node.healthy,
                  },
                ]"
                :aria-pressed="
                  !automatic && status.selectedNodeId === node.nodeId
                "
                :disabled="locked || !node.healthy"
                :aria-label="`选择节点 ${node.name}`"
                @click="client.selectNode(node.nodeId)"
              >
                <span class="node-card-top"
                  ><span class="node-symbol"><Server :size="20" /></span
                  ><span :class="['node-health', { healthy: node.healthy }]">{{
                    !node.healthy
                      ? "不可用"
                      : node.latencyMs == null
                        ? "待探测"
                        : `${node.latencyMs} ms`
                  }}</span></span
                ><strong class="node-name">{{ node.name }}</strong
                ><span class="node-region">{{
                  node.region || "未标注区域"
                }}</span
                ><span class="node-region">{{
                  formatRateLimit(node.rateLimit)
                }}</span
                ><span class="node-card-bottom"
                  ><span>{{ node.nodeId }}</span
                  ><Check
                    v-if="!automatic && status.selectedNodeId === node.nodeId"
                    :size="17" /><ArrowRight v-else :size="16"
                /></span>
              </button>
                </div>
              </section>
            </div>
            <div v-else class="empty-state">
              <Server :size="34" />
              <h2>还没有可用节点</h2>
              <p>刷新列表重试，或联系管理员检查节点状态。</p>
              <button
                class="button secondary"
                :disabled="locked"
                @click="client.refreshNodes"
              >
                刷新节点
              </button>
            </div>
            <p class="subtle-note">
              <Radio :size="14" /> 选择将在下次连接时生效。
            </p>
          </template>

          <template v-else-if="page === 'forwards'">
            <div class="section-toolbar">
              <span class="muted">{{ status.portForwards.length }} 个转发</span>
              <button
                class="button primary compact"
                :disabled="busy || !connected || !status.portForwardQuota.enabled || !status.portForwardQuota.available"
                :aria-expanded="forwardCreateOpen"
                @click="forwardCreateOpen = !forwardCreateOpen"
              >
                {{ forwardCreateOpen ? "取消创建" : "创建转发" }}
              </button>
            </div>
            <div class="quota-summary" role="status">
              <div>
                <span class="small-label">端口转发配额</span>
                <strong v-if="status.portForwardQuota.enabled">
                  {{ status.portForwardQuota.used }} / {{ status.portForwardQuota.limit }} 个
                </strong>
                <strong v-else>未启用</strong>
              </div>
              <span v-if="status.portForwardQuota.enabled" class="muted">
                剩余 {{ status.portForwardQuota.available }} 个
              </span>
              <button class="text-button" :disabled="busy" @click="client.refreshPortForwardQuota">
                刷新配额
              </button>
            </div>
            <p v-if="!connected" class="context-hint">连接内网后可创建转发。</p>
            <section
              v-if="forwardCreateOpen"
              class="settings-card"
              aria-labelledby="forward-create-title"
            >
              <div class="section-title">
                <span class="option-icon"><Layers3 :size="20" /></span>
                <div>
                  <h2 id="forward-create-title">创建端口转发</h2>
                  <p>公网端口由系统自动分配。</p>
                </div>
              </div>
              <form class="forward-form" @submit.prevent="submitForward">
                <label
                  >入口<select
                    v-model="forwardEntryId"
                    :disabled="busy || !connected"
                  >
                    <option :value="null">自动选择</option>
                    <option
                      v-for="entry in sortedForwardEntries"
                      :key="entry.entry_id"
                      :value="entry.entry_id"
                      :disabled="!entry.node_healthy || !entry.available_ports"
                    >
                      {{ entry.name }} ·
                      {{ entry.family === "ipv4" ? "IPv4" : "IPv6" }} ·
                      {{
                        forwardEntryLatency(entry) == null
                          ? "延迟待测"
                          : `${forwardEntryLatency(entry)} ms`
                      }}
                      · 剩余 {{ entry.available_ports }}
                    </option>
                  </select></label
                >
                <div class="setting-row">
                  <div>
                    <label>协议</label>
                    <p>TCP 和 UDP 使用同一公网端口时只计一个额度。</p>
                  </div>
                  <div class="protocol-options">
                    <button
                      type="button"
                      :class="[
                        'button',
                        forwardProtocols.includes('tcp')
                          ? 'primary'
                          : 'secondary',
                      ]"
                      :disabled="busy || !connected"
                      @click="toggleForwardProtocol('tcp')"
                    >
                      TCP
                    </button>
                    <button
                      type="button"
                      :class="[
                        'button',
                        forwardProtocols.includes('udp')
                          ? 'primary'
                          : 'secondary',
                      ]"
                      :disabled="busy || !connected"
                      @click="toggleForwardProtocol('udp')"
                    >
                      UDP
                    </button>
                  </div>
                </div>
                <label
                  >客户端目标端口<input
                    v-model.number="forwardTargetPort"
                    type="number"
                    min="1"
                    max="65535"
                    placeholder="例如 3389"
                    :disabled="busy || !connected"
                /></label>
                <button
                  class="button primary"
                  type="submit"
                  :disabled="
                    busy ||
                    !connected ||
                    !status.portForwardQuota.enabled ||
                    !status.portForwardQuota.available ||
                    !forwardTargetPort ||
                    !forwardProtocols.length
                  "
                >
                  {{
                    operation === "create_port_forward"
                      ? "正在创建…"
                      : "创建转发"
                  }}
                </button>
              </form>
              <StatusNotice
                v-if="status.portForwardMessage"
                :text="status.portForwardMessage"
                kind="info"
              />
              <p v-if="!connected" class="subtle-note">
                连接内网后可创建转发。
              </p>
            </section>
            <section class="settings-card" aria-labelledby="forward-list-title">
              <div class="section-title">
                <span class="option-icon"><Globe2 :size="20" /></span>
                <div>
                  <h2 id="forward-list-title">我的转发</h2>
                </div>
              </div>
              <div v-if="status.portForwards.length" class="forward-list">
                <article
                  v-for="forward in status.portForwards"
                  :key="forward.forward_id"
                  class="forward-item"
                >
                  <div>
                    <strong>{{ forwardAddress(forward) }}</strong
                    ><span
                      >{{ forward.protocols.join(" + ").toUpperCase() }} →
                      {{ forward.target_address }}:{{
                        forward.target_port
                      }}</span
                    >
                  </div>
                  <div class="forward-item-actions">
                    <span
                      :class="[
                        'node-health',
                        { healthy: forward.state === 'active' },
                      ]"
                      >{{ forwardStateLabel(forward.state) }}</span
                    ><button
                      class="button secondary compact"
                      :disabled="busy"
                      @click="client.releasePortForward(forward.forward_id)"
                    >
                      释放
                    </button>
                  </div>
                </article>
              </div>
              <div v-else class="empty-state">
                <Layers3 :size="34" />
                <h2>暂无端口转发</h2>
                <p>创建转发后可从公网访问本机服务。</p>
              </div>
            </section>
          </template>

          <template v-else-if="page === 'peers'">
            <section class="settings-card" aria-labelledby="p2p-title">
              <div class="section-title">
                <span class="option-icon"><Network :size="20" /></span>
                <div>
                  <h2 id="p2p-title">客户端互联</h2>
                  <p>设备之间优先直连，无法直连时使用节点转发。</p>
                </div>
              </div>
              <div class="setting-row">
                <div>
                  <label for="p2p-toggle">开启 P2P 直连</label>
                  <p>默认开启，可随时关闭。</p>
                </div>
                <button
                  id="p2p-toggle"
                  type="button"
                  role="switch"
                  :aria-checked="status.p2pEnabled"
                  :disabled="busy"
                  class="button secondary"
                  @click="client.setP2p(!status.p2pEnabled)"
                >
                  <LoaderCircle
                    v-if="operation === 'set_p2p'"
                    :size="16"
                    class="spinning"
                  />{{ status.p2pEnabled ? "已开启" : "已关闭" }}
                </button>
              </div>
              <p class="context-hint" role="status">
                {{
                  p2pSummary(status.p2pMessage, status.p2pEnabled, connected)
                }}
              </p>
              <div
                v-if="connected"
                class="p2p-peers"
                aria-label="客户端互联状态"
              >
                <p v-if="!status.p2pPeers.length" class="subtle-note">
                  当前没有其他在线客户端。
                </p>
                <div
                  v-for="peer in status.p2pPeers"
                  :key="peer.deviceId"
                  class="setting-row"
                >
                  <div>
                    <strong>{{ peer.username }}</strong>
                    <p class="mono">{{ peer.ipv4 }} · {{ peer.ipv6 }}</p>
                  </div>
                  <span
                    >{{
                      {
                        idle: "待访问",
                        probing: "尝试直连 · 节点中转",
                        direct: "P2P 直连",
                        relay: "节点中转",
                      }[peer.path]
                    }}<template v-if="peer.latencyMs != null">
                      · {{ peer.latencyMs }} ms</template
                    ></span
                  >
                </div>
              </div>
            </section>
          </template>
          <template v-else>
            <div class="settings-navigation" role="group" aria-label="设置分类">
              <button
                v-for="item in [
                  { id: 'network' as const, label: '网络' },
                  { id: 'appearance' as const, label: '外观' },
                  { id: 'about' as const, label: '关于' },
                ]"
                :key="item.id"
                type="button"
                :class="[
                  'settings-navigation-item',
                  { active: settingsSection === item.id },
                ]"
                :aria-pressed="settingsSection === item.id"
                @click="settingsSection = item.id"
              >
                {{ item.label }}
              </button>
            </div>
            <section
              v-if="settingsSection === 'network'"
              class="settings-card"
              aria-labelledby="traffic-title"
            >
              <div class="section-title">
                <span class="option-icon"
                  ><SlidersHorizontal :size="20"
                /></span>
                <div>
                  <h2 id="traffic-title">网络流量</h2>
                  <p>分别设置 IPv4 和 IPv6 的流量范围。</p>
                </div>
              </div>
              <p v-if="connected" class="context-hint">
                断开连接后可修改流量模式。
              </p>
              <div v-for="family in families" :key="family" class="setting-row">
                <div>
                  <label :for="`${family}-mode`"
                    >{{ family === "ipv4" ? "IPv4" : "IPv6" }} 接入模式</label
                  >
                  <p>{{ modeDescriptions[modeOf(family)] }}</p>
                </div>
                <div class="select-wrapper">
                  <select
                    :key="`${family}-${modeRevision}`"
                    :id="`${family}-mode`"
                    :value="modeOf(family)"
                    :disabled="locked"
                    @change="changeMode(family, $event)"
                  >
                    <option
                      v-for="mode in modes(family)"
                      :key="mode"
                      :value="mode"
                    >
                      {{ modeLabels[mode] }}
                    </option></select
                  ><ChevronDown :size="15" />
                </div>
              </div>
              <p class="subtle-note">
                <KeyRound :size="14" /> 可用模式由管理员授权。
              </p>
            </section>
            <section
              v-if="settingsSection === 'appearance'"
              class="settings-card"
              aria-labelledby="appearance-title"
            >
              <div class="section-title">
                <span class="option-icon"><Sun :size="20" /></span>
                <div>
                  <h2 id="appearance-title">外观</h2>
                  <p>选择主题。</p>
                </div>
              </div>
              <div class="theme-choices" role="group" aria-label="主题">
                <button
                  v-for="theme in themes"
                  :key="theme.id"
                  :class="[
                    'theme-choice',
                    { selected: preference === theme.id },
                  ]"
                  :aria-pressed="preference === theme.id"
                  @click="preference = theme.id"
                >
                  <component :is="theme.icon" :size="20" /><span>{{
                    theme.label
                  }}</span
                  ><Check v-if="preference === theme.id" :size="15" />
                </button>
              </div>
            </section>
            <section
              v-if="settingsSection === 'about'"
              class="settings-card about-card"
            >
              <div>
                <h2>Pmman Network</h2>
                <p>Windows 客户端 · v{{ appVersion }}</p>
              </div>
              <div class="about-actions">
                <button class="text-button" @click="showDetails">
                  查看连接详情 <ArrowRight :size="14" />
                </button>
                <button
                  class="text-button"
                  :disabled="busy"
                  @click="checkForUpdate"
                >
                  <RefreshCw :size="14" /> 检查更新
                </button>
              </div>
            </section>
            <section
              v-if="settingsSection === 'about'"
              class="settings-card"
              aria-labelledby="update-title"
            >
              <div class="section-title">
                <span class="option-icon"><RefreshCw :size="20" /></span>
                <div>
                  <h2 id="update-title">客户端更新</h2>
                  <p>{{ updateStateLabel(status.clientUpdate) }}</p>
                </div>
              </div>
              <div class="setting-row">
                <div>
                  <strong v-if="status.clientUpdate.ready">
                    v{{ status.clientUpdate.availableVersion }} 已准备安装
                  </strong>
                  <strong v-else-if="status.clientUpdate.availableVersion">
                    发现 v{{ status.clientUpdate.availableVersion }}
                  </strong>
                  <strong v-else>v{{ appVersion }}</strong>
                </div>
                <div class="update-actions">
                  <button
                    v-if="status.clientUpdate.ready"
                    class="button primary"
                    :disabled="busy || connected"
                    @click="installDownloadedUpdate"
                  >
                    <RefreshCw :size="16" />退出并更新
                  </button>
                  <button
                    v-else-if="status.clientUpdate.availableVersion"
                    class="button primary"
                    :disabled="busy"
                    @click="startUpdateDownload"
                  >
                    <LoaderCircle
                      v-if="operation === 'download_update'"
                      class="spin"
                      :size="16"
                    />
                    {{
                      operation === "download_update" ? "正在下载…" : "下载更新"
                    }}
                  </button>
                  <button
                    v-else
                    class="button secondary"
                    :disabled="busy"
                    @click="checkForUpdate"
                  >
                    <RefreshCw :size="16" />检查更新
                  </button>
                </div>
              </div>
              <p
                v-if="status.clientUpdate.ready && connected"
                class="context-hint"
              >
                请先断开网络连接，再执行更新。
              </p>
            </section>
          </template>

          <p v-if="copyFeedback" class="copy-feedback" role="status">
            {{ copyFeedback }}
          </p>
          <footer class="workspace-footer">
            <span
              ><span :class="['status-dot', { good: connected }]"></span
              >{{ connected ? "加密隧道正在运行" : "等待建立连接" }}</span
            ><span>状态更新于 {{ timeUpdated }}</span>
          </footer>
        </main>
      </div>
    </template>

    <dialog
      ref="details"
      class="details-dialog"
      aria-labelledby="details-title"
    >
      <div class="dialog-heading">
        <div>
          <span class="section-kicker">连接信息</span>
          <h2 id="details-title">连接详情</h2>
        </div>
        <button
          class="icon-button"
          aria-label="关闭连接详情"
          @click="details?.close()"
        >
          <X :size="20" />
        </button>
      </div>
      <div class="dialog-body">
        <dl class="details-list">
          <div>
            <dt>连接状态</dt>
            <dd>{{ connectionLabel }}</dd>
          </div>
          <div>
            <dt>连接节点</dt>
            <dd>{{ nodeLabel }}</dd>
          </div>
          <div>
            <dt>加密隧道</dt>
            <dd>{{ connected ? "WireGuard" : "尚未建立" }}</dd>
          </div>
          <div>
            <dt>IPv4 地址</dt>
            <dd class="mono">{{ displayAddress(status.ipv4) || "—" }}</dd>
          </div>
          <div>
            <dt>IPv6 地址</dt>
            <dd class="mono">{{ displayAddress(status.ipv6) || "—" }}</dd>
          </div>
          <div>
            <dt>流量模式</dt>
            <dd>
              IPv4 {{ modeLabels[status.ipv4Mode] }} / IPv6
              {{ modeLabels[status.ipv6Mode] }}
            </dd>
          </div>
          <div>
            <dt>节点探测延迟</dt>
            <dd>
              {{
                selectedNode?.latencyMs != null
                  ? `${selectedNode.latencyMs} ms（UDP）`
                  : "尚未探测"
              }}
            </dd>
          </div>
        </dl>
        <dl class="details-list">
          <div>
            <dt>DNS</dt>
            <dd>
              {{
                !connected
                  ? "未连接"
                  : status.dnsSplitEnabled
                    ? "国内本机解析 · 海外节点解析"
                    : status.nodeDnsServers.length
                      ? "节点 DNS"
                    : status.dnsServers.length
                      ? "系统 DNS"
                      : "暂无可用 DNS"
              }}
            </dd>
          </div>
          <div>
            <dt>设备互联</dt>
            <dd>
              {{ p2pSummary(status.p2pMessage, status.p2pEnabled, connected) }}
            </dd>
          </div>
          <div>
            <dt>流量统计</dt>
            <dd class="traffic-summary">
              <span>节点：{{ formatBytes(status.trafficStats.nodeRxBytes + status.trafficStats.nodeTxBytes) }}</span>
              <span>P2P 直连：{{ formatBytes(status.trafficStats.p2pDirectRxBytes + status.trafficStats.p2pDirectTxBytes) }}</span>
              <span>P2P 中转：{{ formatBytes(status.trafficStats.p2pRelayRxBytes + status.trafficStats.p2pRelayTxBytes) }}</span>
            </dd>
          </div>
        </dl>
        <details class="diagnostic-disclosure">
          <summary>高级诊断</summary>
          <h3><Layers3 :size="16" /> AllowedIPs</h3>
          <div class="route-list mono">
            {{
              status.allowedIps.length
                ? status.allowedIps.join("\n")
                : "连接后显示生效路由"
            }}
          </div>
          <h3 v-if="status.splitRouteSource"><Globe2 :size="16" /> 客户端分流规则</h3>
          <p v-if="status.splitRouteSource" class="service-message">
            {{ status.splitRouteSource }} · 快照 {{ status.splitRouteSnapshot || "—" }} ·
            IPv4 {{ status.splitIpv4PrefixCount }} 条 · IPv6
            {{ status.splitIpv6PrefixCount }} 条 · 安装 {{ status.splitRouteInstallMs }} ms
          </p>
          <h3><Globe2 :size="16" /> DNS 分流</h3>
          <p class="mono details-dns">
            {{
              status.dnsSplitEnabled
                ? `国内域名使用本机 DNS；海外及未分类域名使用节点 DNS（${status.nodeDnsServers.join(" · ")}）`
                : status.dnsServers.length && status.nodeDnsServers.length
                ? `${status.dnsServers.join(" · ")}（节点 DNS）`
                : status.dnsServers.length
                  ? `${status.dnsServers.join(" · ")}（未配置节点 DNS，使用本地 DNS）`
                  : connected
                    ? "未检测到可用 DNS"
                    : "连接后显示 DNS 配置"
            }}
          </p>
          <h3><Wifi :size="16" /> 服务状态</h3>
          <p class="service-message">{{ status.message }}</p>
          <h3>设备互联</h3>
          <p class="service-message">{{ status.p2pMessage || "—" }}</p>
          <h3>最近错误</h3>
          <pre class="diagnostic-text">{{ lastDiagnosticError || "无" }}</pre>
          <div class="diagnostic-upload">
            <button
              class="button secondary"
              :disabled="busy || !status.authenticated"
              @click="reportDiagnostics"
            >
              <Upload :size="16" />上报诊断日志
            </button>
            <span v-if="diagnosticUploadFeedback" class="muted" role="status">
              {{ diagnosticUploadFeedback }}
            </span>
          </div>
          <p class="muted">
            仅保留最近日志，上传前会自动过滤凭据；也可将报告编号提供给维护人员。
          </p>
        </details>
        <p v-if="copyFeedback" class="copy-feedback" role="status">
          {{ copyFeedback }}
        </p>
      </div>
      <div class="dialog-footer">
        <button
          class="button secondary"
          @click="copy(diagnosticText, '诊断信息')"
        >
          <Copy :size="16" />复制诊断信息
        </button>
        <button class="button secondary" @click="details?.close()">关闭</button>
      </div>
    </dialog>
    <dialog
      ref="updateDialog"
      class="update-dialog"
      aria-labelledby="update-dialog-title"
      @cancel="updateStage === 'downloading' && $event.preventDefault()"
    >
      <div class="dialog-heading">
        <div>
          <span class="section-kicker">客户端更新</span>
          <h2 id="update-dialog-title">{{ updateDialogTitle }}</h2>
        </div>
        <button
          class="icon-button"
          :disabled="updateStage === 'downloading'"
          aria-label="关闭更新窗口"
          @click="closeUpdateDialog"
        >
          <X :size="20" />
        </button>
      </div>
      <div class="dialog-body update-dialog-body">
        <div v-if="updateStage === 'available'" class="update-summary">
          <div class="update-version-badge">
            <RefreshCw :size="20" /> v{{ updateVersion }}
          </div>
          <p>当前版本 v{{ appVersion }}，发现新的客户端版本。</p>
          <h3>更新内容</h3>
          <ul v-if="updateNotes.length" class="release-notes">
            <li v-for="note in updateNotes" :key="note">{{ note }}</li>
          </ul>
          <p v-else class="muted">本次更新没有附加说明。</p>
        </div>
        <div v-else-if="updateStage === 'downloading'" class="update-progress">
          <div class="progress-heading">
            <strong>正在下载 v{{ updateVersion }}</strong
            ><span>{{ status.clientUpdate.progress }}%</span>
          </div>
          <div
            class="progress-track"
            role="progressbar"
            :aria-valuenow="status.clientUpdate.progress"
            aria-valuemin="0"
            aria-valuemax="100"
          >
            <span :style="{ width: `${status.clientUpdate.progress}%` }"></span>
          </div>
          <p class="muted">下载完成后可安装更新。</p>
        </div>
        <div v-else-if="updateStage === 'ready'" class="update-summary">
          <div class="update-version-badge success">
            <Check :size="20" /> 下载完成
          </div>
          <p>
            v{{ updateVersion }}
            已下载并通过完整性校验。安装时客户端会自动退出并重新启动。
          </p>
          <p v-if="connected" class="context-hint">
            请先断开当前网络连接，再继续安装。
          </p>
        </div>
        <div v-else class="update-summary">
          <div class="update-version-badge warning">
            <CircleHelp :size="20" />更新失败
          </div>
          <StatusNotice
            v-if="status.clientUpdate.message"
            :text="status.clientUpdate.message"
          />
          <p>可在“设置 → 关于”重试。</p>
        </div>
      </div>
      <div class="dialog-footer">
        <button
          v-if="updateStage !== 'downloading'"
          class="button secondary"
          :disabled="busy && operation !== 'download_update'"
          @click="closeUpdateDialog"
        >
          稍后处理
        </button>
        <button
          v-if="updateStage === 'available' || updateStage === 'error'"
          class="button primary"
          :disabled="busy"
          @click="startUpdateDownload"
        >
          <LoaderCircle
            v-if="operation === 'download_update'"
            class="spin"
            :size="16"
          />
          {{ updateStage === "error" ? "重新下载" : "下载更新" }}
        </button>
        <button
          v-if="updateStage === 'ready'"
          class="button primary"
          :disabled="busy || connected"
          @click="installDownloadedUpdate"
        >
          <RefreshCw :size="16" />{{ connected ? "请先断开连接" : "现在更新" }}
        </button>
      </div>
    </dialog>
    <dialog
      ref="account"
      class="account-dialog"
      aria-labelledby="account-title"
      @cancel="busy && $event.preventDefault()"
    >
      <div class="dialog-heading">
        <h2 id="account-title">网络账户</h2>
        <button
          class="icon-button"
          :disabled="busy"
          aria-label="关闭账户窗口"
          @click="
            account?.close();
            closePasswordChange();
          "
        >
          <X :size="20" />
        </button>
      </div>
      <div class="dialog-body">
        <div class="account-summary">
          <span class="avatar large"><UserRound :size="24" /></span>
          <div>
            <strong>{{ status.username }}</strong>
            <p>{{ status.role === "admin" ? "管理员账户" : "网络账户" }}</p>
          </div>
        </div>
        <p>
          {{
            connected
              ? "退出登录会断开当前网络连接并清理隧道。"
              : "退出后需要重新登录才能连接内部网络。"
          }}
        </p>
        <button class="button secondary account-password-button" :disabled="busy" @click="openInvitations">邀请用户</button>
        <button
          v-if="!passwordChangeOpen"
          class="button secondary account-password-button"
          :disabled="busy"
          @click="openPasswordChange"
        >
          <KeyRound :size="16" /> 修改密码
        </button>
        <form
          v-else
          class="password-change-form"
          @submit.prevent="submitPasswordChange"
        >
          <div class="password-change-heading">
            <div>
              <strong>修改登录密码</strong>
              <p>修改后当前连接不会中断，下次登录请使用新密码。</p>
            </div>
            <button
              type="button"
              class="icon-button"
              aria-label="取消修改密码"
              :disabled="busy"
              @click="closePasswordChange"
            >
              <X :size="17" />
            </button>
          </div>
          <label for="account-current-password">当前密码</label>
          <input
            id="account-current-password"
            v-model="currentPassword"
            type="password"
            autocomplete="current-password"
            :disabled="busy"
          />
          <label for="account-new-password">新密码</label>
          <input
            id="account-new-password"
            v-model="newPassword"
            type="password"
            autocomplete="new-password"
            minlength="6"
            maxlength="256"
            :disabled="busy"
          />
          <label for="account-confirm-password">确认新密码</label>
          <input
            id="account-confirm-password"
            v-model="confirmPassword"
            type="password"
            autocomplete="new-password"
            minlength="6"
            maxlength="256"
            :disabled="busy"
          />
          <div
            v-if="passwordFeedback"
            class="inline-notice"
            :class="passwordFeedback.startsWith('密码已') ? 'info' : 'error'"
            :role="passwordFeedback.startsWith('密码已') ? 'status' : 'alert'"
          >
            <p>{{ passwordFeedback }}</p>
          </div>
          <div class="password-change-actions">
            <button
              type="button"
              class="button secondary"
              :disabled="busy"
              @click="closePasswordChange"
            >
              取消
            </button>
            <button type="submit" class="button primary" :disabled="busy">
              <LoaderCircle
                v-if="operation === 'change_password'"
                class="spin"
                :size="16"
              />
              {{ operation === "change_password" ? "正在保存…" : "保存新密码" }}
            </button>
          </div>
        </form>
        <div
          v-if="notice?.kind === 'error'"
          class="inline-notice error"
          role="alert"
        >
          <p>{{ notice.text }}</p>
        </div>
      </div>
      <div class="dialog-footer">
        <button
          class="button secondary"
          :disabled="busy"
          @click="
            account?.close();
            closePasswordChange();
          "
        >
          取消</button
        ><button class="button danger" :disabled="busy" @click="logout">
          <LoaderCircle
            v-if="operation === 'logout'"
            class="spin"
            :size="16"
          /><LogOut v-else :size="16" />{{
            operation === "logout" ? "正在退出…" : "退出登录"
          }}
        </button>
      </div>
    </dialog>
    <dialog ref="invitationDialog" aria-labelledby="invitation-title" class="invitation-dialog" @close="invitationsOpen=false" @cancel="busy && $event.preventDefault()">
      <div class="dialog-heading"><h2 id="invitation-title">邀请用户</h2><button class="icon-button" aria-label="关闭邀请窗口" :disabled="busy" @click="invitationDialog?.close()"><X :size="20" /></button></div>
      <div class="dialog-body"><InvitationPanel v-if="invitationsOpen&&status.authenticated" :busy="busy" :request="client.invitationRequest" /></div>
    </dialog>
  </div>
</template>
