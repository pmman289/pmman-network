<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue';
import type { FormInstance, FormRules } from 'element-plus';
import { api } from '../api/admin';
import { useMutation } from '../composables/useAdmin';
import type { NodeEnrollment, NodeNetworkConfig } from '../types';

const visible = defineModel<boolean>({ required: true });
const { saving, run } = useMutation();
const formRef = ref<FormInstance>();
const result = ref<NodeEnrollment | null>(null);
const downloaded = ref(false);
const defaults = () => ({
  node_id: '',
  name: '',
  region: '',
  route_table: '',
  connectable: true,
  probe_endpoint: '',
  wireguard_endpoint: '',
  interface: 'pmman0',
  wireguard_ipv4: '',
  wireguard_ipv6: '',
  dns_ipv4: '',
  dns_ipv6: '',
  pmmandns_upstreams: '',
  pmmandns_domestic_upstreams: '',
  pmmandns_foreign_upstreams: '',
  pmmandns_default_group: 'foreign',
  pmmandns_domestic_rules: '',
  pmmandns_foreign_rules: '',
  pmmandns_rules: '',
  pmmandns_geosite_enabled: false,
  master_url: window.location.origin,
  probe_listen: '0.0.0.0:51840',
});
const form = reactive(defaults());
watch(visible, (open) => {
  if (!open) {
    result.value = null;
    downloaded.value = false;
    Object.assign(form, defaults());
  }
});
const validUrl = (value: string) => {
  try {
    const url = new URL(value);
    return (
      (url.protocol === 'https:' ||
        (url.protocol === 'http:' && ['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname))) &&
      !url.username &&
      !url.password &&
      url.pathname === '/' &&
      !url.search &&
      !url.hash
    );
  } catch {
    return false;
  }
};
const urlRule = {
  validator: (_: unknown, value: string, done: (error?: Error) => void) =>
    done(validUrl(value) ? undefined : new Error('请输入 HTTPS 根地址，本机回环地址可使用 HTTP')),
  trigger: 'blur',
};
const udpRule = {
  required: true,
  validator: (_: unknown, value: string, done: (error?: Error) => void) => {
    const match = value.match(/^(?:[A-Za-z0-9.-]+|\[[0-9a-fA-F:]+\]):(\d+)$/);
    done(
      match && Number(match[1]) > 0 && Number(match[1]) <= 65535
        ? undefined
        : new Error('请输入域名或 IP:UDP端口，IPv6 地址使用方括号'),
    );
  },
  trigger: 'blur',
};
const cidrRule = (label: string, ipv6: boolean) => ({
  required: true,
  validator: (_: unknown, value: string, done: (error?: Error) => void) => {
    const pattern = ipv6
      ? /^[0-9a-fA-F:]+\/(?:\d|[1-9]\d|1[01]\d|12[0-8])$/
      : /^\d{1,3}(?:\.\d{1,3}){3}\/(?:\d|[12]\d|3[0-2])$/;
    done(pattern.test(value.trim()) ? undefined : new Error(`请输入有效的${label} CIDR`));
  },
  trigger: 'blur',
});
const rules: FormRules = {
  node_id: [
    {
      required: true,
      pattern: /^[A-Za-z0-9_-]{1,64}$/,
      message: '1–64 位字母、数字、短横线或下划线',
      trigger: 'blur',
    },
  ],
  name: [
    {
      required: true,
      whitespace: true,
      max: 128,
      message: '请输入节点名称（最长 128 位）',
      trigger: 'blur',
    },
  ],
  probe_endpoint: [udpRule],
  master_url: [urlRule],
  route_table: [
    {
      pattern: /^[A-Za-z0-9_.-]{0,64}$/,
      message: '仅允许字母、数字、下划线、点和短横线，最长 64 位',
      trigger: 'blur',
    },
  ],
  wireguard_endpoint: [udpRule],
  wireguard_ipv4: [cidrRule('IPv4 网关', false)],
  wireguard_ipv6: [cidrRule('IPv6 网关', true)],
  interface: [
    {
      pattern: /^[A-Za-z0-9_.-]{1,15}$/,
      message: '接口名称最长 15 位，仅允许字母、数字、点、下划线和短横线',
      trigger: 'blur',
    },
  ],
  probe_listen: [
    {
      required: true,
      validator: (_: unknown, value: string, done: (error?: Error) => void) => {
        const match = value.match(/^(?:0\.0\.0\.0|127\.0\.0\.1|\[::\]):(\d+)$/);
        done(
          match && Number(match[1]) > 0 && Number(match[1]) <= 65535
            ? undefined
            : new Error('请输入 0.0.0.0:端口 或 [::]:端口，监听 UDP 探测'),
        );
      },
      trigger: 'blur',
    },
  ],
};
async function create() {
  if (!(await formRef.value?.validate().catch(() => false))) return;
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
  const network: NodeNetworkConfig = {
    pmmandns_geosite_enabled: form.pmmandns_geosite_enabled,
    dns_servers: [form.dns_ipv4.trim(), form.dns_ipv6.trim()].filter(Boolean),
    pmmandns_upstreams: form.pmmandns_upstreams
      .split(/[\s,;]+/)
      .map((item) => item.trim())
      .filter(Boolean),
    pmmandns_domestic_upstreams: form.pmmandns_domestic_upstreams
      .split(/[\s,;]+/)
      .map((item) => item.trim())
      .filter(Boolean),
    pmmandns_foreign_upstreams: form.pmmandns_foreign_upstreams
      .split(/[\s,;]+/)
      .map((item) => item.trim())
      .filter(Boolean),
    pmmandns_default_group: form.pmmandns_default_group,
    pmmandns_domestic_rules: parseDomainRules(form.pmmandns_domestic_rules),
    pmmandns_foreign_rules: parseDomainRules(form.pmmandns_foreign_rules),
    pmmandns_rules: parseDnsRules(form.pmmandns_rules),
    gateway_ipv4: form.wireguard_ipv4.trim() || null,
    gateway_ipv6: form.wireguard_ipv6.trim() || null,
  };
  await run(async () => {
    result.value = await api.enrollNode({
      node_id: form.node_id,
      name: form.name.trim(),
      region: form.region.trim() || null,
      route_table: form.route_table.trim() || null,
      connectable: form.connectable,
      wireguard_interface: form.interface.trim() || null,
      network,
    });
  }, '接入记录已创建，请保存配置并在节点启动服务');
}
const configuration = computed(() => {
  if (!result.value) return '';
  const values: Record<string, string> = {
    PMMAN_NODE_ID: result.value.node.nodeId,
    PMMAN_NODE_NAME: result.value.node.name,
    PMMAN_NODE_REGION: result.value.node.region ?? '',
    PMMAN_NODE_TOKEN: result.value.node_token,
    PMMAN_MASTER_URL: form.master_url.replace(/\/$/, ''),
    PMMAN_PROBE_ENDPOINT: form.probe_endpoint,
    PMMAN_PROBE_LISTEN: form.probe_listen,
    PMMAN_WG_INTERFACE: form.interface,
    PMMAN_WG_IPV4_ADDRESS: form.wireguard_ipv4,
    PMMAN_WG_IPV6_ADDRESS: form.wireguard_ipv6,
    PMMAN_WG_PRIVATE_KEY_FILE: '/var/lib/pmman-node/wireguard-private.key',
    PMMAN_WG_ENDPOINT: form.wireguard_endpoint,
    PMMAN_WG_CLI: '/usr/bin/wg',
    PMMAN_IP_CLI: '/usr/sbin/ip',
    PMMAN_ROUTE_TABLE: result.value.node.routeTable ?? '',
    PMMAN_DNS_IPV4: form.dns_ipv4,
    PMMAN_DNS_IPV6: form.dns_ipv6,
    PMMAN_DNS_UPSTREAMS: form.pmmandns_upstreams,
    PMMAN_DNS_DOMESTIC_UPSTREAMS: form.pmmandns_domestic_upstreams,
    PMMAN_DNS_FOREIGN_UPSTREAMS: form.pmmandns_foreign_upstreams,
    PMMAN_DNS_DEFAULT_GROUP: form.pmmandns_default_group,
    PMMAN_DNS_DOMESTIC_RULES_FILE:
      '/opt/pmman-network-node-' + result.value.node.nodeId + '/domestic.dns',
    PMMAN_DNS_FOREIGN_RULES_FILE:
      '/opt/pmman-network-node-' + result.value.node.nodeId + '/foreign.dns',
    PMMAN_SERVICE_NAME: 'pmman-network-node-' + result.value.node.nodeId,
  };
  return (
    '# Pmman Network node systemd EnvironmentFile; do not source in a shell.\n' +
    Object.entries(values)
      .map(([key, value]) => `${key}=${JSON.stringify(value)}`)
      .join('\n') +
    '\n'
  );
});
const quoteShell = (value: string) => `'${value.replace(/'/g, `'"'"'`)}'`;
const deploymentScript = computed(() => {
  if (!result.value) return '';
  const master = form.master_url.replace(/\/$/, '');
  const node = result.value.node;
  return `#!/usr/bin/env bash
set -euo pipefail

# Pmman Network node deployment. Run as root on the target Linux host.
MASTER_URL=${quoteShell(master)}
NODE_ID=${quoteShell(node.nodeId)}
NODE_TOKEN=${quoteShell(result.value.node_token)}
  INSTALL_DIR=/opt/pmman-network-node-${node.nodeId}
  BINARY_PATH="$INSTALL_DIR/pmman-node"
  ENV_PATH="$INSTALL_DIR/node.env"
  VERSION_PATH=/var/lib/pmman-node/version
  SERVICE_NAME=pmman-network-node-${node.nodeId}

if [[ "$(id -u)" != 0 ]]; then echo '请使用 root 执行此脚本' >&2; exit 1; fi
for command in curl sha256sum install systemctl wg ip; do
  command -v "$command" >/dev/null || { echo "缺少命令：$command，请先安装 WireGuard、iproute2 和 curl" >&2; exit 1; }
done

WG_ENDPOINT=${quoteShell(form.wireguard_endpoint)}
PROBE_ENDPOINT=${quoteShell(form.probe_endpoint)}
WG_INTERFACE=${quoteShell(form.interface)}
WG_IPV4_ADDRESS=${quoteShell(form.wireguard_ipv4)}
WG_IPV6_ADDRESS=${quoteShell(form.wireguard_ipv6)}
  WG_PRIVATE_KEY_FILE=/var/lib/pmman-node/wireguard-private.key
  PROBE_LISTEN=${quoteShell(form.probe_listen)}
  ROUTE_TABLE=${quoteShell(node.routeTable ?? '')}
  NODE_NAME=${quoteShell(node.name)}
  NODE_REGION=${quoteShell(node.region ?? '')}
  DNS_IPV4=${quoteShell(form.dns_ipv4)}
  DNS_IPV6=${quoteShell(form.dns_ipv6)}
  DNS_UPSTREAMS=${quoteShell(form.pmmandns_upstreams)}
  DNS_DOMESTIC_UPSTREAMS=${quoteShell(form.pmmandns_domestic_upstreams)}
  DNS_FOREIGN_UPSTREAMS=${quoteShell(form.pmmandns_foreign_upstreams)}
  DNS_DEFAULT_GROUP=${quoteShell(form.pmmandns_default_group)}
  DOMESTIC_RULES=${quoteShell(form.pmmandns_domestic_rules)}
  FOREIGN_RULES=${quoteShell(form.pmmandns_foreign_rules)}
[[ -n "$WG_ENDPOINT" ]] || read -r -p 'WireGuard 公网地址（host:port）： ' WG_ENDPOINT
[[ -n "$PROBE_ENDPOINT" ]] || read -r -p 'UDP 探测公网地址（host:port）： ' PROBE_ENDPOINT

  mkdir -p "$INSTALL_DIR" "$(dirname "$VERSION_PATH")"
  DOMESTIC_RULES_FILE="$INSTALL_DIR/domestic.dns"
  FOREIGN_RULES_FILE="$INSTALL_DIR/foreign.dns"
  printf '%s\n' "$DOMESTIC_RULES" | sed '/^[[:space:]]*$/d' > "$DOMESTIC_RULES_FILE"
  printf '%s\n' "$FOREIGN_RULES" | sed '/^[[:space:]]*$/d' > "$FOREIGN_RULES_FILE"

tmp="$INSTALL_DIR/pmman-node.download.$$"
headers="$INSTALL_DIR/pmman-node.headers.$$"
trap 'rm -f "$tmp" "$headers"' EXIT
curl --fail --silent --show-error --location --retry 3 \\
  -H "X-PMMAN-NODE-TOKEN: $NODE_TOKEN" \\
  -D "$headers" "$MASTER_URL/v1/nodes/$NODE_ID/resource" -o "$tmp"
expected=$(awk 'tolower($1)=="x-pmman-node-sha256:" {print $2}' "$headers" | tr -d '\\r' | tail -n 1)
resource_version=$(awk 'tolower($1)=="x-pmman-node-version:" {print $2}' "$headers" | tr -d '\\r' | tail -n 1)
actual=$(sha256sum "$tmp" | awk '{print $1}')
[[ -n "$expected" && -n "$resource_version" && "$actual" == "$expected" ]] || { echo '节点资源校验失败' >&2; exit 1; }
install -m 0755 "$tmp" "$BINARY_PATH"
printf '%s\\n' "$resource_version" > "$VERSION_PATH"

cat > "$ENV_PATH" <<EOF
PMMAN_NODE_ID=$NODE_ID
PMMAN_NODE_NAME=$NODE_NAME
PMMAN_NODE_REGION=$NODE_REGION
PMMAN_NODE_TOKEN=$NODE_TOKEN
PMMAN_MASTER_URL=$MASTER_URL
PMMAN_PROBE_ENDPOINT=$PROBE_ENDPOINT
PMMAN_PROBE_LISTEN=$PROBE_LISTEN
PMMAN_WG_INTERFACE=$WG_INTERFACE
PMMAN_WG_ENDPOINT=$WG_ENDPOINT
PMMAN_WG_IPV4_ADDRESS=$WG_IPV4_ADDRESS
PMMAN_WG_IPV6_ADDRESS=$WG_IPV6_ADDRESS
PMMAN_WG_PRIVATE_KEY_FILE=$WG_PRIVATE_KEY_FILE
PMMAN_WG_CLI=/usr/bin/wg
PMMAN_IP_CLI=/usr/sbin/ip
  PMMAN_ROUTE_TABLE=$ROUTE_TABLE
  PMMAN_DNS_IPV4=$DNS_IPV4
  PMMAN_DNS_IPV6=$DNS_IPV6
  PMMAN_DNS_UPSTREAMS=$DNS_UPSTREAMS
  PMMAN_DNS_DOMESTIC_UPSTREAMS=$DNS_DOMESTIC_UPSTREAMS
  PMMAN_DNS_FOREIGN_UPSTREAMS=$DNS_FOREIGN_UPSTREAMS
  PMMAN_DNS_DEFAULT_GROUP=$DNS_DEFAULT_GROUP
  PMMAN_DNS_DOMESTIC_RULES_FILE=$DOMESTIC_RULES_FILE
  PMMAN_DNS_FOREIGN_RULES_FILE=$FOREIGN_RULES_FILE
  PMMAN_NODE_BINARY=$BINARY_PATH
  PMMAN_NODE_VERSION_FILE=$VERSION_PATH
  PMMAN_SERVICE_NAME=$SERVICE_NAME
EOF
chmod 600 "$ENV_PATH"
  cat > "/etc/systemd/system/$SERVICE_NAME.service" <<EOF
[Unit]
Description=Pmman Network node
After=network-online.target
Wants=network-online.target
[Service]
EnvironmentFile=$ENV_PATH
ExecStart=$BINARY_PATH
Restart=on-failure
RestartSec=3
[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
  systemctl enable --now "$SERVICE_NAME.service"
echo "Pmman Network 节点 $NODE_ID 已部署并启动"
`;
});
function download() {
  if (!result.value) return;
  const url = URL.createObjectURL(
    new Blob([configuration.value], { type: 'text/plain;charset=utf-8' }),
  );
  const link = document.createElement('a');
  link.href = url;
  link.download = `${result.value.node.nodeId}.env`;
  link.click();
  URL.revokeObjectURL(url);
  downloaded.value = true;
}
function downloadScript() {
  if (!result.value) return;
  const url = URL.createObjectURL(
    new Blob([deploymentScript.value], { type: 'text/x-shellscript;charset=utf-8' }),
  );
  const link = document.createElement('a');
  link.href = url;
  link.download = `${result.value.node.nodeId}-deploy.sh`;
  link.click();
  URL.revokeObjectURL(url);
  downloaded.value = true;
}
</script>
<template>
  <el-dialog
    v-model="visible"
    title="接入节点"
    width="720px"
    :close-on-click-modal="false"
    :close-on-press-escape="!saving"
    :show-close="!saving"
    destroy-on-close
  >
    <template v-if="!result">
      <el-alert title="生成节点接入脚本并启动节点服务。" type="info" :closable="false" show-icon />
      <el-form
        ref="formRef"
        :model="form"
        :rules="rules"
        label-position="top"
        @submit.prevent="create"
      >
        <el-tabs type="border-card">
          <el-tab-pane label="基础信息">
            <el-row :gutter="16"
              ><el-col :span="12"
                ><el-form-item label="节点 ID" prop="node_id"
                  ><el-input
                    v-model="form.node_id"
                    placeholder="例如 gz-01"
                    maxlength="64" /></el-form-item
              ></el-col>
              <el-col :span="12"
                ><el-form-item label="节点名称" prop="name"
                  ><el-input
                    v-model="form.name"
                    placeholder="例如 广州节点 01"
                    maxlength="128" /></el-form-item></el-col
            ></el-row>
            <el-row :gutter="16"
              ><el-col :span="12"
                ><el-form-item label="区域"
                  ><el-input
                    v-model="form.region"
                    placeholder="例如 Guangzhou"
                    maxlength="128" /></el-form-item
              ></el-col>
              <el-col :span="12"
                ><el-form-item label="返回路由表" prop="route_table"
                  ><el-input
                    v-model="form.route_table"
                    placeholder="例如 51830；留空不安装路由" /></el-form-item></el-col
            ></el-row>
            <el-form-item label="允许客户端连接">
              <el-switch v-model="form.connectable" />
              <div class="form-help">关闭后节点仅作为端口转发入口，不会出现在客户端连接列表。</div>
            </el-form-item>
            <el-form-item label="主控地址" prop="master_url"
              ><el-input v-model="form.master_url"
            /></el-form-item>
            <el-form-item label="UDP 探测公网地址" prop="probe_endpoint"
              ><el-input v-model="form.probe_endpoint" placeholder="node.example.com:51840"
            /></el-form-item>
            <el-form-item label="WireGuard 公网接入地址" prop="wireguard_endpoint"
              ><el-input v-model="form.wireguard_endpoint" placeholder="node.example.com:51820"
            /></el-form-item>
            <el-row :gutter="16"
              ><el-col :span="12"
                ><el-form-item label="WireGuard 接口名称" prop="interface"
                  ><el-input v-model="form.interface" />
                  <div class="form-help">
                    脚本会自动创建并管理此接口，不要求节点预先存在。
                  </div></el-form-item
                ></el-col
              >
              <el-col :span="12"
                ><el-form-item label="UDP 探测监听" prop="probe_listen"
                  ><el-input v-model="form.probe_listen" /></el-form-item></el-col
            ></el-row>
          </el-tab-pane>
          <el-tab-pane label="网络与 DNS">
            <el-row :gutter="16"
              ><el-col :span="12"
                ><el-form-item label="节点 IPv4 地址" prop="wireguard_ipv4"
                  ><el-input v-model="form.wireguard_ipv4" placeholder="例如 10.99.0.1/24" />
                  <div class="form-help">
                    必须填写节点实际接口地址；不会自动套用地址池默认值。
                  </div></el-form-item
                ></el-col
              >
              <el-col :span="12"
                ><el-form-item label="节点 IPv6 地址" prop="wireguard_ipv6"
                  ><el-input v-model="form.wireguard_ipv6" placeholder="例如 fd99:1000::1/64" />
                  <div class="form-help">
                    必须填写节点实际接口地址；不会自动套用地址池默认值。
                  </div></el-form-item
                ></el-col
              >
            </el-row>
            <el-row :gutter="16"
              ><el-col :span="12"
                ><el-form-item label="pmmandns IPv4 监听地址"
                  ><el-input
                    v-model="form.dns_ipv4"
                    placeholder="可选，例如 10.99.0.1" /></el-form-item
              ></el-col>
              <el-col :span="12"
                ><el-form-item label="pmmandns IPv6 监听地址"
                  ><el-input
                    v-model="form.dns_ipv6"
                    placeholder="可选，例如 fd99:1000::1" /></el-form-item
              ></el-col>
            </el-row>
            <el-form-item label="Geosite 分流">
              <el-switch v-model="form.pmmandns_geosite_enabled" />
              <div class="form-help">使用主控发布的 geosite 资源；请配置国内和国外两组上游。</div>
            </el-form-item>
            <el-alert
              title="pmmandns 国内外分流"
              description="使用主控发布的 geosite 资源。"
              type="info"
              :closable="false"
              show-icon
            />
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
                    :rows="5"
                    placeholder="domain:baidu.com&#10;domain:qq.com" /></el-form-item
              ></el-col>
              <el-col :span="12"
                ><el-form-item label="国外域名规则"
                  ><el-input
                    v-model="form.pmmandns_foreign_rules"
                    type="textarea"
                    :rows="5"
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
              title="境外分流由客户端内置 cnipgen 双栈规则生成，节点部署不再维护分流路由文件。"
              type="info"
              :closable="false"
              show-icon
            />
          </el-tab-pane>
        </el-tabs>
      </el-form>
    </template>
    <template v-else>
      <el-alert
        title="凭据仅在此弹窗展示。请下载并妥善保管配置；丢失后可取消待接入记录，再重新创建。"
        type="warning"
        :closable="false"
        show-icon
      />
      <el-descriptions :column="1" border class="node-details">
        <el-descriptions-item label="节点 ID">{{ result?.node.nodeId }}</el-descriptions-item>
        <el-descriptions-item label="接入状态"
          ><el-tag type="info">待节点注册</el-tag></el-descriptions-item
        >
        <el-descriptions-item label="专属凭据"
          ><el-input
            :model-value="result?.node_token"
            type="password"
            show-password
            readonly
            aria-label="节点专属凭据"
        /></el-descriptions-item>
      </el-descriptions>
      <el-steps direction="vertical" :active="downloaded ? 1 : 0">
        <el-step title="下载一键部署脚本" description="下载并校验节点资源，写入服务配置。" />
        <el-step title="在目标 Linux 节点执行" description="在目标节点以 root 执行脚本。" />
        <el-step title="刷新列表确认在线" description="刷新节点列表，确认首次心跳。" />
      </el-steps>
    </template>
    <template #footer>
      <el-button :disabled="saving" @click="visible = false">{{
        result ? '关闭' : '取消'
      }}</el-button>
      <el-button v-if="!result" type="primary" :loading="saving" @click="create"
        >创建接入配置</el-button
      >
      <template v-else
        ><el-button @click="download">下载环境配置</el-button
        ><el-button type="primary" @click="downloadScript">下载一键部署脚本</el-button></template
      >
    </template>
  </el-dialog>
</template>
