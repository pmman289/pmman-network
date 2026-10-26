# 端口转发模块设计

本文定义 Pmman Network 端口转发模块的正式设计约束。端口转发由主控分配资源和管理租约，由节点执行公网数据面转发，客户端只通过主控申请、查看和释放自己的转发。

当前实现使用 PostgreSQL 保存租约、管理 API、客户端 IPC/UI、节点心跳同步和 Linux nftables DNAT/SNAT；公网入口和租约由管理员显式创建，不内置测试数据。

## 入口回程与策略路由

节点为每个已识别的入口路径（地址族、监听地址、本机地址、接口及网关）持久分配标记和专属路由表。入口 DNAT 时只写入 conntrack mark 的项目保留位；回包在 conntrack 关联之后、路由查找之前，通过 `prerouting priority mangle` 将保留位恢复到 fwmark。恢复规则仅匹配 `ct direction reply` 与 `ct status dnat`，不标记原方向请求，也不改变普通客户端代理流量的源地址策略。SNAT 仍使用到客户端路径上的网关地址。

默认资源：优先级 `9000`，路由表 `52100`—`52163`，掩码 `0xffff0000`，标记从 `0x504d0000` 起每个槽递增 `0x10000`，低 16 位保留给现有防火墙/QoS。项目路由和规则标记协议 `242`；检查现有规则、表及持久化所有权，发生冲突拒绝覆盖。优先级应置于宿主机需要绕过的源策略之前，保留 `local` 规则。不能通过更晚的 mark 规则覆盖本项目保留位；节点不自动改写管理员其他防火墙链。

可在节点环境文件配置 `PMMAN_PORT_FORWARD_RULE_PRIORITY`、`PMMAN_PORT_FORWARD_TABLE_BASE`、`PMMAN_PORT_FORWARD_MARK_BASE`、`PMMAN_PORT_FORWARD_MARK_MASK`（十进制或 `0x` 十六进制；mask 为连续高位）。修改已有所有权资源的参数前，需先排空连接并清理原项目专属资源，不能直接重用可能冲突的旧标记。

路由表包含明确的 `default via <gateway> dev <ingress>`，以及高 metric 的 unreachable 默认路由。必须先安装回程表和规则才能启用 DNAT；接口下线时失败关闭，不回落到原有源策略。直接连接的公网地址按其实际接口匹配；云 EIP 自动识别仅在唯一出口且本机地址可确定时使用。多个出口、ECMP 或缺少匹配默认路由时拒绝猜测，可用 `PMMAN_PORT_FORWARD_INGRESS_ROUTES` 指定，例如：

```json
[{"listen_address":"203.0.113.10","local_address":"10.1.0.6","interface":"eth0","gateway":"10.1.0.1"}]
```

上例为部署示例，不是内置生产地址；IPv6 同样支持，点到点出口可将 gateway 设为 null。本机地址必须确实配置在指定接口上，不可指定客户端隧道为公网入口。自动探测只读取现有接口和 main 默认路由，不修改 BIRD、OSPF 或宿主机主路由表。

所有权日志位于 `PMMAN_NODE_VERSION_FILE` 后附 `.port-forward-routing.json`，跨节点进程重启保留槽位。删除映射先移除接收新连接的 DNAT 规则；已有 conntrack 连接仍保留回程。入口不再使用至少 120 秒，且 `/proc/net/nf_conntrack` 确认无该标记连接后，才精确删除项目规则和两条默认路由，不 flush 整表。无法读取 conntrack 时保守保留资源；总量上限 64 个入口路径槽，耗尽明确报错而不复用仍可能活跃的标记。

WSL 真实内核回归脚本 `scripts/verify-port-forward-routing.py` 使用一次性的网络命名空间，覆盖源策略冲突复现、双入口双栈 TCP/UDP、外部标记位保留、重复对账与重启、停止新映射后已建立连接保持、出口下线禁止错误回落。它不替代公网节点和真实 Windows 客户端验收。

## 一、整体架构

~~~text
客户端
  │ HTTPS：查询入口、申请/释放转发
  ▼
主控
  │ 用户授权、入口管理、端口分配、租约状态
  │ 通过节点心跳下发 desired forwards
  ▼
节点
  │ nftables / conntrack 执行 DNAT、SNAT
  ▼
客户端 WireGuard 地址:目标端口
~~~

用户 Bearer Token 只访问主控，不直接发送给节点。节点继续使用现有节点凭据轮询主控，节点不提供端口转发 HTTP 管理接口。

所有节点视为同一内网的路由节点。任意节点都必须具备到达任意在线客户端 WireGuard 地址的路由，因此入口节点直接把流量转发到目标客户端，不需要通过其他节点中转。

## 二、入口模型

“入口”是节点上的一个公网监听地址和独立端口池，不是一个节点记录。一个节点可以有多个入口，例如：

- 不同公网 IPv4；
- 不同公网 IPv6；
- 不同运营商线路；
- 不同地区或用途；
- 不同 TCP/UDP 能力或端口范围。

入口示例：

~~~json
{
  "entry_id": "cn-gz02-v4-main",
  "node_id": "cn-gz02",
  "name": "广州主入口 IPv4",
  "public_address": "203.0.113.10",
  "hostname": "gz.example.com",
  "family": "ipv4",
  "protocols": ["tcp", "udp"],
  "port_ranges": [
    { "start": 20000, "end": 20999 },
    { "start": 30000, "end": 30999 }
  ],
  "reserved_ports": [20022, 20080],
  "enabled": true
}
~~~

入口的端口范围必须标准化、合并重叠区间，并排除：

- WireGuard 监听端口；
- UDP 探测端口；
- SSH、HTTPS、DNS 和其他系统服务端口；
- 管理员配置的保留端口；
- 节点当前已使用且不能被转发占用的端口。

入口地址必须是节点实际拥有并能监听的地址。入口启用前，主控和节点需要校验地址族、协议能力、端口范围、保留端口和监听权限。

## 三、TCP/UDP 共享端口和额度规则

端口资源的唯一键是：

~~~text
(entry_id, public_port)
~~~

协议不是资源唯一键的一部分。一个入口的同一个公网端口可以同时配置 TCP 和 UDP，且只占用一个用户活动转发额度：

~~~text
入口 cn-gz02-v4-main 的 20431/TCP + 20431/UDP = 1 个活动额度
~~~

因此：

- 只启用 TCP，占用 1 个额度；
- 只启用 UDP，占用 1 个额度；
- 同时启用 TCP 和 UDP，仍占用 1 个额度；
- 同一入口和同一公网端口不能被两个租约占用；
- TCP 和 UDP 不能被错误分配到同一入口的两个不同公网端口来表达同一个双协议转发；
- 同一个数字端口可以在不同入口同时使用，因为入口之间的端口池、冲突域和容量统计完全独立。

数据库必须使用 UNIQUE(entry_id, public_port)，不能使用 UNIQUE(entry_id, protocol, public_port)。租约保存一个协议集合，例如 ["tcp"]、["udp"] 或 ["tcp", "udp"]。

用户策略中的 max_active_forwards 统计租约数量，不统计协议数量。一个双协议租约不会消耗两个额度。

## 四、用户授权策略

每个用户增加端口转发策略：

~~~json
{
  "enabled": true,
  "max_active_forwards": 5,
  "allowed_entry_ids": [
    "cn-gz02-v4-main",
    "cn-gz02-v6-main"
  ],
  "allowed_protocols": ["tcp", "udp"],
  "allowed_target_ports": null,
  "allowed_families": ["ipv4", "ipv6"]
}
~~~

策略至少支持：

- 是否启用端口转发；
- 最大同时转发数量；
- 可使用的入口列表；
- 可使用的协议；
- 可选的目标端口白名单或范围；
- 是否允许 IPv4/IPv6 入口；
- 是否允许 UDP；
- 是否允许用户查看公网源地址。

allowed_entry_ids 为空表示没有入口权限，不表示允许全部入口。若需要全部入口，使用明确的全入口策略字段，避免空值语义混淆。

客户端不能自行指定公网端口，只能指定：

- TCP、UDP 或 TCP+UDP；
- 客户端本地目标端口；
- 入口，或者自动选择入口。

公网端口始终由主控随机分配。

## 五、转发租约模型

每个入口公网端口对应一个独立租约：

~~~json
{
  "forward_id": "pf_01J...",
  "user_id": "u1",
  "device_id": "device-uuid",
  "entry_id": "cn-gz02-v4-main",
  "node_id": "cn-gz02",
  "protocols": ["tcp", "udp"],
  "public_address": "203.0.113.10",
  "public_port": 20431,
  "target_address": "10.88.0.20",
  "target_port": 3389,
  "state": "active",
  "config_version": 3,
  "created_at": "...",
  "last_seen_at": "...",
  "expires_at": null
}
~~~

租约状态：

~~~text
pending     主控已锁定端口，等待节点应用
applying    已下发节点，等待节点确认
active      节点已确认协议规则生效
error       应用失败，端口待释放或重试
releasing   正在撤销节点规则
released    已释放端口资源
expired     租约超时后释放
~~~

target_address 必须是当前用户当前设备有效的 WireGuard 地址。用户不能指定其他用户的地址、任意内网地址或任意节点地址。

## 六、端口分配算法

主控必须在事务中完成授权校验、额度校验和端口锁定：

1. 检查用户已启用端口转发。
2. 检查活动租约数量小于 max_active_forwards。
3. 检查入口存在、已启用、所属节点健康，并支持请求的协议。
4. 检查目标设备属于当前用户且在线。
5. 检查目标端口在 1..65535 内，并符合用户目标端口策略。
6. 读取所选入口的端口范围和保留端口。
7. 排除系统服务端口、入口保留端口和已锁定的 (entry_id, public_port)。
8. 随机选择起始偏移，再线性扫描可用端口，避免端口集中在范围起点。
9. 在数据库事务中锁定端口并生成 forward_id。
10. 通过节点心跳下发完整规则。
11. 节点确认成功后将状态改为 active。
12. 节点失败或超时则撤销规则并释放端口。

不能由客户端或节点自行选择公网端口。

入口容量按入口独立计算：

~~~text
available(entry) =
  all_ports(entry)
  - reserved_ports(entry)
  - system_ports(entry)
  - allocated_ports(entry)
~~~

入口 A 耗尽时，只拒绝入口 A 的新分配；入口 B 仍可分配。入口编辑导致现有租约落在新范围之外时，不得静默删除租约，应拒绝配置变更或先要求管理员迁移/释放受影响租约。

## 七、节点执行方式

Linux 节点第一版使用 nftables/conntrack，不为每个端口启动一个 socat 进程。

~~~text
公网入口:20431/TCP ─┐
公网入口:20431/UDP ─┴─ DNAT ─→ 客户端 WireGuard 地址:3389
~~~

节点需要配置：

- DNAT 规则；
- 必要时的 SNAT/MASQUERADE；
- conntrack；
- WireGuard peer 路由；
- 转发防火墙规则。

第一版默认启用节点侧 SNAT/MASQUERADE，保证目标客户端回程路径稳定：

~~~text
公网客户端 → 入口节点 → WireGuard → 用户客户端
用户客户端 → WireGuard → 入口节点 → 公网客户端
~~~

后续可以增加“保留真实源地址”模式，但必须额外验证客户端默认路由、策略路由和 IPv4/IPv6 回程，不能把无 SNAT 作为第一版隐含前提。

节点通过心跳接收完整 desired state：

~~~json
{
  "port_forwards": [
    {
      "forward_id": "pf_01J...",
      "config_version": 3,
      "protocols": ["tcp", "udp"],
      "listen_address": "203.0.113.10",
      "listen_port": 20431,
      "target_address": "10.88.0.20",
      "target_port": 3389
    }
  ],
  "revoked_forward_ids": []
}
~~~

### Windows 客户端防火墙

节点完成 DNAT/SNAT 后，数据包仍会经过客户端 Windows Defender Firewall 的入站过滤。Windows 后台服务在隧道建立且租约属于当前设备时，按目标端口和协议创建项目专属规则：

- 规则组为 `Pmman Network Port Forward`；
- 只允许进入当前 WireGuard 接口（默认 `pmman-client`）；
- TCP 和 UDP 分别按目标端口创建，不放开其它接口或全部入站端口；
- 租约释放、隧道断开、服务端撤销和服务启动时清理旧规则；
- 配置失败通过 IPC 返回明确错误和手动放行范围；租约不会因本机防火墙配置失败而自动释放，用户修复防火墙后可继续使用。

因此节点防火墙和客户端 Windows 防火墙分别承担数据路径上的两段放行职责。生产验收必须同时检查节点 DNAT/SNAT、客户端项目规则和外部真实 TCP/UDP 流量。

节点返回每条规则的应用结果：

~~~json
{
  "applied_forwards": ["pf_01J..."],
  "errors": []
}
~~~

节点必须按 forward_id + config_version 幂等应用，删除不在 desired state 中的 nftables 规则，并在节点重启后由主控重新下发。

## 八、客户端功能

客户端新增“端口转发”页面。

创建字段：

- 协议：TCP、UDP 或 TCP+UDP；
- 客户端目标端口；
- 入口：指定入口或自动选择；
- 创建按钮。

成功后显示：

~~~text
公网地址：gz.example.com:20431
协议：TCP + UDP
转发到：10.88.0.20:3389
状态：已启用
~~~

客户端不显示不可用入口，并展示：

- 入口健康状态；
- 当前延迟；
- 剩余端口数量；
- 支持的协议；
- 地址族。

客户端断开内网时，相关租约进入 error 或 expired，节点删除转发规则。客户端重新连接后，优先恢复原租约；如果入口或端口不可恢复，主控才重新分配。

## 九、主控 API

客户端接口：

~~~text
GET    /v1/client/port-forwards/entries
GET    /v1/client/port-forwards
POST   /v1/client/port-forwards
DELETE /v1/client/port-forwards/{id}
~~~

创建请求：

~~~json
{
  "device_id": "device-uuid",
  "entry_id": "cn-gz02-v4-main",
  "protocols": ["tcp", "udp"],
  "target_port": 3389
}
~~~

返回至少包含：

~~~json
{
  "forward_id": "pf_01J...",
  "entry_id": "cn-gz02-v4-main",
  "public_host": "gz.example.com",
  "public_address": "203.0.113.10",
  "public_port": 20431,
  "protocols": ["tcp", "udp"],
  "target_address": "10.88.0.20",
  "target_port": 3389,
  "state": "active"
}
~~~

管理员接口：

~~~text
GET    /v1/admin/port-forward/entries
POST   /v1/admin/port-forward/entries
POST   /v1/admin/port-forward/entries/{id}
DELETE /v1/admin/port-forward/entries/{id}

GET    /v1/admin/port-forward/allocations
POST   /v1/admin/port-forward/allocations/{id}/revoke

GET    /v1/admin/users/{username}/port-forward-policy
POST   /v1/admin/users/{username}/port-forward-policy
~~~

## 十、持久化、并发和实时状态

推荐使用独立 SQLite + WAL：

~~~text
/opt/pmman-network-dev/data/port-forward.db
~~~

至少包含：

- forward_entries：入口；
- forward_entry_ranges：入口端口范围；
- forward_entry_reserved_ports：入口保留端口；
- user_forward_policies：用户策略；
- port_forward_allocations：租约和节点应用状态；
- port_forward_events：审计、重试和错误记录。

必须建立：

~~~sql
UNIQUE(entry_id, public_port)
~~~

同一数字端口在不同入口不冲突，因为入口 ID 属于唯一键的一部分。

管理后台建议使用：

~~~text
SSE GET /v1/admin/port-forward/events
~~~

事件包括租约创建、进入 applying、节点应用成功/失败、释放、过期和节点离线。SSE 断线时自动重连，并保留 5 秒轮询作为兜底。

主控或节点重启后，主控通过节点心跳重新下发活动租约。主控不能仅根据数据库记录宣称转发 active，必须等待节点确认。

## 十一、管理后台

增加三个标准中台页面：

### 1. 入口管理

展示入口名称、节点、公网地址、域名、协议、端口范围、总端口数、已分配数、剩余数、健康状态和最近心跳时间。

### 2. 端口分配

展示公网地址和端口、协议集合、用户、设备、目标地址和端口、节点、创建时间、最近心跳、当前状态和节点错误。

支持用户名、节点、入口、协议、状态筛选，以及强制释放。

### 3. 用户授权

在用户编辑中增加启用开关、最大并发转发数、允许入口、允许 TCP、允许 UDP、目标端口限制和地址族限制。

入口端口范围编辑使用可增删的重复表单项；保存前校验和提示重叠范围，主控进行最终校验。

## 十二、必须处理的边界

- 未授权用户不能创建转发；
- 达到最大并发数时拒绝；
- 没有允许入口时拒绝；
- 入口节点不健康时拒绝；
- 入口端口范围耗尽时返回明确错误；
- 节点应用失败必须释放端口；
- TCP+UDP 同端口只计一个额度；
- 不同入口相同数字端口可以同时使用；
- 节点必须能到达其他节点上的客户端地址；
- 节点重启后主控自动重新下发；
- 主控重启后租约可恢复；
- 用户断开客户端后清理规则；
- 管理员撤销授权后按明确策略立即释放或停止续期；
- 禁止用户访问其他客户端地址；
- 禁止用户指定任意内网目标；
- 入口删除前必须阻止存在活动租约，或提供迁移流程；
- 端口转发不能占用 WireGuard、UDP 探测、SSH、DNS、控制面等端口；
- 入口 A 的端口范围、保留端口和容量统计不能影响入口 B。

## 十三、第一版范围

第一版实现：

- Linux 节点；
- TCP、UDP 和同端口双协议；
- IPv4/IPv6 入口；
- nftables DNAT + SNAT；
- 单用户最大并发数量；
- 入口白名单；
- 随机公网端口；
- 主控 PostgreSQL 持久化租约；
- 节点心跳同步；
- 客户端创建、释放和查看；
- 管理后台入口、授权和分配查看。

暂不实现：

- 用户自定义公网端口；
- 真实源地址保留；
- 多主控；
- 七层 HTTP 转发；
- 端口转发流量计费；
- 复杂端口段批量映射。
