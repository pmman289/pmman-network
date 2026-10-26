<script setup lang="ts">
import { computed, reactive, ref } from 'vue';
import { Search } from '@element-plus/icons-vue';
import { ElMessage } from 'element-plus';
import type { FormInstance, FormRules } from 'element-plus';
import { api } from '../api/admin';
import { useAdminStore } from '../stores/admin';
import type { Node, NodeBirdConfig, NodeNetworkConfig } from '../types';
import { usePagination, useMutation, confirmAction, formatDate } from '../composables/useAdmin';
import PageHeading from '../components/PageHeading.vue';
import ListPagination from '../components/ListPagination.vue';
import NodeEnrollmentDialog from '../components/NodeEnrollmentDialog.vue';
import GeositeDialog from '../components/GeositeDialog.vue';
const store = useAdminStore();
const filters = reactive({ query: '', status: '' });
const statusOf = (node: Node) =>
  !node.registered ? 'pending' : !node.enabled ? 'disabled' : node.healthy ? 'online' : 'offline';
const enrollmentDialog = ref(false);
const geositeDialog = ref(false);
const filtered = computed(() =>
  store.nodes.filter(
    (n) =>
      `${n.name} ${n.nodeId} ${n.region ?? ''}`
        .toLowerCase()
        .includes(filters.query.trim().toLowerCase()) &&
      (!filters.status || statusOf(n) === filters.status),
  ),
);
const { page, size, rows } = usePagination(filtered);
const { saving, run } = useMutation();
const target = ref<Node | null>(null);
const dialog = ref(false);
const birdConfig = ref<NodeBirdConfig | null>(null);
const birdLoading = ref(false);
const birdError = ref('');
const form = reactive({
  name: '',
  region: '',
  wireguard_endpoint: '',
  probe_endpoint: '',
  route_table: '',
  enabled: true,
  connectable: true,
  uploadMbps: 0,
  downloadMbps: 0,
  gateway_ipv4: '',
  gateway_ipv6: '',
  dns_servers: '',
  pmmandns_upstreams: '',
  pmmandns_domestic_upstreams: '',
  pmmandns_foreign_upstreams: '',
  pmmandns_default_group: 'foreign',
  pmmandns_domestic_rules: '',
  pmmandns_foreign_rules: '',
  pmmandns_rules: '',
  pmmandns_geosite_enabled: false,
});
const formRef = ref<FormInstance>();
const rules: FormRules = {
  name: [{ required: true, message: '请输入节点名称', trigger: 'blur' }],
  wireguard_endpoint: [
    { required: true, message: '请输入 WireGuard 公网地址', trigger: 'blur' },
    {
      pattern: /^(\[[0-9a-fA-F:]+\]|[^:\s]+):\d{1,5}$/,
      message: '格式应为 host:port 或 [IPv6]:port',
      trigger: 'blur',
    },
  ],
  probe_endpoint: [
    { required: true, message: '请输入 UDP 探测地址', trigger: 'blur' },
    {
      pattern: /^(\[[0-9a-fA-F:]+\]|[^:\s]+):\d{1,5}$/,
      message: '格式应为 host:port 或 [IPv6]:port',
      trigger: 'blur',
    },
  ],
  route_table: [
    {
      pattern: /^[A-Za-z0-9_.-]{0,64}$/,
      message: '仅允许字母、数字、下划线、点和短横线，最长 64 位',
      trigger: 'blur',
    },
  ],
};
function edit(node: Node) {
  target.value = { ...node };
  birdConfig.value = null;
  birdError.value = '';
  const network = node.network ?? ({} as NodeNetworkConfig);
  Object.assign(form, {
    name: node.name,
    region: node.region ?? '',
    wireguard_endpoint: node.wireguardEndpoint,
    probe_endpoint: node.probeEndpoint,
    route_table: node.routeTable ?? '',
    enabled: node.enabled,
    connectable: node.connectable,
    uploadMbps: node.rateLimit?.uploadMbps ?? 0,
    downloadMbps: node.rateLimit?.downloadMbps ?? 0,
    gateway_ipv4: network.gateway_ipv4 ?? '',
    gateway_ipv6: network.gateway_ipv6 ?? '',
    dns_servers: network.dns_servers?.join(', ') ?? '',
    pmmandns_upstreams: network.pmmandns_upstreams?.join(', ') ?? '',
    pmmandns_domestic_upstreams: network.pmmandns_domestic_upstreams?.join(', ') ?? '',
    pmmandns_foreign_upstreams: network.pmmandns_foreign_upstreams?.join(', ') ?? '',
    pmmandns_default_group: network.pmmandns_default_group || 'foreign',
    pmmandns_geosite_enabled: network.pmmandns_geosite_enabled ?? false,
    pmmandns_domestic_rules: network.pmmandns_domestic_rules?.join('\n') ?? '',
    pmmandns_foreign_rules: network.pmmandns_foreign_rules?.join('\n') ?? '',
    pmmandns_rules: (network.pmmandns_rules ?? [])
      .map((rule) => `${rule.suffix}=${rule.upstreams.join(',')}`)
      .join('\n'),
  });
  dialog.value = true;
  void loadBirdConfig(node.nodeId);
}
async function loadBirdConfig(nodeId: string) {
  birdLoading.value = true;
  birdError.value = '';
  try {
    birdConfig.value = await api.nodeBirdConfig(nodeId);
  } catch (cause) {
    birdError.value = cause instanceof Error ? cause.message : 'BIRD 模板读取失败';
  } finally {
    birdLoading.value = false;
  }
}
async function copyBirdText(value: string) {
  try {
    await navigator.clipboard.writeText(value);
    ElMessage.success('已复制到剪贴板');
  } catch {
    ElMessage.error('复制失败，请手动选择文本复制');
  }
}
function downloadBirdConfig() {
  if (!birdConfig.value) return;
  const blob = new Blob([birdConfig.value.config], { type: 'text/plain;charset=utf-8' });
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = birdConfig.value.fileName;
  link.click();
  URL.revokeObjectURL(url);
}
async function save() {
  if (!target.value || !(await formRef.value?.validate().catch(() => false))) return;
  const changedEndpoint =
    target.value.wireguardEndpoint !== form.wireguard_endpoint.trim() ||
    target.value.probeEndpoint !== form.probe_endpoint.trim();
  const changedIdentity =
    target.value.name !== form.name.trim() || target.value.region !== (form.region.trim() || null);
  const changedTable = target.value.routeTable !== (form.route_table.trim() || null);
  const changedLimit =
    target.value.rateLimit?.uploadMbps !== form.uploadMbps ||
    target.value.rateLimit?.downloadMbps !== form.downloadMbps;
  const splitList = (value: string) => {
    const values = value
      .split(/[\s,;]+/)
      .map((item) => item.trim())
      .filter(Boolean);
    return values.length ? values : null;
  };
  const parseDomainRules = (value: string) =>
    value
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter((line) => line && !line.startsWith('#'))
      .map((line) =>
        line
          .replace(/^domain:/i, '')
          .replace(/^\.+/, '')
          .trim(),
      );
  const parseDnsRules = (value: string) =>
    value
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter((line) => line && !line.startsWith('#'))
      .map((line) => {
        const [rawSuffix, rawUpstreams] = line.split('=', 2);
        if (!rawUpstreams) throw new Error(`DNS 分流规则缺少等号：${line}`);
        const suffix = rawSuffix
          .trim()
          .replace(/^domain:/i, '')
          .replace(/^\.+/, '');
        const upstreams = rawUpstreams
          .split(/[\s,;]+/)
          .map((item) => item.trim())
          .filter(Boolean);
        if (!suffix || !upstreams.length) throw new Error(`DNS 分流规则格式无效：${line}`);
        return { suffix, upstreams };
      });
  const oldNetwork = target.value.network;
  const network: NodeNetworkConfig = {
    pmmandns_geosite_enabled: form.pmmandns_geosite_enabled,
    gateway_ipv4: form.gateway_ipv4.trim() || null,
    gateway_ipv6: form.gateway_ipv6.trim() || null,
    dns_servers: splitList(form.dns_servers) ?? [],
    pmmandns_upstreams: splitList(form.pmmandns_upstreams) ?? [],
    pmmandns_domestic_upstreams: splitList(form.pmmandns_domestic_upstreams) ?? [],
    pmmandns_foreign_upstreams: splitList(form.pmmandns_foreign_upstreams) ?? [],
    pmmandns_default_group: form.pmmandns_default_group,
    pmmandns_domestic_rules: parseDomainRules(form.pmmandns_domestic_rules),
    pmmandns_foreign_rules: parseDomainRules(form.pmmandns_foreign_rules),
    pmmandns_rules: parseDnsRules(form.pmmandns_rules),
    // Keep legacy fields intact for old clients, but do not expose or edit
    // them. New clients use the built-in cnipgen snapshot.
    ipv4_split_routes: oldNetwork?.ipv4_split_routes ?? null,
    ipv6_split_routes: oldNetwork?.ipv6_split_routes ?? null,
  };
  const changedNetwork = JSON.stringify(oldNetwork) !== JSON.stringify(network);
  if (
    (changedTable ||
      changedLimit ||
      changedEndpoint ||
      changedIdentity ||
      changedNetwork ||
      !form.enabled) &&
    !(await confirmAction(
      !form.enabled
        ? '停用节点将撤销该节点的活动连接，确认保存？'
        : changedEndpoint
          ? '修改节点公网接入地址会影响新客户端连接，确认保存？'
          : changedIdentity
            ? '修改节点名称或区域会影响管理识别和自动选节点，确认保存？'
            : changedNetwork
              ? '修改节点网关、DNS 或 pmmandns 配置会影响后续客户端；请确认节点网络可达。确认保存？'
              : changedLimit
                ? '修改限速会影响该节点上的客户端传输，确认保存？'
                : '修改返回路由表会迁移客户端主机路由，请确保网络已使用该路由表。确认保存？',
    ))
  )
    return;
  await run(
    () =>
      api.node(target.value!.nodeId, {
        name: form.name.trim(),
        region: form.region.trim(),
        wireguard_endpoint: form.wireguard_endpoint.trim(),
        probe_endpoint: form.probe_endpoint.trim(),
        route_table: form.route_table.trim() || null,
        enabled: form.enabled,
        connectable: form.connectable,
        rate_limit: { uploadMbps: form.uploadMbps, downloadMbps: form.downloadMbps },
        network,
      }),
    '节点配置已保存，将通过下一次心跳同步',
    () => {
      dialog.value = false;
    },
  );
}
async function cancel(node: Node) {
  if (!(await confirmAction(`取消 ${node.name} 的接入将立即作废凭据，确认取消？`))) return;
  await run(() => api.cancelEnrollment(node.nodeId), '节点接入已取消，凭据已作废');
}
async function updateNode(node: Node) {
  if (!node.registered) return;
  if (
    !(await confirmAction(
      `将通过主控通知 ${node.name} 下载并重启节点服务，活动连接会在重启期间短暂中断。确认更新？`,
    ))
  )
    return;
  await run(() => api.nodeUpdate(node.nodeId), '节点更新指令已下发，等待下一次心跳执行');
}
</script>
<template>
  <PageHeading title="节点管理" description="管理节点接入、网络参数和资源版本" />
  <el-card shadow="never"
    ><div class="filter-bar">
      <el-input
        v-model="filters.query"
        :prefix-icon="Search"
        placeholder="搜索节点名称、ID 或区域"
        aria-label="搜索节点"
        clearable
      /><el-select v-model="filters.status" placeholder="全部状态" aria-label="节点状态" clearable
        ><el-option label="待接入" value="pending" /><el-option
          label="在线"
          value="online" /><el-option label="离线" value="offline" /><el-option
          label="已停用"
          value="disabled" /></el-select
      ><el-button @click="Object.assign(filters, { query: '', status: '' })">重置</el-button>
      <el-button type="primary" @click="enrollmentDialog = true">接入节点</el-button>
      <el-button @click="geositeDialog = true">域名规则</el-button>
    </div>
    <el-table
      v-loading="store.loading"
      :data="rows"
      row-key="nodeId"
      empty-text="暂无符合条件的节点"
      ><el-table-column label="节点" min-width="170"
        ><template #default="{ row }"
          ><div class="cell-primary">{{ row.name }}</div>
          <div class="cell-secondary">{{ row.nodeId }}</div></template
        ></el-table-column
      ><el-table-column prop="region" label="区域" min-width="120" /><el-table-column
        prop="wireguardEndpoint"
        label="WireGuard 地址"
        min-width="250"
        show-overflow-tooltip
      /><el-table-column
        prop="probeEndpoint"
        label="UDP 探测地址"
        min-width="220"
        show-overflow-tooltip
      /><el-table-column label="返回路由表" min-width="140"
        ><template #default="{ row }"
          ><code>{{ row.routeTable || '未启用' }}</code></template
        ></el-table-column
      ><el-table-column label="节点限速" min-width="180"
        ><template #default="{ row }">{{
          row.rateLimit?.uploadMbps || row.rateLimit?.downloadMbps
            ? `上行 ${row.rateLimit.uploadMbps || '不限'} / 下行 ${row.rateLimit.downloadMbps || '不限'} Mbps`
            : '不限速'
        }}</template></el-table-column
      ><el-table-column label="资源版本" min-width="180"
        ><template #default="{ row }"
          ><code>{{ row.resourceVersion || '未上报' }}</code>
          <div
            v-if="row.desiredResourceVersion && row.desiredResourceVersion !== row.resourceVersion"
            class="cell-secondary"
          >
            待更新 {{ row.desiredResourceVersion }}
          </div></template
        ></el-table-column
      ><el-table-column label="状态" width="110"
        ><template #default="{ row }"
          ><el-tag :type="row.healthy ? 'success' : 'info'">{{
            !row.registered
              ? '待接入'
              : !row.enabled
                ? '已停用'
                : row.healthy && !row.connectable
                  ? '仅转发入口'
                  : row.healthy
                    ? '在线'
                    : '离线'
          }}</el-tag></template
        ></el-table-column
      ><el-table-column label="最后心跳" min-width="180"
        ><template #default="{ row }">{{
          row.registered ? formatDate(row.lastSeenAt) : '尚未注册'
        }}</template></el-table-column
      ><el-table-column label="操作" width="220" fixed="right"
        ><template #default="{ row }"
          ><el-button type="primary" link :disabled="saving" @click="edit(row)">配置</el-button
          ><el-button
            v-if="row.registered"
            type="warning"
            link
            :disabled="saving"
            @click="updateNode(row)"
            >一键更新</el-button
          ><el-button
            v-if="!row.registered"
            type="danger"
            link
            :disabled="saving"
            @click="cancel(row)"
            >取消接入</el-button
          ></template
        ></el-table-column
      ></el-table
    ><ListPagination v-model:page="page" v-model:size="size" :total="filtered.length" />
  </el-card>
  <el-dialog
    v-model="dialog"
    :title="`节点配置 · ${target?.name ?? ''}`"
    width="620px"
    :close-on-click-modal="false"
    :close-on-press-escape="!saving"
    :show-close="!saving"
    destroy-on-close
  >
    <el-descriptions :column="1" border class="node-details"
      ><el-descriptions-item label="节点 ID">{{
        target?.nodeId
      }}</el-descriptions-item></el-descriptions
    >
    <el-form ref="formRef" :model="form" :rules="rules" label-position="top" @submit.prevent="save">
      <el-tabs type="border-card">
        <el-tab-pane label="基础信息">
          <el-form-item label="节点名称" prop="name"
            ><el-input v-model="form.name" maxlength="128"
          /></el-form-item>
          <el-form-item label="区域" prop="region"
            ><el-input v-model="form.region" maxlength="128" placeholder="例如 Guangzhou"
          /></el-form-item>
          <el-form-item label="WireGuard 公网接入地址" prop="wireguard_endpoint">
            <el-input v-model="form.wireguard_endpoint" placeholder="例如 vpn.example.com:51820" />
            <div class="form-help">客户端通过此地址连接节点；修改不会改变节点本机监听配置。</div>
          </el-form-item>
          <el-form-item label="UDP 探测地址" prop="probe_endpoint">
            <el-input v-model="form.probe_endpoint" placeholder="例如 probe.example.com:51840" />
            <div class="form-help">客户端使用 UDP 探测此地址计算延迟，不经过 HTTP。</div>
          </el-form-item>
          <el-form-item label="WireGuard 服务端公钥">
            <el-input
              :model-value="target?.serverPublicKey || '等待节点首次注册后自动生成'"
              readonly
            />
            <div class="form-help">
              公钥由节点首次启动时本地生成并上报，主控不接受手工填写或修改。
            </div>
          </el-form-item>
          <el-form-item label="返回路由表" prop="route_table">
            <el-input
              v-model="form.route_table"
              placeholder="例如 51830 或 ospf_return；留空不安装路由"
              maxlength="64"
            />
            <div class="form-help">节点在此表安装客户端 IPv4 /32、IPv6 /128 主机路由。</div>
          </el-form-item>
          <el-form-item label="启用节点服务">
            <el-switch v-model="form.enabled" />
            <div class="form-help">停用会撤销该节点上的活动连接，端口转发入口也会停止。</div>
          </el-form-item>
          <el-form-item label="允许客户端连接">
            <el-switch v-model="form.connectable" />
            <div class="form-help">
              关闭后节点仍可作为端口转发入口，但不会出现在客户端连接节点列表或 P2P 中转选择中。
            </div>
          </el-form-item>
        </el-tab-pane>
        <el-tab-pane label="路由广播">
          <el-alert
            title="主控不会自动修改节点上的 BIRD"
            description="请登录节点，确认现有 bird.conf、OSPF 邻居和策略路由后，再安装或调整下面的模板。"
            type="warning"
            :closable="false"
            show-icon
          />
          <div class="bird-toolbar">
            <el-button :loading="birdLoading" @click="target && loadBirdConfig(target.nodeId)">
              刷新模板
            </el-button>
            <el-button
              :disabled="!birdConfig"
              @click="birdConfig && copyBirdText(birdConfig.config)"
            >
              复制 BIRD 配置
            </el-button>
            <el-button type="primary" :disabled="!birdConfig" @click="downloadBirdConfig">
              下载配置文件
            </el-button>
          </div>
          <el-alert v-if="birdError" :title="birdError" type="error" :closable="false" show-icon />
          <el-skeleton v-if="birdLoading" :rows="8" animated />
          <template v-else-if="birdConfig">
            <el-descriptions :column="2" border class="bird-summary">
              <el-descriptions-item label="WireGuard 接口">
                <code>{{ birdConfig.interface || '未获取' }}</code>
              </el-descriptions-item>
              <el-descriptions-item label="返回路由表">
                <code>{{ birdConfig.routeTable || '未配置' }}</code>
              </el-descriptions-item>
              <el-descriptions-item label="模板状态">
                <el-tag :type="birdConfig.ready ? 'success' : 'warning'">
                  {{ birdConfig.ready ? '可直接使用' : '需要替换占位值' }}
                </el-tag>
              </el-descriptions-item>
              <el-descriptions-item label="配置文件">
                <code>{{ birdConfig.fileName }}</code>
              </el-descriptions-item>
            </el-descriptions>
            <el-alert
              v-for="warning in birdConfig.warnings"
              :key="warning"
              :title="warning"
              type="warning"
              :closable="false"
              show-icon
            />
            <el-form-item label="pmman_network.conf">
              <el-input :model-value="birdConfig.config" type="textarea" :rows="14" readonly />
            </el-form-item>
            <el-form-item label="IPv4 OSPF export 规则">
              <el-input
                :model-value="birdConfig.ospfIpv4FilterSnippet"
                type="textarea"
                :rows="2"
                readonly
              />
            </el-form-item>
            <el-form-item label="IPv6 OSPF export 规则">
              <el-input
                :model-value="birdConfig.ospfIpv6FilterSnippet"
                type="textarea"
                :rows="2"
                readonly
              />
            </el-form-item>
            <el-form-item label="安装步骤">
              <el-input
                :model-value="birdConfig.installationSteps.join('\n')"
                type="textarea"
                :rows="6"
                readonly
              />
            </el-form-item>
            <el-form-item label="节点验证命令">
              <el-input
                :model-value="birdConfig.verificationCommands.join('\n')"
                type="textarea"
                :rows="9"
                readonly
              />
            </el-form-item>
          </template>
        </el-tab-pane>
        <el-tab-pane label="网络与 DNS">
          <el-form-item label="Geosite 分流">
            <el-switch v-model="form.pmmandns_geosite_enabled" />
            <div class="form-help">使用主控资源中的国内/国外分类；请先配置两组上游 DNS。</div>
          </el-form-item>
          <el-alert
            title="pmmandns 国内外分流"
            description="使用主控发布的 geosite 资源。"
            type="info"
            :closable="false"
            show-icon
          />
          <el-row :gutter="16">
            <el-col :span="12"
              ><el-form-item label="IPv4 网关地址（CIDR）"
                ><el-input
                  v-model="form.gateway_ipv4"
                  placeholder="例如 10.99.0.1/24" /></el-form-item
            ></el-col>
            <el-col :span="12"
              ><el-form-item label="IPv6 网关地址（CIDR）"
                ><el-input
                  v-model="form.gateway_ipv6"
                  placeholder="例如 fd99:1000::1/64" /></el-form-item
            ></el-col>
          </el-row>
          <el-form-item label="pmmandns 监听地址"
            ><el-input
              v-model="form.dns_servers"
              placeholder="例如 10.99.0.1, fd99:1000::1；留空自动使用网关地址"
            />
            <div class="form-help">
              使用网关地址且未填写任何上游时自动生成默认配置；可按需修改。
            </div>
          </el-form-item>
          <el-form-item label="兼容默认上游 DNS"
            ><el-input
              v-model="form.pmmandns_upstreams"
              placeholder="旧配置兼容；填写后优先于默认分组"
          /></el-form-item>
          <el-row :gutter="16">
            <el-col :span="12"
              ><el-form-item label="国内上游 DNS"
                ><el-input
                  v-model="form.pmmandns_domestic_upstreams"
                  placeholder="114.114.114.114:53, 223.5.5.5:53" /></el-form-item
            ></el-col>
            <el-col :span="12"
              ><el-form-item label="国外上游 DNS"
                ><el-input
                  v-model="form.pmmandns_foreign_upstreams"
                  placeholder="1.1.1.1:53, 8.8.8.8:53" /></el-form-item
            ></el-col>
          </el-row>
          <el-form-item label="未命中规则时的默认分组"
            ><el-radio-group v-model="form.pmmandns_default_group"
              ><el-radio value="foreign">国外</el-radio
              ><el-radio value="domestic">国内</el-radio></el-radio-group
            ></el-form-item
          >
          <el-row :gutter="16">
            <el-col :span="12"
              ><el-form-item label="国内域名规则"
                ><el-input
                  v-model="form.pmmandns_domestic_rules"
                  type="textarea"
                  :rows="6"
                  placeholder="domain:baidu.com&#10;domain:qq.com" /></el-form-item
            ></el-col>
            <el-col :span="12"
              ><el-form-item label="国外域名规则"
                ><el-input
                  v-model="form.pmmandns_foreign_rules"
                  type="textarea"
                  :rows="6"
                  placeholder="domain:google.com&#10;domain:youtube.com" /></el-form-item
            ></el-col>
          </el-row>
          <el-form-item label="高级精确规则（兼容旧配置）"
            ><el-input
              v-model="form.pmmandns_rules"
              type="textarea"
              :rows="3"
              placeholder="example.com=1.1.1.1:53"
          /></el-form-item>
        </el-tab-pane>
        <el-tab-pane label="路由与限速">
          <el-alert
            title="境外分流由客户端内置 cnipgen 双栈规则生成。节点上的旧分流字段仅为旧版客户端保留，不在此页面维护。"
            type="info"
            :closable="false"
            show-icon
          />
          <el-row :gutter="16">
            <el-col :span="12"
              ><el-form-item label="客户端上行限速（Mbps）"
                ><el-input-number
                  v-model="form.uploadMbps"
                  :min="0"
                  :max="100000"
                  :precision="0"
                  controls-position="right"
                />
                <div class="form-help">0 表示不限速；P2P 直连不受此限制。</div></el-form-item
              ></el-col
            >
            <el-col :span="12"
              ><el-form-item label="客户端下行限速（Mbps）"
                ><el-input-number
                  v-model="form.downloadMbps"
                  :min="0"
                  :max="100000"
                  :precision="0"
                  controls-position="right" /></el-form-item
            ></el-col>
          </el-row>
        </el-tab-pane>
      </el-tabs>
    </el-form>
    <template #footer
      ><el-button :disabled="saving" @click="dialog = false">取消</el-button
      ><el-button type="primary" :loading="saving" @click="save">保存配置</el-button></template
    >
  </el-dialog>
  <NodeEnrollmentDialog v-model="enrollmentDialog" />
  <GeositeDialog v-model="geositeDialog" />
</template>
