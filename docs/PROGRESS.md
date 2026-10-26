# Development Progress

## 端口映射入口回程标记，生产 0.3.47 — 2026-10-25

- 新增节点回程路由模块：映射首包保存入口 conntrack mark，仅在 DNAT reply 方向于路由查找前恢复 fwmark。默认保留低 16 位，使用高位 `0x504d0000/0xffff0000`、规则优先级 9000、独立表 52100 起；不标记原方向请求或普通代理流量。SNAT 继续使用通往客户端的节点网关地址，而非修改普通源策略。
- 每个入口绑定实际接口、地址和网关。云 EIP 自动识别只接受唯一明确出口，多出口不猜测，可通过 `PMMAN_PORT_FORWARD_INGRESS_ROUTES` 明确配置。标记/表/优先级冲突、路由安装失败时不启用对应 DNAT；先安装回程再启用映射，出口失效时使用 unreachable 防止回落到错误源策略。
- 项目 journal 持久化资源所有权，重启不重复添加规则、不覆盖或 flush 宿主机路由表。停止映射先撤销新连接规则，保留已有连接的回程；入口退休至少 120 秒且确认 conntrack 无引用才精确释放。无法读取 conntrack 时保守保留，最多 64 个入口路径。旧版 hash conntrack 标记不回填，升级后的新连接使用新规则。参数和运维边界见 `docs/PORT-FORWARDING.md`。
- 本地 WSL musl 主控 80 项、节点 24 项、协议 23 项通过；15 项专属数据库单元测试本轮未显式重跑。节点实核专项通过一次性 netns 脚本显式执行：23 项真实检查覆盖两个入口、双栈 TCP/UDP echo、旧故障复现、外部 mark 位保留、重复对账/进程重启、旧连接保留及出口失效。测试未触及 WSL 主网络，专属 namespace/子进程已清理。修正原节点单元夹具误用真实 nft，并为 nft 临时文件增加 UUID 避免并发测试串写。
- Admin 格式/lint/类型检查/生产构建及日期 3 项、pmmandns Go 测试通过，保留既有 dead-code/前端大 chunk 提示。最终镜像真实 Compose/PostgreSQL/HTTP 81 项通过，临时容器与卷已清理；发布脚本回归 4 项通过，包含独立 PostgreSQL schema 回滚。没有以单元夹具或 HTTP 健康代替实际转发验收。
- 已用标准发布脚本将生产主控从 `0.3.46` 更新为 `0.3.47`，公网版本、Compose 主控/PostgreSQL 健康、远程资源 SHA-256、后台/注册页及 35 个前端资源逐字节核对通过。发布前后 10 个账号、网络设置、客户端发布、邀请及转发授权摘要一致，7 张地址池关系表完整摘要一致。对冻结 PostgreSQL dump 单独比较三个节点的管理员配置均不变；旧 `0.3.46` 更新目标按既有资源替换规则清空，未向其他节点下发新目标。留存管理员凭据仍为 401，未重置密码。
- 此修复需要节点执行，已显式只更新广州 `tencent-gz-02` 至 `0.3.47`：运行进程 SHA-256 `d76178c0e13ae60acfa2c2882f2875aa8bf90fcbd96d82c2083d7591b822a3dd`，node.env 摘要保持不变，独立备份 `/opt/pmman-network-dev/release-backups/node-tencent-gz-02-20261025-154922-0.3.47`。上海、东京节点 PID/启动时间保持不变；没有修改 BIRD/OSPF、DNS、网站或普通源地址策略，未启动 Windows 客户端或构建客户端包。
- 生产规则确认 `1.14.226.49:50510` 使用 eth0/10.1.0.6，标记回包查询走表 52100、经 10.1.0.1 从 eth0 返回；未标记查询仍经表 236 的 wg-inetcrossyt。节点对账 applied=1/errors=0。公网实测仍受限：主控按既有安全逻辑撤销恢复设备的旧授权后，此用户尚未重连，节点已无该客户端 peer 与返回路由；shixcm 的新 TCP 探测失败，包头证实请求无法再到达客户端。未伪造用户注册、重建其 peer 或修改租约绕过授权，不能将该条用户映射记为公网端到端通过；需客户端重新连接后复验。
- 主控回滚备份 `/opt/pmman-network-dev/release-backups/20261025-154850-0.3.47-9dd40cc8`，postgres.sql 权限 0600、126586 bytes、SHA-256 `9e9ed81efc8be92156d444c9eebaed443ac97d02b6cce5fd989b489e0a3a73ad`。本地归档 `artifacts/server-releases/0.3.47/pmman-server-node-0.3.47.tar.gz`，27207796 bytes，SHA-256 `bbfcddbc0500137abd2577cf706b17d15abd407c1fb7162e9fa694849f7946eb`；验证证据保存于同一忽略目录。

## jincarrot 公网端口转发回程排查 — 2026-10-25

- 只读检查报告 `diag_9611e018-9d4e-4112-a614-bf96bf5af0c4`：客户端 `0.1.22-alpha`，上报时为 connected，当前主控设备为 active。广州节点对应 peer 的双栈 AllowedIPs、近期握手、表 52001 的 `/32` 和 `/128` 返回路由均存在；用户 IPv4 已从旧地址迁移为 `162.141.136.136`，当前映射也指向新地址，没有旧租约目标残留。
- 双栈 ICMP 探测无回复，但节点通过真实 socket 连接 `162.141.136.136:3000` 与 `[2a0f:1cc5:4700:beef::c]:3000` 均成功（约 31/25ms）。因此不能以 ping 不通断言隧道或应用不可达，也不能将这次 TCP 故障归因为客户端目标端口防火墙拒绝；ICMP 无回复的具体原因未从该报告确认。
- 从 shixcm 发起公网 `1.14.226.49:50510` TCP 探测超时。节点有该租约的 DNAT/SNAT 规则且对账 applied、无执行错误。短时有界 tcpdump 仅查看目标连接包头：请求经 eth0 到达 `10.1.0.6:50510`，转为 `172.20.60.97:<源端口> → 162.141.136.136:3000` 从 wgnetport 发出；客户端 SYN-ACK 已返回 wgnetport，却在反向 NAT 后以 `10.1.0.6:50510` 为源从 wg-inetcrossyt 发出，没有走回 eth0。
- 根因是现有 `from 162.141.136.136 lookup 236` 源策略在反向 NAT 完成前决定了映射回包路径，表 236 默认出口为 wg-inetcrossyt。普通代理业务沿该策略可用，但公网入口映射需要独立、按连接绑定的回程选路，不能依赖 SNAT 自动修正策略路由。
- 建议后续由节点为映射连接保存入口回程标记，并在回包路由查找前恢复 routing mark，通过项目专属规则/路由表保证回到对应公网入口；仅作用于端口映射，不更改普通代理流量的源策略，也不全局关闭防火墙。该轮用户要求排查，未修改源码实现、生产 nftables/策略路由、BIRD、节点配置或客户端，未重启服务；tcpdump 随 timeout 退出，未保存用户应用载荷。

## Windows 防火墙同步失败不再误报连接失败 — 2026-10-25

- 根因是隧道已完成握手，后续端口映射防火墙同步失败却返回 `ok=false`；Tauri 因而丢弃真实成功快照，前端暂时显示失败，直到下一次状态读取恢复。连接、查询、创建及释放映射的后置防火墙同步统一返回已完成的业务结果，不再覆盖连接状态或成功文案，不释放成功建立的映射；真实注册、握手或主控映射操作错误继续报错。
- typed IPC 新增兼容的 `portForwardFirewallWarning`。界面单独展示浅/深色警告和可展开详情，不把 Windows 错误号或 HTTP 字样归纳为连接失败；普通 Status 保留警告，后续规则同步成功清除，退出账户时重置。失败及恢复写入诊断日志，未扩大防火墙放行范围、关闭防火墙或修改权限。
- Windows MSVC service 94 项、protocol 23 项测试通过；包含握手后防火墙失败仍 connected、映射操作成功不被覆盖、状态读取保留警告与同步恢复清除。service 静态 CRT release、Tauri `cargo check`、客户端类型检查与生产构建通过，保留既有 RouteManager 未使用方法提示。
- 按 Playwright 技能验证连接首次返回即成功且有独立警告、详情、深浅主题、5 秒刷新后状态稳定、创建/释放映射成功及同步恢复清除。独立回归脚本 `scripts/ui/verify-firewall-warning.js`，截图 `output/playwright/firewall-warning-*.png`。首次脚本末尾导航名称写为“连接”导致超时，改成页面实际名称“网络连接”后完整重跑通过，非产品故障。
- 本轮仅源码修复与构建/浏览器回归，未打包新版本、未部署生产、未启动 Windows 待测客户端或操作宿主机防火墙；浏览器夹具不代表真实隧道/防火墙验收。具体用户机器上的权限或策略拒绝原因仍需其诊断日志确认；本修复在客户端升级后生效，无需更新主控或节点。

## 客户端地址切换清理等待 — 2026-10-25

- 诊断报告 `diag_543ec594-4da9-4d31-890e-5fdb78c6be30` 确认首次注册遇到清理 409，随后重试成功；并非持续隧道故障。本次没有修改该用户、生产地址池或节点状态。
- Windows 服务仅对已知地址切换/旧节点清理冲突自动重试，保持本次公钥及配置，间隔 2 秒、注册预算 90 秒，HTTP 请求取 12 秒与剩余预算的较小值。不重试真实地址占用、其他活动连接、认证或网络错误，不绕过清理确认。
- typed IPC 增加等待标记及取消请求；前端轮询连接阶段的共享进度，显示简短等待文案并提供取消。完成后丢弃迟到的进度读取。取消在命令锁之外处理，但仅限清理等待阶段；迟到的成功注册先撤销，不安装隧道。取消成功返回 disconnected/ok，避免 Tauri 再转换为失败。注册成功前不启动 P2P worker。
- Windows MSVC 服务 91 项、协议 23 项测试通过，含真实 loopback HTTP 409→200、请求内容稳定、错误分类、超时、取消、迟到成功撤销、取消 IPC 成功封装和 IPC 命令锁测试。服务静态 CRT release 构建、Tauri `cargo check`、Vue 类型检查与生产构建通过。现有 RouteManager 未使用方法警告保留。
- Playwright 验证等待不报错、深浅主题、键盘取消、等待后成功、完成状态稳定、超时与按钮恢复；夹具和可重复脚本位于 `scripts/ui/cleanup-retry-fixture.js`、`scripts/ui/verify-cleanup-retry.js`，截图位于忽略目录 `output/playwright/cleanup-*.png`。浏览器验收使用构建后 preview；旧通用夹具缺少更新字段且开发服务器页面加载超时，改用本次独立完整 IPC 夹具验证，未将夹具问题当成产品网络故障。
- 本轮仅实现并验证源码，未生成新版本安装/升级包、未部署生产、未启动 Windows 待测应用或进行新的真实 WireGuard 流量验收；客户端升级后才生效，主控/节点不需要为此更新。

Updated: 2026-10-25

## 地址池网段自动加入内网路由，生产 0.3.46 — 2026-10-25

- 所有地址池的 IPv4/IPv6 原始网段自动与手工内网 CIDR 合并去重后下发，包括非主池和离散 `/32`、`/128`；分配排除项不从网段路由中扣除。内网/分流/全局模式复用同一来源，覆盖初次连接、节点连接确认响应和配置查询。
- 自动路由只从已提交的池配置派生，不回写手工网络文档。增删改池自动反映到后续连接和配置查询；同名手工路由仍保留。连接分配已持有池写锁时直接使用当前快照，不重复获取锁；节点安装确认后使用本次注册的路由快照，避免预校验的旧快照覆盖注册结果。现有连接不强制重建，客户端需重连一次安装新路由，无须升级客户端或节点。
- 后台网络配置分开展示只读“地址池路由（自动）”和“附加内网 CIDR”，地址池列表改为“自动下发”，不再提示管理员手动补齐池网段。Admin 格式/lint/类型检查/生产构建及日期测试 3 项通过；未进行新的浏览器交互验收。
- 主控 musl release 全量 80 项、节点 19 项通过，含新增路由 CRUD/去重/手工保护、注册与配置刷新、三种接入模式回归；15 项专用数据库单元测试本轮未重跑。最终镜像在 WSL 的真实 Compose/PostgreSQL/HTTP 验收 81 项通过，新增 6 项验证重启后路由合并、非主池主机前缀、编辑替换、删除清理及手工数据不变。legacy 注册仅作为无内核接口的协议夹具，不冒充实际 WireGuard 流量；临时容器和卷已清理。
- 使用本地静态 musl 程序和镜像，通过标准脚本将生产主控从 `0.3.45` 发布为 `0.3.46`。公网版本、主控/数据库健康、资源哈希及全部 35 个前端资源验证通过。发布前后 24 项稳定数据一致，7 张地址池关系表的完整摘要一致；保留 10 个账号、12 个未释放地址、手工内网 CIDR、权限、邀请和客户端版本。没有为了自动路由更改任何生产网段或地址策略。
- 三个节点心跳均为 HTTP 200，三地节点的 PID/启动时间均未改变；未下发节点更新，未操作客户端，未修改 BIRD/OSPF、WireGuard、DNS、宿主机路由或网站。留存管理员凭据仍为 401，未重置密码；没有执行登录后的生产池写操作或 Windows 实际流量验收。
- 回滚备份 `/opt/pmman-network-dev/release-backups/20261024-183940-0.3.46-f71026ca`，PostgreSQL dump 111319 bytes、权限 0600，SHA-256 `2ee3e478d535e19fe4be7ab07fa971bd0c9ccf3c0181eb234a4770b566fdb418`。发布归档 `artifacts/server-releases/0.3.46/pmman-server-node-0.3.46.tar.gz`，27070794 bytes，SHA-256 `e5adc517b94c2f3b5783ad93308c6aba521f3f7a479f773866e6846cd4feff1f`；发布及比对证据保存在同一忽略目录。

## 生产主控发布 0.3.45：多地址池 — 2026-10-25

- 已通过本地 WSL 构建和标准发布脚本，将 `ssh txgz` 的生产主控从 `0.3.44` 升级为 `0.3.45`。公网版本、HTTP 健康、Compose 主控/PostgreSQL 健康及远程资源 SHA-256 均通过；主控仍以非 root 容器运行。
- schema 5 已应用。旧双栈池迁移为一个“主地址池”，保留两条原 CIDR 和两条原网关排除；8 个自动账号跟随主池，2 个固定账号转换为分族固定/手动策略。逐项核对 12 个未释放地址的所有权，无重新编号、无重复地址，未释放地址唯一索引存在。
- 发布前后 23 项稳定数据对比通过：10 个账号完整文档（含密码、权限与地址）、网络设置、3 个节点的管理员配置与更新目标、设备历史活动时间、转发入口及授权、邀请策略/邀请码/注册记录、客户端版本记录均保持一致。内网 CIDR 没有被地址池迁移覆盖，可信代理配置保持不变。
- 三个节点均恢复心跳 HTTP 200；广州、上海、东京节点的 PID 和启动时间逐一比较未改变。本次仅更新主控及资源库，没有向节点下发更新，也没有修改 WireGuard、BIRD/OSPF、DNS、宿主机路由或客户端发布版本。
- 公网 `/admin/`、`/register` 和全部 35 个 JS/CSS 资源逐一匹配本地构建，包括地址池页及用户地址配置所在的延迟加载资源；没有 HTML fallback。地址池/主池/候选查询匿名请求被拒绝。候选查询首次检查漏传必填 family 而返回 400，补全合法查询后返回 403；未为迎合检查放宽服务端校验。
- 发布复验：Admin 格式/lint/类型检查/生产构建、日期测试 3 项、pmmandns Go 测试、主控 musl 全量 78 项和节点 19 项通过。15 项专用 PostgreSQL 测试在此次全量命令中仍为 ignored，地址池 5 项此前已显式通过，不冒称再次执行。最终 `0.3.45` 镜像的 Compose 75 项检查通过，专属测试容器和卷已清理；保留既有 dead-code、前端大 chunk 提示。
- 发布保护补齐：旧主控在数据库备份前停止接收写入；备份额外保存原 public 表清单，失败回滚先移除本次新增表，再恢复旧 dump，避免只恢复旧表却留下 schema 5。新增 4 项发布脚本测试通过，其中真实 WSL Docker/PostgreSQL 测试执行本次 schema 5 后回滚至旧 schema 和原记录，专属容器已清理。未在生产触发破坏性回滚测试。
- 回滚备份 `/opt/pmman-network-dev/release-backups/20261024-182659-0.3.45-c93eb533`，含 PostgreSQL dump、原表清单、旧程序和 Compose/环境配置。dump 为 98243 bytes、权限 0600，SHA-256 `c136f6da430b853486423bb560378cb9ced0a8450d7f5721b07a710521ab2579`。本地发布归档 `artifacts/server-releases/0.3.45/pmman-server-node-0.3.45.tar.gz`，27073388 bytes，SHA-256 `dab57dbd7cb4700112532ac7c14b8fcf072a67d60c0b72e869d5381a0bff24d8`。
- 验证限制：留存的生产管理员凭据仍返回 401，未重置账号，因此没有执行登录后的生产地址池写操作；真实后台交互采用上节独立 PostgreSQL 验收结果。本轮未启动客户端、未改动真实用户地址，未执行多池实际 WireGuard/P2P/端口转发流量测试，不以节点心跳和静态资源检查替代这些验收。部署及对比证据保存于忽略目录 `artifacts/server-releases/0.3.45/`。

## 多地址池与双栈独立分配 — 2026-10-25

- 新增命名多段地址池、保留 CIDR、IPv4/IPv6 独立主池和用户分族策略。支持自动、明确选址的池内固定、池外手动地址；新账号默认跟随主池。不再按相同偏移配对两族，不跨池兜底，单族失败不提交另一族。
- 按原始 CIDR 计算可用主机后再合并区间：IPv4 `/31`、`/32` 与 IPv6 `/128` 可分配全部主机；普通段排除网络/广播或子网地址。排除特殊用途、节点网关、显式保留与全局占用；大量离散主机段不枚举大子网。容量以十进制字符串传输。
- PostgreSQL schema 5 保存地址池、CIDR、主池、分族策略、地址租约与转发清理确认。事务锁、配置版本和未释放地址唯一索引保护写入；提交成功后才更新内存。一次性迁移保留原地址及旧网关排除项；已有关系表但主配置缺失时拒绝启动，不重建默认池。
- 连接保持当前地址至下次重连。变更地址前确认旧节点 peer/返回路由清理，并让相关入口节点明确确认旧转发规则删除；历史 expired/released 不能替代实际规则删除确认。账号删除也沿用此隔离；未变化地址族的映射不随另一族换址被撤销。离线节点保持待清理，不能提前复用地址。
- 后台新增“资源管理 → 地址池”，内网 CIDR 改为独立设置；用户地址窗口独立编辑两族，支持具体候选选择、精确查询与游标翻页。概览改为池数量及各族资源，不再展示误导性的成对容量。旧单池/成对写入接口拒绝 409，不能覆盖新配置。
- Admin 格式、lint、类型检查、生产构建及日期回归 3 项通过；协议 musl 23 项通过。真实 WSL Docker Compose 隔离验收 75 项通过，覆盖空库迁移、配置持久化、容器重建、数据库重启/中断、拒绝默认数据回退，以及旧 API 防覆盖；对应临时容器和卷已清理。
- 主控 musl 全量 77 项、节点 19 项通过，15 项数据库测试默认 ignored；本轮显式执行其中地址池 PostgreSQL 专项 5 项全部通过，覆盖固定地址抢占唯一获胜、并发自动分配无重复、第二族失败整体回滚、持久化/故障回滚以及删除账号的转发清理确认。其他历史数据库专项未重跑。最终复查补齐释放完成后的缓存占用刷新、地址池收缩后范围外隔离地址的统计，地址池常规 15 项回归已通过；全量 77 项是在最后这两处收尾之前执行，不将专项复验记作再次全量测试。
- 本轮使用独立 PostgreSQL 和真实后台页面进行 Playwright 验证：新增离散地址池、具体 IPv4 选址、精确查找 `/32` 末地址、两族独立模式、池外 IPv6 保存、主控重启保留配置、跟随主池的显示、无效 IP 的服务端错误和重试，以及 390px 表单/键盘 Escape。截图 `output/playwright/address-pool-exact-selection.png`、`output/playwright/address-pool-error-mobile.png`；浏览器和测试主控已关闭。预期的初始密码 403、重启后旧会话 401、无效地址 400 不算成功请求，也未发现新增页面脚本异常。
- 最终代码复验：地址池常规 15 项、显式 PostgreSQL 5 项均再次通过；主控静态 musl release、scoped rustfmt 和 diff 检查通过。最终构建镜像再次执行 Compose 75 项检查全部通过。专属 `pmman-address-pool-test-20261025` PostgreSQL 容器及其匿名卷已删除，未涉及生产数据；保留本地测试镜像与浏览器截图。既有 Rust dead-code、Element Plus 大 chunk 和 npm 本地配置提示未在本轮处理。
- 说明与迁移/回滚步骤见 `docs/ADDRESS-POOLS.md`。本地构建主控 `0.3.45`，尚未部署生产或创建正式发布归档；生产仍为 `0.3.44`。无需为本功能升级客户端或节点，未修改生产 WireGuard、BIRD、OSPF、DNS，未启动宿主机客户端。真实公网多池 WireGuard/P2P/端口转发流量验收、生产数据迁移对比与回滚演练尚未执行；现有后台未提供深色主题，未把浅色检查计为深浅主题验收。

## 最后活动时间修复与生产主控 0.3.44 — 2026-10-25

- 根因是主控从 PostgreSQL 恢复设备时，为撤销旧连接同时将 `last_seen_at` 清零；后续整份设备快照保存会将零时间写回。后台直接格式化后在 UTC+8 显示为 1970/1/1 08:00:00，并非客户端时钟问题。
- 恢复流程现在仅撤销连接授权，保留活动时间、地址、流量及 released 历史状态，不允许迟到心跳复活旧连接。设备列表、详情和概览使用独立活动时间格式化方法，零值、缺失或无效时间显示“未记录”；其他日期字段无效时显示破折号，不再暴露纪元时间或 Invalid Date。未通过填入当前时间伪造历史活动。
- 本地 WSL 回归：主控 musl release 常规测试 63 项通过（10 项 PostgreSQL 测试默认 ignored）；活动时间专项 2 项通过，其中新增真实 PostgreSQL 专项显式执行，验证 active/revoked/released/历史零值跨三轮恢复、保存和再加载不变，旧心跳拒绝及新授权后的心跳持久化。其余 PostgreSQL 专项本轮未重跑。节点 19 项和 pmmandns Go 测试通过；新增 Admin 日期测试 3 项、格式/lint/类型检查/生产构建均通过。
- 按 Playwright 技能使用独立本地数据验证设备列表、详情和概览，真实日期正确显示、零值显示“未记录”。截图 `output/playwright/activity-history-devices.png` 和 `output/playwright/activity-history-overview.png`。测试不访问生产账号，浏览器、预览进程、专属 PostgreSQL 容器和临时卷已清理；不将浏览器夹具描述为生产鉴权验收。
- 生产主控已从 `0.3.43` 升级为 `0.3.44`，公网版本/健康检查、Compose/PostgreSQL 健康检查和远端产物哈希通过；三节点心跳均为 HTTP 200。发布前后 10 个用户、网络、节点配置/更新目标、邀请记录、转发授权和客户端版本数据哈希一致，6 条设备的活动时间哈希也一致（原有 5 条零值仍保留为未知，未擅自回填历史）。未更新或重启节点、未修改客户端发布版本、未操作宿主机客户端。
- 公网后台/注册页及所引用资源、设备页、概览和日期处理的延迟加载模块内容均与本地构建哈希一致。旧的生产管理员保存凭据仍为 401，未重置密码，未执行登录后的生产管理操作。数据库 schema 保持版本 4，无破坏性迁移。
- 回滚备份 `/opt/pmman-network-dev/release-backups/20261024-165417-0.3.44-b1db7a34`，包含 PostgreSQL dump、旧 Compose/环境及产物。归档 `artifacts/server-releases/0.3.44/pmman-server-node-0.3.44.tar.gz`，26519241 bytes，SHA-256 `501ca3fdd97a09b67841669695777a01e70b11b9ce1a5375fc185cfa7027bfd8`。发布前后快照和资源校验证据在同版本忽略产物目录。保留既有 Rust dead-code、Element Plus chunk 及本地 npm 配置提示。

## 客户端升级包 0.1.22-alpha — 2026-10-25

- 从已发布的 `0.1.21-alpha` 递增至 `0.1.22-alpha`，同步 package、service/Tauri Cargo 清单及锁文件、全量/更新 Tauri 配置。通过本地 Visual Studio MSVC 工具链重新构建 service 和桌面程序，打包时的 sidecar 与本次 service release 产物 SHA-256 一致，不复用上版后台服务。
- 包含前述管道并发/超时隔离、共享认证恢复、断开清理和状态读取修复。Windows service 单元测试 82 项全部通过；客户端 TypeScript 检查、前端生产构建、MSVC release 和 NSIS 打包通过，保留既有 `route_manager::install` dead-code 提示。
- 产物 `artifacts/client-updates/Pmman-Network-0.1.22-alpha-x64-update.exe`，3901175 bytes（约 3.72 MiB），SHA-256 `DE10AF4859F37D7D71C4A4DAD3DD38F0E48407FFC892583EB1A5441509665D1E`。安装包和桌面 EXE 产品名/版本均验证为 `Pmman Network / 0.1.22-alpha`，安装包与 NSIS 原始输出哈希相同；桌面程序保留 requireAdministrator 和 Common Controls v6 清单，未检出 localhost:1420 开发地址。
- 这是不含 WebView2、WireGuard 安装依赖的轻量升级包，供已有安装使用；更新沿用当前安装目录和静默安装流程。用户可用发布说明保存在同目录 `Pmman-Network-0.1.22-alpha-release-notes.txt`。
- 本轮仅构建交付，未创建主控客户端版本或上传包体、未安装或启动客户端，也未执行安装/自动升级/真实连接的原生验收；单元和静态检查不替代这些验收。生产主控保持 `0.3.43`，公网 stable 客户端仍为 `0.1.21-alpha`。

## 生产主控发布 0.3.43 — 2026-10-25

- 已在本地 WSL 构建并通过 `publish-server-release-txgz.ps1` 将生产主控从 `0.3.42` 升级为 `0.3.43`，包含管理员删除账号、资源回收隔离、账号操作竞态及登录失败分类日志。公网 `/v1/version` 返回新版本；主控和 PostgreSQL 均为 healthy，发布清单和远端资源哈希校验通过。
- PostgreSQL migration 4 已应用，历史邀请注册的用户名唯一约束已移除、普通查询索引存在；保留原有注册记录和身份 UUID 约束。发布前后 10 个用户（含密码、初始密码标记）、网络配置、3 个节点的管理员配置和更新目标、转发入口与授权、邀请策略/邀请码/注册记录及客户端版本记录哈希一致。未删除真实账号，未重置密码，未发布客户端版本。
- 三个节点 `tencent-gz-02`、`nbd-sha-01`、`nbd-jp-tyo-01` 在切换后均恢复心跳 HTTP 200；广州节点进程 PID 和启动时间不变。仅更新主控发布资源，未触发运行中节点更新或重启，未修改 WireGuard、BIRD、DNS 和宿主机网络。可信代理配置 `172.19.0.1` 保留。
- 公网 `/admin/`、`/register` 及其引用资源均返回 200，内容逐一匹配本地构建哈希；额外校验用户、设备、邀请码和诊断管理的延迟加载页面资源，未出现 HTML fallback。管理接口匿名请求被拒绝。本地保存的生产管理员凭据仍返回 401，未重置账号，因此未进行登录后的生产删除操作；账号删除事务和邀请登录的验证沿用前述隔离 PostgreSQL 回归，不以静态资源检查替代业务验收。
- 本次发布复验：Admin 格式/lint/类型检查/生产构建、pmmandns Go 测试、主控 musl release 测试 62 项及节点 19 项通过。9 项 PostgreSQL 测试仍默认 ignored，其中删除的 6 项及新增密码专项此前已显式通过，本次未重跑；保留既有 Rust dead-code 和 Element Plus chunk 提示。修正发布脚本，使后台前端在本地 WSL 构建，避免 Windows 使用 Linux 依赖目录失败。
- 回滚备份 `/opt/pmman-network-dev/release-backups/20261024-163103-0.3.43-b1317787`，包含 PostgreSQL dump、旧二进制、Compose 和环境配置。发布归档 `artifacts/server-releases/0.3.43/pmman-server-node-0.3.43.tar.gz`，26524238 bytes，SHA-256 `0677befab033d0c66eea71bf8249c29fcb4b5bb7326b4f0d4f4f9d08cd46c3f9`；镜像 ID `sha256:491b4b9e31916f23141dea67511c251e92fcab722ad728cc3e4f14a60faaf160`。发布前后证据与公网资源校验保存在同版本的忽略产物目录。
- Windows 管道和会话恢复修复不随主控发布安装至客户端；本轮未构建或安装新的客户端包，未操作宿主机正在运行的客户端。

## 管理员删除账号 — 2026-10-24

- 用户管理新增删除操作，要求输入区分大小写的完整用户名；不可删除当前登录账号或最后一个可用管理员。提交时禁用重复操作，失败保留弹窗及服务端错误，成功后刷新真实列表。
- 新增管理员 `DELETE /v1/admin/users/{username}`，成功返回 204。账号、设备撤销、转发待释放、授权删除、可用邀请码撤销和审计写入同一 PostgreSQL 事务；失败不发布内存状态。保留邀请注册、设备和转发历史，删除已注册邀请人不连带删除受邀者。
- 同名重新注册使用新 UUID；Migration 4 将邀请历史的用户名唯一约束改为普通查询索引，保留 `user_id` 唯一约束。会话签发在账号锁内复核身份与密码版本，避免验证完成后删除/重建账号的竞态；P2P 发布在异步解析之后再次检查身份和设备，防止撤销后重新公布目录。
- 删除后地址先隔离，等待所属节点确认 peer/路由清理、所有关联转发确认释放，再将历史设备标记为 released 并回收地址；主控重启保留此标记。离线或清理失败的节点不触发地址提前复用，端口同样等待节点确认后才可再分配。
- 第二轮复审补齐停用节点的 devices → nodes 锁顺序，避免与创建转发、安装状态查询互锁；迟到连接回滚只允许匹配代次的 active → revoked，不能覆盖已释放历史。跨节点重连在覆盖旧代次前确认旧节点清理；旧式主控网关连接提供提交后撤销及周期重试路径，不以删除数据库账号替代实际 peer 清理。
- Admin 格式、lint、类型检查、生产构建通过。Playwright 独立本地夹具验证当前账号禁删、完整用户名确认、pending 禁用、204 刷新、503 原文及重试恢复、390px 弹窗与 Escape 退出；截图 `output/playwright/account-delete-error.png`、`output/playwright/account-delete-mobile.png`。夹具不替代后端事务或节点实机验收；浏览器、预览和 SSH 转发已关闭。
- 删除专项最终 13 项通过（其中 6 项显式 PostgreSQL 集成测试），涵盖 7 处独立写入故障回滚、v3 历史迁移且重复运行 v4、公开邀请同名重注册、新旧身份权限隔离、地址隔离/确认/复用、迟到请求及 legacy 清理失败重试。专属 PostgreSQL schema、测试容器及匿名数据卷均已清理。旧网关测试使用注入回调和故意不存在的 CLI，不作为真实内核 WireGuard 验收。
- 全量调试回归发现旧的租约过期夹具把设备手动改为 node-a 后直接调用 legacy 注册，实际是一次未确认清理的跨网关切换，按新增安全校验应返回 409。已补上清理前拒绝及节点确认后成功的断言；定向复验通过。节点锁回归也已单独通过，没有放宽生产校验以迎合旧夹具。
- 最终主控 musl release 全量回归 62 项通过、0 失败，9 项 PostgreSQL 测试默认 ignored；其中删除的 6 项已在上述专项显式通过，邀请码新增密码回归另行通过，未重跑其余 2 项旧邀请专项。最终静态 musl release 编译、Cargo 格式检查及 scoped `git diff --check` 通过；保留既有 `node_device_config_with_routes` dead-code 提示。
- 未删除任何真实账号、未部署生产、未更新节点或发布客户端包。客户端 IPC 修复仅完成前述单元/编译验证，未操作宿主机正在运行的客户端，也未将服务 release 单文件当成已更新的安装包。

## 邀请注册后登录 401 的回归调查 — 2026-10-24

- 生产只读记录确认存在注册返回 201 后登录返回 401；未记录请求密码或完整邀请码，相关账号后来已被管理员重置，无法由现有日志还原原始输入。未擅自重置任何生产账号，也未将此问题归因于未经证实的密码算法缺陷。
- 新增真实 PostgreSQL 回归 `postgres_invitation_registration_passwords_survive_login_and_reload`，1 项通过（172.87 秒）：覆盖普通密码、前后空格、Unicode、256 字节密码，注册后立即登录及数据库重载后登录；验证用户名大小写、UUID 标识、后续认证及错误密码拒绝。此轮只显式运行新增专项，不冒称重新执行全部 PostgreSQL 专项。
- 使用与生产完全一致的 `0.3.42` 镜像（digest `sha256:f8303646f57fe19fdb13cf51d797f1c6433dce4a33536302fa634cc7dae4932e`），隔离容器中 5 个新账号的 36 项 HTTP 检查通过。实际重启后旧 token 按设计失效，但所有账号均能用原密码重新登录，无需重置密码。临时容器与专属卷已清理。
- 结论：目前未复现“注册的密码不可登录”，根因仍未确认。已确认的旧 bearer 401 恢复缺陷由客户端会话恢复修复覆盖，但不能据此宣称已解决初次密码登录失败。主控登录补充 account_missing、account_disabled、password_mismatch、account_changed 分类日志，仅记录合法用户名及原因，非法用户名统一代称，不输出密码、哈希、token 或请求正文。本地 Windows service release 编译通过，未部署或打包交付本轮改动。

## 管道繁忙、会话恢复与断开清理修复 — 2026-10-24

- 补充读取用户提供的 `D:/tmp/service.log` 和错误 231。与只读主控访问记录核对：23:05 主控重启后，心跳返回 401，被旧客户端当作设备撤销；23:06:32 清理 29693 条路由，WireGuard 服务停止及删除耗时 610 ms。23:09 连续日志上传均因旧会话 401 失败。此次不是日志中 10 月 18 日的 SCM 停止超时；不能从日志断言永久死锁或精确确定占住管道的请求。
- Windows 监听循环现在先创建同 ACL 的后继实例再分派当前连接，首实例独占名称并拒绝远程客户端。传输限制 16 个并发连接/工作任务、64 KiB 请求、总读取 3 秒及响应写入/flush 5 秒；解析和处理移入 blocking pool，单连接异常不终止监听。慢 handler 的 permit 保持到实际退出，避免超时后堆积后台线程。
- 变更命令使用独立 command gate；业务处理使用局部快照，不持状态锁做 HTTP、路由清理或 join。Status 绕过命令锁并非阻塞读取传输状态；连接/断开发布 pending 状态，并发变更明确拒绝。Tauri 启动 helper 的锁覆盖检查和 spawn；状态查询失联保留最后已知状态并明确标注，不冒称隧道已经断开。
- 心跳、P2P、诊断上报与设备撤销共用认证恢复：401 尝试使用本机 DPAPI 凭据续期并仅重试一次，续期结果共享；403/404/409 仍为设备撤销，网络/5xx 不触发错误拆隧道。凭据被明确拒绝或不存在可恢复凭据时结束连接并提示重新登录。续期受账号/代次约束，迟到的新 token 在本地拒绝提交后会撤销，不覆盖退出登录。
- 代理限速等待支持唤醒取消，大报文按额度分段扣费，不再因超过令牌桶容量永久等待。传输对象在容器锁外析构；主控撤销时先卸载接口再清理大规模路由。退出登录等待本次设备撤销后再注销 bearer；异步防火墙清理持生命周期锁并确认无新连接，避免删除重连后的规则。
- WireGuard 状态采样/peer 命令增加有界执行，子进程 stdout/stderr 同时排空并限制捕获大小，避免大分流表填满管道后等待退出形成阻塞。新增命令耗时、认证恢复、撤销和 route clear 开始/耗时日志，不记录密码、token 或 IPC 正文。
- 已通过 Windows MSVC service 单元测试 82 项，包括真正的独立临时 Named Pipe 慢请求并发接入（不使用正在运行的应用管道）、空/坏/超大/慢连接、限速取消、会话续期及退出竞态、旧防火墙清理隔离、大输出/超时子进程。协议 musl 23 项、节点 musl 19 项、客户端类型检查与生产前端构建、Tauri MSVC 编译检查通过；保留既有 route_manager::install dead-code 警告。
- 未关闭、重启、安装或连接宿主机正在运行的客户端；未部署生产主控、更新节点或发布升级包。本节单元验证不替代当前客户端的原生/公网验收；邀请码首次登录问题的真实 PostgreSQL 检查另行记录。

## 断开失败报告排查（diag_ff93cc95）— 2026-10-24

- 只读获取报告 `diag_ff93cc95-c97e-404f-bef5-c078d4332e89`，未操作正在运行的客户端或生产网络。报告摘要为 `0.1.21-alpha / connected`。
- 日志中唯一的 `disconnect_failed` 发生在旧版时期：北京时间 2026-10-18 05:27，WireGuard 服务停止超时（本地清理约 8310 ms），并有异步防火墙清理超时。不能将这条历史错误视为 10 月 24 日的新版本故障。
- 10 月 24 日 23:02:41 和 23:03:43 均记录 `disconnect_succeeded`；后一轮 SCM 从 running 到 stopped 再到 missing 共 304 ms，本地完整拆除 1781 ms，撤销随后成功。23:04:06 双栈分流连接完成，安装 29693 条路由；23:06:02 上传前没有新的 Disconnect 请求或断开失败记录。
- 初次上传报告不足以确认新故障；后续用户补充 `service.log` 及错误 231 后，已确认串行 IPC 和会话 401 处理缺陷并实施修复，见本页“管道繁忙、会话恢复与断开清理修复”。未将旧 SCM 超时当作本次故障，也未声称已在用户正在运行的客户端完成实机验收。

## 邀请功能不以修改初始密码为前置条件 — 2026-10-24

- 根因：邀请码模块在通用客户端认证后额外检查 `must_change_password`，而管理员创建/重置密码的账号通常保留此标记，导致已获邀请授权的用户仍收到 403。现已移除客户端邀请接口的额外改密限制；不修改用户密码、初始密码标记或邀请策略，也不增加改密引导。
- 仍校验账号启用状态、有效会话、邀请授权、配额、资源所有权和受邀权限；管理员接口继续遵循原有 `require_admin` 规则。无需修改客户端或升级包。
- 补充普通用户及管理员使用客户端接口的初始密码认证测试，并将 PostgreSQL 生命周期测试改为保留邀请人的初始密码标记，验证授权前拒绝、授权后查询/生成/兑换、配额及撤销边界。
- 验证通过：本地 WSL 主控 54 项、节点 19 项、邀请码专项 8 项（含 2 个真实 PostgreSQL 测试）；Admin 格式、lint、类型检查及生产构建通过。专项测试容器已清理；未启动或操作宿主机客户端。
- 已发布生产主控 `0.3.42`，公网版本/健康检查、Compose/PostgreSQL 健康检查、发布资源哈希校验通过；三个节点均恢复心跳 HTTP 200。发布前后用户（含密码和初始密码标记）、网络、节点配置、转发策略及客户端版本记录哈希完全一致，未修改节点或发布客户端。
- 回滚备份 `/opt/pmman-network-dev/release-backups/20261024-150447-0.3.42-977a8823`。发布归档 SHA-256 `9665fc750f08b1a5741b11c0b24c9d1d2ee353043aecd2f36c06671ce06c877c`。本地保存的生产管理员凭据仍为 401，未重置账号；鉴权业务通过隔离 PostgreSQL 验证，未宣称已登录生产客户端完成操作。

## 生产主控发布 0.3.41：邀请码注册与授权管理 — 2026-10-24

- 已在本地 WSL 构建并通过 `publish-server-release-txgz.ps1` 将生产主控从 `0.3.40` 原子升级为 `0.3.41`。生产镜像为 `pmman-network/master:0.3.41`，公网 `/v1/version` 返回新版本；主控和 PostgreSQL 均为 healthy，发布清单及远端资源哈希校验通过。
- PostgreSQL migration 3 已应用，邀请策略、邀请码、注册记录表均存在。发布前后比较确认 9 个用户、3 个节点、1 个端口转发入口数量不变，用户文档、网络配置、管理员维护的节点配置、转发授权及客户端版本记录哈希一致。未创建客户端发布版本，未更新或重启节点，未修改 BIRD、WireGuard、DNS 或其它网站。
- 公网 `/admin/` 及其 7 个引用资源、`/register` 及其 8 个引用资源均返回 200，资源未被 HTML fallback 替代。邀请码管理员接口匿名访问被拒绝。本地保存的生产管理员凭据返回 401，未重置密码；本轮未验证登录后的生产后台操作，真实注册及后台管理流程已在前述隔离 PostgreSQL/Compose 环境验收。
- 三个节点 `tencent-gz-02`、`nbd-sha-01`、`nbd-jp-tyo-01` 均在升级后持续获得心跳 HTTP 200；切换窗口内广州节点短暂收到 502 后自动重注册恢复。节点心跳仅更新运行时内存，不应以 PostgreSQL 节点文档的旧 `last_seen_at` 判断当前在线状态。
- 根据现有 OpenResty 覆写 `X-Real-IP`、后端端口仅绑定 loopback 的实际路径，显式配置可信代理地址 `172.19.0.1`；运行容器已加载。升级脚本保留并校验既有 `PMMAN_TRUSTED_PROXY_IPS`，未修改反向代理配置。配置修改前备份位于 `/opt/pmman-network-dev/backups/registration-proxy-20261024-144144.env`。
- 发布构建复验：Admin lint/类型检查/生产构建、pmmandns Go 测试、主控 musl 测试 53 项、节点测试 19 项、静态 release 与镜像导出通过；2 个默认 ignored 的 PostgreSQL 专项此前已显式通过。保留既有 Element Plus chunk 和 Rust dead-code 提示。
- 发布归档 `artifacts/server-releases/0.3.41/pmman-server-node-0.3.41.tar.gz`，26355990 bytes，SHA-256 `381fc134da6a65e0ede5383e30b07c3cdb74e116370ff10f53dad9fef95b39ac`。回滚备份 `/opt/pmman-network-dev/release-backups/20261024-144421-0.3.41-0edc76d9`，包含 PostgreSQL dump、原 Compose/环境配置、二进制和发布元数据。

## 邀请码注册与授权管理 — 2026-10-24

- 完成 PostgreSQL migration 3、用户邀请策略、单次/管理员无限码、公开注册接口及邀请记录。邀请码仅持久化摘要和前缀；注册在同一事务锁定用户文档、当前策略和邀请码，原子提交用户、端口转发策略、兑换计数、实际授予权限及审计记录，提交后才更新内存。
- 普通用户默认无邀请资格；默认限额为未使用 10、UTC 每日生成 20、有效期 7 天。管理员可配置独立的受邀双栈模式和完整端口转发策略。停用邀请人或关闭邀请资格立即阻止兑换；撤销不可恢复；降低额度不删除旧码。新用户固定普通角色、自动地址、无邀请资格，不强制首次改密。
- 客户端增加登录页注册和账户“邀请用户”窗口，全部通过 typed IPC。创建结果不写入常规状态缓存或本地持久化，邀请错误不改变隧道状态。后台增加邀请授权、邀请码管理及注册记录，服务端分页/筛选；长授权弹窗固定标题与操作区，表单区滚动。
- 新增独立 `/register` 构建入口，分享码位于 URL fragment，读取后移除，不使用后台认证拦截器。注册请求 8 KiB 上限，密码哈希前按 IP/全局限流，429 返回重试时间；默认不信任转发头，`PMMAN_TRUSTED_PROXY_IPS` 仅允许显式代理地址。失败/未知结果不自动重试，提示先尝试登录。说明见 [INVITATIONS.md](INVITATIONS.md)。
- 回归：WSL 协议 `23/23`；主控 `53/53`（另有 2 个 PostgreSQL 专项默认 ignored，已显式执行）；节点 `19/19`；邀请码专项 `7/7`，含两个真实 PostgreSQL 集成用例。验证 12 个独立注册表并发兑换单次码仅 1 人成功、无限码 8 个并发注册计数准确、用户名冲突不消耗码、故障注入后账号/授权/计数完整回滚、停用/恢复、配额/UTC 重置、失效入口拒绝且不消耗码、降低配额和重启恢复。
- Windows MSVC service `46/46` 通过；客户端 Vue 类型检查/生产构建、Tauri release 和 NSIS 升级包构建通过。Admin Prettier、ESLint（零 warning）、类型检查及生产构建通过；保留既有 Element Plus 大 chunk 提示和两个既有 Rust dead-code 提示。
- WSL 隔离 Docker Compose 两轮各 `76` 项通过（第二轮为包含最终页面修正的镜像），包括公开页及资源、完整转发授权落库、会话失效、数据库中断 fail-closed、重启和容器替换后数据一致、日志无密码/完整码。测试仅使用专属临时项目/卷，不访问生产库；测试容器、专属卷、临时 PostgreSQL、浏览器和 SSH 转发均已清理。
- Playwright 已完成真实网页注册、后台创建无限码/编辑权限/撤销、注册记录查询、邀请授权保存；客户端浏览器夹具覆盖注册返回登录、密码不一致、生成/撤销、空态、失败恢复按钮、Escape、浅深主题和 390px 布局。网络中断时网页仅提交一次并提示尝试登录。截图位于 `output/playwright/invitations-*.png`；浏览器夹具不作为 Windows 真实网络验收。
- 升级包：`artifacts/client-updates/Pmman-Network-0.1.21-alpha-x64-update.exe`，`3854059` bytes，SHA-256 `9543BA9AD431DAC2A031E3D471D7EA1C37A78A282896B0A1273E79E6782ACB4C`。客户端版本递增为 `0.1.21-alpha`，本地主控候选镜像 `pmman-network/master:invitation-test-final-0.3.41`，静态 musl 编译通过。
- 发布边界：本轮未部署生产主控、未创建生产客户端版本、未更新节点，也未运行 Windows 安装器或启动宿主机客户端。上线应先升级兼容主控，再由管理员发布客户端。未执行打包客户端的原生 UI/安装验收，不将浏览器测试描述为该项验收。

## 客户端执行 DNS 国内外分流 — 2026-10-18

- 主控连接配置为客户端提供节点 DNS 分流规则与当前 geosite 清单；配置了节点 DNS 的连接要求主控存在有效 geosite 资源，否则明确拒绝不完整的 DNS 分流。新增用户会话认证的 `/v1/client/geosite` 下载接口，节点原有资源接口和凭据边界保持不变。
- Windows service 在本机 forwarder 中分类：国内发往连接前捕获的系统 DNS，海外和未分类域名发往所选节点 DNS；海外查询失败不回退本机，避免错误结果竞争和 DNS 泄漏。WireGuard 只配置 `127.0.0.2`，系统不会并行查询多个解析器；连接建立后刷新 Windows DNS 缓存，避免沿用此前的污染记录。
- geosite 下载限制 32 MiB，校验大小、SHA-256 和分类格式后写入用户目录缓存；下载/校验/forwarder 启动失败会撤销本次连接注册。客户端详情同步显示新的 DNS 解析策略，诊断日志记录规则版本及上游数量。
- 验证：协议测试 19 项通过；主控完整测试 43/43 通过，DNS 配置回归及 geosite 下载会话认证测试通过；Windows MSVC service release 构建通过，service 单测 39/39 通过（跳过既有访问公网的代理集成用例）。本机实际国内/海外 A/AAAA 解析和 HTTPS 验收尚未执行。

## 客户端升级包 0.1.18-alpha — 待发布

- 客户端版本清单递增至 `0.1.18-alpha`。Windows 前端生产构建、MSVC service release 和 Tauri NSIS 更新包构建通过；没有启动安装包或关闭当前客户端。
- 产物 [Pmman-Network-0.1.18-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.1.18-alpha-x64-update.exe)，大小 `3298995` bytes，SHA-256 `82A5E97259DBD8532BBCE0E9F23C85E793EBBFA751119DB395A599317DFD3634`；文件产品名/版本为 `Pmman Network / 0.1.18-alpha`。
- 生产发布未完成：主控管理员登录返回 HTTP 401，旧本地凭据在上传前被拒绝。公网 stable 仍为 `0.1.17-alpha`，未创建或覆盖任何生产版本；需刷新本机发布凭据后重试。

## 内网 IPv6 回程默认路由与连接/断开耗时 — 2026-10-18

- 按产品选择恢复内网/分流模式的 IPv6 低优先级默认路由：WireGuard `AllowedIPs` 包含 `::/0`，Windows 侧安装 metric `5000` 的 `::/0`；系统已有的低 metric IPv6 默认路由应优先，缺少原生 IPv6 出口时其余 IPv6 流量会回落到隧道。
- 为兼容旧主控响应，客户端在 IPv6 内网模式过滤服务端遗留的 `::/0`、`::/1`、`8000::/1`，再由本地 route manager 按当前选择生成低优先级回退路由；IPv4 模式不受影响。Windows 按目的地址选择时保留物理 IPv6 默认路由；按分配 GUA 作为本地源地址查询时，Windows 选择 WireGuard `::/0`，从而使该源地址的回包经隧道，而普通本机 IPv6 流量仍走物理出口。
- 握手等待移除串行 IPv4/IPv6 网关 ping，改为 WireGuard keepalive 与握手轮询，等待上限从 25 秒收敛至 12 秒；WireGuard 安装/卸载命令、停止服务轮询及防火墙清理均增加有界超时或后台清理。断开先清本机接口与路由，主控租约撤销异步发送。
- 验证：WSL service `39/39` 通过。按用户要求在宿主机通过客户端 IPC 完成多轮真实连接/断开，没有使用虚拟机：连接约 `8.7–12.9s`，WireGuard 握手完成；Windows 实际安装 `pmman-client ::/0 metric 5000`，物理以太网默认路由 metric 为 `256`。普通源地址访问 `2606:4700:4700::1111:443` 成功并选择物理以太网；指定客户端分配 GUA `2a0f:1cc5:4700:beef::b` 查询同一目的地址时选择 `pmman-client`，绑定该 GUA 的 TCP/443 握手也成功，验证了公网响应经分配 GUA 回到客户端的双向路径。断开本地拆除耗时约 `0.34–0.46s`；断开后 WireGuard 服务和隧道 `::/0` 均已清理。
- 入站边界：txgz 到分配 GUA 的 IPv6 ping `3/3` 成功，但其路由查询显示探测经 `wgnetport` 和表 `52001`，不等于独立公网来源。分配地址的源路由回程已通过公网 TCP 响应验证；任意公网客户端主动新建到具体应用端口的完整验收仍取决于上游路由通告和 Windows 对该应用/端口的防火墙规则，本轮未改防火墙，也未声称所有应用端口都可达。

## 生产主控发布 0.3.34：内置分流能力协商修复 — 2026-10-18

- 排查客户端启用 IPv4 分流时报 `HTTP 400；节点未配置 IPv4 分流表`：生产主控仍为 `0.3.33`，尚未包含 `split_route_source=client_builtin_v1` 的能力协商逻辑，因此把新客户端请求按旧协议处理。
- 本地重新构建 `0.3.34`：Admin 构建、pmmandns 检查、主控 `43/43`、节点 `17/17`、x86_64 musl release 和主控 Docker 镜像均通过。内置分流回归测试确认节点没有 IPv4/IPv6 分流表时，新能力请求仍能返回基础内网路由。
- 已通过 `publish-server-release-txgz.ps1` 原子发布到 txgz。公网 `/v1/version` 返回 `0.3.34`，公网健康检查、Compose/PostgreSQL 健康检查、发布清单哈希和远端哈希校验全部通过。
- 生产数据库、用户、地址池、节点配置和节点服务未被覆盖或自动更新；节点仍由主控中的管理员流程独立管理。发布回滚备份：`/opt/pmman-network-dev/release-backups/20261017-174305-0.3.34-e94176d3`。

## 客户端升级包 0.1.17-alpha — 2026-10-18

- 基于单实例启动保护和动态 WireGuard MTU 修复构建轻量升级包。版本高于已交付的 `0.1.16-alpha`，产品名、文件版本均为 `Pmman Network / 0.1.17-alpha`。
- 产物 [Pmman-Network-0.1.17-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.1.17-alpha-x64-update.exe)，大小 `3299102` bytes，SHA-256 `E1082B4F18C7851DAACB734C11124FB3EDB068644DC0DBC59D0831126A25881E`。
- 本轮仅构建，未发布到生产主控，也未创建主控版本记录。

## 客户端单实例与动态 WireGuard MTU — 2026-10-18

- Windows 客户端 UI 启动入口新增系统级 `Global\\PmmanNetworkClient` 命名互斥体；重复启动立即退出，不会重复启动托盘 UI 或后台 service。WebView2 检查和更新器短期健康探针不占用互斥体，避免影响安装更新流程。
- 将 WireGuard 隧道 MTU 从固定 `1280` 改为连接时按所选节点公网 endpoint 探测：使用有界 DF/ICMP 负载二分搜索（路径 MTU 发现原理与 tracepath 相同），按外层 IPv4/IPv6 分别扣除 60/80 字节 WireGuard 封装开销，结果上限 1420，内层 IPv6 最低 1280。
- 探测失败时读取当前默认物理出口接口 MTU；仍不可用则回退 1280。探测到不足以承载 IPv6 的路径时拒绝安装隧道。endpoint、路径 MTU、来源、最终 MTU 和拒绝原因写入受限大小的 service 诊断日志。
- 验证：WSL service 测试 `38/38`、Windows MSVC service 测试 `38/38`、WSL master/node 测试 `43/43`/`17/17`、Windows service 与 Tauri `cargo check`、Vue 类型检查和生产构建通过；本地 release 客户端已构建。Hyper-V 中 UAC 接受和登录 UI 启动截图成功，但通用 Harness 因常驻客户端 PID 未按进程树清理而整体失败，且本次单文件载荷未包含 sidecar；因此不据此宣称单实例行为或真实隧道已验收。重复启动互斥验证和真实公网 PMTU/双栈流量验收仍待产品包环境完成。

## 不携带 WebView2 的全量安装包 — 2026-10-11

- 新增 `scripts/build-windows-installer.ps1 -WithoutWebView2`。该变体保留客户端、后台 service、主控配置和 WireGuard MSI，不嵌入或安装 WebView2；安装钩子要求系统已有 WebView2 Runtime，否则明确提示并中止。
- 修复无 WebView2 变体的 Tauri 资源映射：构建期间临时使用根目录 `pmman-client.json` 和 `runtime/wireguard-amd64-0.5.3.msi`，构建完成后恢复源码配置，避免 WebView2 被合并进包或主控配置落入错误目录。
- 产物 [Pmman Network_0.1.16-alpha_no-webview2_x64-setup.exe](../artifacts/windows-installer/Pmman%20Network_0.1.16-alpha_no-webview2_x64-setup.exe)，大小 `5785773` bytes，SHA-256 `D7D4A019355C5066348B8F17DF164EBFA35F9EEF475D1E67D3D8E4EA8BABE025`；静态资源扫描未发现 WebView2 安装包。
- Hyper-V 隔离安装器验收通过：UAC 接受、静默安装器退出码 0、断网网络配置、VM 关机和清理均通过。该包要求目标 Windows 已预装 WebView2 Runtime。

## Review、生产发布 0.3.32 与客户端升级包 0.1.16-alpha — 2026-10-11

- 完成工作区 review：修复发布/验证脚本默认升级包文件名仍为旧 `PMMan-Network-Client` 的问题，统一使用 `Pmman-Network-<version>-x64-update.exe`；补充 `target-*` 构建缓存忽略规则，未将构建缓存、`output`、`artifacts` 或凭据纳入版本库。
- Windows 后台服务端口转发防火墙逻辑复核通过：规则只绑定 `pmman-client` 接口、目标端口和 TCP/UDP 协议；配置失败保留主控租约并提示手动放行，不会误释放映射。
- Rust 测试通过：service `32/32`、protocol `19/19`、server `38/38`、node `17/17`；管理后台 Prettier、ESLint、TypeScript 检查和生产构建通过；本地 WSL musl 主控/节点构建和 Docker 镜像构建通过。
- 客户端版本递增为 `0.1.16-alpha`。升级包 [Pmman-Network-0.1.16-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.1.16-alpha-x64-update.exe)，大小 `3217162` bytes，SHA-256 `C398800F1DD2B280584374E773AF5FD256D536BB971E9BE288D626B1717836F8`；产品名/文件版本均为 `Pmman Network / 0.1.16-alpha`。
- 主控版本递增为 `0.3.32`，发布归档 `pmman-server-node-0.3.32.tar.gz` SHA-256 `CDD173614135D37990926965A4D864DEFA619DEDBD924F86139F32A2E4026835`。已在 txgz 原子激活，生产 `/healthz` 返回 `ok`、主控/PostgreSQL 均为 healthy、`/v1/version` 返回 `0.3.32`，生产 Compose 使用 `pmman-network/master:0.3.32`。
- 发布后发现校验脚本在 Docker 替换窗口内立即读取到 `starting` 会误报失败；已将 `verify-server-release.ps1` 改为最多 60 秒等待公开健康检查，避免下次发布出现“已激活但脚本失败”的竞态。
- Hyper-V 隔离启动检查确认 UAC、客户端进程和 WebView2 启动路径；由于客户端按设计常驻托盘，通用 smoke 的自动进程树清理留下 WebView 子进程，Harness 将该次记为清理失败，未将其宣称为完整 UI 通过。公网 WireGuard/端口转发流量验收不在本次离线隔离检查范围。

## 端口转发 Windows 防火墙闭环 — 2026-10-11

- 修复“主控和节点租约已 active，但 Windows 客户端防火墙未放行导致外部访问不通”的闭环缺口。
- Windows 后台服务按活动租约汇总目标端口和 TCP/UDP 协议，使用系统 NetSecurity 规则创建专属 `Pmman Network Port Forward` 规则；规则限定 `pmman-client` WireGuard 接口和精确目标端口，不开放其它本机接口或全部入站流量。
- 创建、释放、转发列表刷新、连接成功、断开、服务端撤销和服务启动都会同步规则；配置失败会在 IPC 中明确返回手动放行范围，同时保留映射租约供用户修复防火墙后继续使用。
- 本轮尚未执行带管理员权限的 Windows 实机公网端口流量验收；需在 Windows service release 上创建 TCP/UDP 映射后，从外部网络验证目标服务实际可达，并确认 `Get-NetFirewallRule -Group 'Pmman Network Port Forward'` 只包含对应租约端口。

## 生产主控 0.3.31 与客户端升级包 0.1.15-alpha — 2026-10-11

- 复核客户端发布版本：上一版交付为 `0.1.14-alpha`；此前误按源码残留的 `0.1.13-alpha` 构建的包不作为交付。已将前端、Windows service、Tauri 配置和 Cargo lock 统一提升为 `0.1.15-alpha`。
- 本地 MSVC 构建轻量升级包 `artifacts/client-updates/Pmman-Network-0.1.15-alpha-x64-update.exe`，大小 `3198704` bytes，SHA-256 `44F8FA6E8ED5963F70DA16B08371B7C08DB2443F4BFF290A3AEE0610D3E03CEC`。
- 生产发布 `0.3.30` 时发现激活脚本每次发布都会重复执行一次性旧数据迁移；检测到 PostgreSQL 中已有 `legacy-import.completed` 后自动回滚，生产仍保持原版本和数据。已修复激活逻辑，并以 `0.3.31` 重新构建发布。
- `0.3.31` 已在 txgz 原子激活，生产 `/healthz`、Compose 主控/PostgreSQL 健康、服务端版本、节点资源和远端哈希验证全部通过；现运行镜像为 `pmman-network/master:0.3.31`。未自动创建、覆盖或删除节点及网络配置。

## 全量 Code Review 与本地容器验收 — 2026-10-11

- 修复普通用户撤销设备的生命周期顺序：设备现在先写入 PostgreSQL 并标记为 `revoked`，再清理主控网关、节点 peer 和端口转发；网关清理失败不会让设备在主控重启后复活，接口会返回明确的 `502`/`202` 状态。
- 修复客户端 heartbeat 和过期租约的持久化一致性：PostgreSQL 写入失败时不再静默更新内存状态，客户端 heartbeat/P2P 目录/节点 heartbeat 会返回 `503`，过期租约也不会只在内存中撤销。
- 修复旧 JSON/SQLite 迁移：所有源文件和端口转发记录先完整解析，策略字段不再用 `unwrap_or_default()` 吞掉坏数据；文档、入口、策略、租约、事件和迁移审计现在在同一个 PostgreSQL 事务中提交，失败自动回滚，已有活动入口的地址族/节点/范围变更会被拒绝，事件导入可安全重试。
- 修复更新文件原子写入和更新阶段文件的潜在 panic，缺少有效文件名或 JSON 编码失败现在会返回可诊断错误。
- 本地 WSL 主控 release musl 构建、Docker 镜像构建和 `scripts/verify-compose.py` 通过：49 项 Compose 检查通过，覆盖非 root 主控、空环境无 demo 数据、管理员改密、主控重启、PostgreSQL 重启、数据库不可用时 503、不回退内存数据、资源目录权限和容器替换后的数据保留；测试容器、网络和卷已清理。
- Rust 单元测试：主控 37/37、节点 17/17、Windows 后台服务 31/31；管理后台 Prettier、ESLint、vue-tsc 和生产构建通过。迁移二进制 `--features legacy-migration` 编译通过。
- 复核 Windows 构建环境：本机已安装 Visual Studio 2022 Build Tools，`vcvars64.bat` 可加载 MSVC 14.43.34808，`cl.exe`/`link.exe` 可用；此前普通 PowerShell 未加载 VS 开发者环境，不能据此判断机器缺少 MSVC。通过项目脚本 `scripts/build-client.ps1` 完成 Windows service release、Vue 生产构建和 Tauri client release。Windows target `cargo check --locked` 及正式构建均通过。

## shixcm 新节点接入故障修复 — 2026-10-07

- 排查节点 `nbd-sha-01` 未上线问题：节点服务并非卡在公网 UDP 或主控认证，而是在启动阶段因空的 `ipv4-split.routes` 被判定为无效配置，随后 systemd 无限重启，未执行注册。
- 已在 shixcm 节点移除两个空分流文件的环境变量引用；服务恢复 `active (running)`，主控已记录节点注册、公钥和 `cn-shixcm.pmman.tech:23077/23078` 端点，节点 WireGuard 与 UDP 探测 socket 正常监听。
- 从本机实测 `cn-shixcm.pmman.tech:23078/UDP` 返回 24 字节探测回显，IPv4 RTT 约 42 ms。
- 修复节点程序：空的可选分流文件按未配置处理，不再阻止节点启动；新增回归测试，节点测试从 15 项增至 16 项并全部通过。
- 修复一键接入脚本：未填写 IPv4/IPv6 分流 CIDR 时不创建空文件，也不写入对应环境变量。管理页 Prettier、ESLint、vue-tsc 和生产构建全部通过。
- 本地服务端发布 `0.3.12` 已部署到 txgz；发布健康检查、远程哈希校验和节点资源校验通过。发布脚本同时固定 txgz SSH/SCP 使用 IPv4，避免 DNS AAAA 不可达导致上传失败。

## pmman_network BIRD 路由导出 — 2026-10-07

- 新增可复用 `/etc/bird/pmman_network.conf`，部署参数集中在顶部 `define`：`PMMAN_NETWORK_KERNEL_TABLE`、`PMMAN_NETWORK_INTERFACE`、`PMMAN_NETWORK_SCAN_TIME`；其他节点只需修改这三个变量。
- `pmman_network_direct` 从配置的 WireGuard 接口直接学习主表中的 connected route，并直接进入现有 `master4/master6`；`pmman_network_kernel_v4/v6` 继续从专用 Linux 表读取动态 return route，通过专用 BIRD table 和 pipe 汇入 master。BIRD 只允许一个 kernel syncer 绑定 master，因此不能把第二个 kernel protocol 直接绑定 master。
- 节点安装 Linux return route 时使用 `scope global`，避免 BIRD kernel protocol 忽略默认的 `scope link` 路由。
- 本地构建服务端发布 `0.3.11`，master 32 项测试、node 15 项测试全部通过；未在 txgz 远程编译，使用本地产物部署并通过公网发布校验。
- txgz 实测 `pmman_network_direct` 已将 `172.20.60.96/27 dev wgnetport`、`2a0f:1cc5:4700:beef::/64 dev wgnetport` 直接学习到 `master4/master6`，并出现在 `internal_ospf/internal_ospf_v6` export 中；该 connected route 不再经过 pipe。
- 当前 txgz 的服务由项目专属 `pmman-dev-master.service` 和 `pmman-dev-node-tencent-gz-02.service` 管理；未修改宿主机其它 WireGuard、BIRD、路由、DNS 或网站配置。
- 随后验证并收紧回写策略：BIRD pipe 保留原始 `proto`，因此改为按 `pmman_network_direct`、`pmman_network_kernel_v4/v6` 精确控制 OSPF 放行和 kernel 拒绝；不再依赖宽泛的 `source = RTS_PIPE` 判断。
- 已部署到 `ssh shixcm`（主机 `nbnet-810`）：BIRD 2.19.1 使用 `wgnetport`、Linux table `52001`，`pmman_network_direct`、双 kernel protocol 和双 pipe 均为 `up`；现有 OSPF v2/v3 邻居保持 `Full/PtP`。当前 table 52001 为空，因为没有在线客户端租约；接口直连的 `172.20.60.96/27` 和 `2a0f:1cc5:4700:beef::/64` 已出现在 OSPF v2/v3 export 中。
- `scripts/txgz/install-pmman-network-bird.py` 现在兼容带本地注释的 OSPF/kernel filter；当 live `birdc configure` 因新增 kernel protocol 权限失败时，可用 `--restart-on-configure-failure` 在语法检查通过后安全重启 BIRD，并在失败时恢复备份。shixcm 使用该路径部署成功，未修改其它 WireGuard、BIRD、路由或 DNS 配置。

## pmmandns 节点解析链路与客户端 DNS 展示修复 — 2026-10-07

### 生产节点实机国内/国外分流验收 — 2026-10-07

- 通过主控管理页将生产节点 `tencent-gz-02` 更新到节点资源 `0.3.7`，未直接覆盖节点文件。
- 临时配置节点 pmmandns IPv4 监听 `172.20.60.97:53`、国内上游 `223.5.5.5:53`、国外上游 `1.1.1.1:53`，并配置 `baidu.com`/`qq.com` 为国内规则、`google.com`/`cloudflare.com` 为国外规则。
- 宿主机客户端真实连接节点并分配 `172.20.60.106/32`、`2a0f:1cc5:4700:beef::a/128`；客户端 DNS 列表首项为节点 `172.20.60.97`。
- 实测 `Resolve-DnsName baidu.com -Server 172.20.60.97` 返回国内地址，节点抓包确认请求发送到 `223.5.5.5:53`；`google.com` 请求发送到 `1.1.1.1:53` 并返回国外解析结果。说明客户端到节点、节点分组匹配、对应上游转发和返回链路均真实生效。
- 生产节点 `wgnetport` 当前没有配置 `2a0f:1cc5:4700:beef::1`，因此本次没有启用 IPv6 DNS 监听；这不是 IPv4 分流失败。测试配置随后已通过主控恢复为原配置，客户端已断开，节点无残留 pmmandns 监听。

### 域名 DNS 分流 — 2026-10-07

- `NodeNetworkConfig` 和客户端 `DeviceConfig` 新增 `pmmandns_rules`，主控、节点心跳和 Windows 应用内 DNS 转发器全链路传递分流规则。
- 管理页的节点创建/编辑支持逐行填写 `域名后缀=DNS1,DNS2`，例如 `google.com=1.1.1.1:53,8.8.8.8:53`；支持将 mrouter 清单中的 `domain:example.com` 作为左侧域名写入。规则按最长后缀优先，`notgoogle.com` 不会误匹配 `google.com`。
- 规则 DNS 无有效答案时回退节点默认上游；节点仍无结果时客户端应用内转发器再回退本地 DNS。只有本次连接主控下发的 DNS/规则会被使用，不把规则写入发布包或客户端固定配置。
- 增加最长后缀优先、失败回退和真实 UDP socket 国内/国外分组转发测试；protocol 12 项测试通过，admin Prettier、ESLint、vue-tsc 和生产构建通过。

### mrouter 国内外 DNS 分组与管理表单 — 2026-10-07

- `NodeNetworkConfig` 新增 `pmmandns_domestic_upstreams`、`pmmandns_foreign_upstreams`、两组域名规则和 `pmmandns_default_group`；旧 `pmmandns_upstreams` 继续兼容。
- 节点按分组处理查询：国内规则使用 domestic 上游，国外规则使用 foreign 上游，未命中使用默认分组；客户端只把查询发送到节点 pmmandns，不再把上游 DNS 规则绕过节点直接执行。
- 节点本地部署配置支持 `PMMAN_DNS_DOMESTIC_UPSTREAMS`、`PMMAN_DNS_FOREIGN_UPSTREAMS`、`PMMAN_DNS_DEFAULT_GROUP` 及两组规则文件。
- 节点编辑与接入页面改为 Element Plus tabs 分组，避免单页长表单；国内/国外上游与规则分开配置。
- 主控/节点发布 `0.3.7` 已切换生产，公网健康检查和远程哈希校验通过。生产节点的一键更新请求因当前生产管理员凭据返回 401 未执行，未直接覆盖节点本机配置。
- 客户端修复为只向节点 pmmandns 查询，不绕过节点直连 domestic/foreign 上游；重新生成 `0.1.4-alpha` 全量/升级包，大小 `3083949` bytes，SHA-256 `D6AFBE7507C2E2887278A5C0CE4D6C46E3109ED86C93FA6DB13F339679F86A2C`。

- 主控的节点网络配置支持旧 `pmmandns_upstreams`、国内/国外分组上游和规则；配置通过节点心跳下发，管理员修改后不需要向节点开放 HTTP。主控 `0.3.7` 已部署到 txgz，公网健康和远程哈希校验通过。
- 节点进程内置 pmmandns 转发器，在 WireGuard 网关 IPv4/IPv6 地址监听 UDP/TCP 53，按主控配置依次查询上游；无有效结果时返回失败，让 Windows DNS 继续尝试客户端本地 DNS。
- 节点没有显式 DNS 监听地址但启用 pmmandns 时，主控和节点自动使用实际 WireGuard 网关地址，避免把地址池或不存在的 DNS 地址写入客户端配置。
- 客户端 IPC 现在区分节点 DNS和最终生效 DNS列表；连接详情不再把节点 DNS为空时的本地备用解析笼统显示为“使用系统 DNS”。
- 主控/节点本地 musl 测试通过（32 + 15），管理页规范检查、lint、类型检查和生产构建通过。
- 生产节点 `tencent-gz-02` 已切换到 `0.3.4` 节点资源并确认监听 `172.20.60.97:53` 与 `[2a0f:1cc5:4700:beef::1]:53` 的 UDP/TCP；当前尚未配置 pmmandns 上游，因此不会伪造 DNS 响应，管理员需要在节点配置中填写可达的内部上游 DNS。
- 客户端 `0.1.4-alpha` 已按本轮分流代码重新生成升级包和全量包：升级包 [Pmman-Network-0.1.4-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.1.4-alpha-x64-update.exe)，全量包 [Pmman Network_0.1.4-alpha_x64-setup.exe](../src-tauri/target/release/bundle/nsis/Pmman%20Network_0.1.4-alpha_x64-setup.exe)，两者均为 `3083949` bytes，SHA-256 `D6AFBE7507C2E2887278A5C0CE4D6C46E3109ED86C93FA6DB13F339679F86A2C`。
- 服务端发布归档 [pmman-server-node-0.3.7.tar.gz](../artifacts/server-releases/0.3.7/pmman-server-node-0.3.7.tar.gz) 大小 `5702030` bytes，SHA-256 `8D5545228A9E8A84F48104CAC3384A1A2EB711218D89DE8743A727FA9E041D4F`；主控已切换，生产节点资源仍遵循管理员在主控中的显式更新流程，不自动覆盖节点本机配置。

## 客户端 `0.1.3-alpha` 升级包 — 2026-10-07

- 将客户端版本提升至 `0.1.3-alpha`，包含节点列表 HTTP 状态解析、主控重启后的会话自动恢复和本次 P2P 目录修复配套客户端改动。
- 本地完成 Vue 生产构建、Windows MSVC 后台服务 release 编译、Tauri release 编译和 NSIS 轻量升级包生成。
- 升级包：[Pmman-Network-0.1.3-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.1.3-alpha-x64-update.exe)，大小 `3068614` bytes，SHA-256 `3CD8CCB7F41FE46BB77CDDD6B6EB94DC73A41A02FAA2F49F2C4699F8760B4269`。
- 本轮只生成升级包，没有上传或切换生产主控的客户端版本指针；管理员需要在主控创建 `0.1.3-alpha` 后上传该包和更新日志。

## 生产 P2P 目录 503 修复与宿主机真实验收 — 2026-10-07

- 根因确认：主控 P2P 目录接口选择健康节点后，直接用 `parse::<SocketAddr>()` 解析节点的 `host:port` 探测地址；生产节点使用 `cn-gz02.pmman.tech:51840`，因此即使节点心跳健康也会被主控误判为不可用并返回 HTTP 503。
- 服务端修复为在主控异步 DNS 解析节点探测域名，IPv4 优先，同时保留字面量 `IP:port`；锁不再跨越异步解析。回归覆盖域名探测地址，主控 32 项、节点 15 项测试全部通过。
- 本地生成并部署服务端 `0.3.3`：主控、节点资源和远程哈希核验通过，公网 `healthz` 与 `/v1/version` 通过；未覆盖地址池、用户、节点注册和网络配置。回滚快照保留在 `/opt/pmman-network-dev/release-backups/20261006-235500-0.3.3-bdfa780d`。
- 宿主机真实客户端重启后台服务后自动恢复已保存登录状态，节点 `tencent-gz-02` 健康，UDP 延迟 9ms；点击连接成功建立真实 WireGuard 隧道，分配 `172.20.60.106/32` 和 `2a0f:1cc5:4700:beef::a/128`，IPv4 网关探测成功。P2P 状态显示“按需直连已就绪；UPnP 已启用”，无 HTTP 503；当前仅一个在线客户端，因此 `p2pPeers=[]` 属于正常结果。
- 客户端节点列表请求新增 HTTP 状态码和错误正文解析，401/503 不再显示 `json: EOF`；连接遇到主控重启导致的 401 时，会使用本机 DPAPI 保存的凭据自动刷新会话后重试。Windows MSVC `cargo check`、WSL service 21 项测试通过。
- IPv6 ICMP 回显未作为连接成功条件；当前实现以 WireGuard 握手和业务流量判断隧道，避免将运营商/主机禁用 ICMP 误报为隧道失败。

## 启动自动登录提示与生产节点心跳修复 — 2026-10-07

- 新增 `authRestorePending` IPC 状态。客户端启动时如果存在本地加密登录凭据，会显示“正在启动后台服务并自动登录”加载页，直到自动登录成功或失败；没有凭据时直接进入登录页。
- 修复后台服务监听 IPC 后异步恢复登录造成的登录页闪现和无提示等待。
- 生产 txgz 节点 `tencent-gz-02` 曾出现进程存活但主控内存中的心跳租约失效，导致 P2P 目录接口返回 HTTP 503。已重启项目专属节点服务并确认管理面板显示在线，最后心跳 `2026/10/7 07:31:01`。
- 启动提示修复版本为 `0.1.2-alpha`，已生成全量安装包 SHA-256 `41FD18F0788D107C921BC6E93F4781D2D614D01AC9F2A9BCE44D689CD4E9143A`、轻量升级包 SHA-256 `FD0BB0044829100C0DACFFE27A989161142ED94A02E3D5F5A99A142EBCA9F39E`、standalone 客户端 SHA-256 `B64A87D62D6A936045BA4A611D8DF00D2272F52C75E84A52657FD300FD111A71`；生产主控不会自动切换客户端发布版本。

## P2P 目录同步与 UPnP 修复 — 2026-10-07

- 修复 Windows UPnP SSDP 探测绑定 `0.0.0.0` 导致家庭网关不响应的问题。现在使用 P2P 实际 IPv4 出口地址绑定发现 socket，并将发现超时从 900ms 提高到 3 秒；UPnP 仍是增强路径，失败时继续使用 UDP 打洞和节点中转。
- 修复互联目录同步把主控空响应误报为 `json: EOF` 的问题。客户端现在先检查 HTTP 状态并解析错误正文，503 会显示“当前没有健康节点”，502 会显示主控/节点暂时不可用，认证和设备错误也会分别提示。
- 目录短暂同步失败时保留最近一次成功的 peer 目录和连接状态 30 秒，避免客户端列表因一次代理/节点抖动立即消失；持续失败超过 30 秒才清理本地 peer。
- P2P 回归测试覆盖真实 UDP 报文按需触发直连、直连失败后的中转回退、状态防抖；Windows service 测试 21 项通过，主控 32 项和节点 15 项测试通过。
- 修复版本发布为 `0.1.1-alpha`，已生成：
  - [全量安装包](../artifacts/windows-installer/Pmman%20Network_0.1.1-alpha_x64-setup.exe)，SHA-256 `7BAF61CABEDB600E8FABD22515112C00BB7D68CDE18AF075CBC5D34C326DEC7A`。
  - [轻量升级包](../artifacts/client-updates/Pmman-Network-0.1.1-alpha-x64-update.exe)，SHA-256 `6A0AE922181BFF1095F09C228AFF7CE3FA36A90E708A0B7E23F391C95D1FCF04`。
  - [standalone 客户端](../artifacts/standalone/pmman-client.exe)，SHA-256 `53DCB5778345C90D4AD3C1FAD0417A0B99BABA9EC0140A2CF007DB2F786B5427`。
- 生产主控 `https://network.pmman.tech` 当前在线，txgz 主控与 `tencent-gz-02` 节点 systemd 均为 active；未修改生产地址池、节点配置或持久化数据。管理员需在主控创建 `0.1.1-alpha` 发布并上传轻量升级包后，现有 `0.1.0-alpha` 客户端才会收到更新。

## 客户端品牌与版本显示修复 — 2026-10-07

- 统一客户端用户可见品牌为 `Pmman` / `Pmman Network`，清理登录页和更新弹窗中的旧品牌写法及冗余引导文案。
- 首页右下角和“偏好设置 → 关于”统一显示当前前端构建版本，版本来源为 `package.json`（当前为 `0.1.0-alpha`），不再回退到 `0.0.1a`。
- 前端生产构建通过；更新器和安装器中的相关用户提示同步使用 `Pmman Network`。
- 修正客户端构建脚本的服务版本默认值：未设置环境变量时跟随 Tauri manifest，不再回退到 `0.0.1a`。
- 客户端版本统一提升至 `0.1.0-alpha`，同步更新 package、Tauri manifest、service Cargo manifest/lockfile，并重新生成全量安装包和轻量升级包。
- 全量安装包 [Pmman Network_0.1.0-alpha_x64-setup.exe](../artifacts/windows-installer/Pmman%20Network_0.1.0-alpha_x64-setup.exe)，大小 `220833455` bytes，SHA-256 `4115835A0FD4B1E4DAA139B25480784B517211AF69D44A4443A524A1AF4CEC9E`；轻量升级包 [Pmman-Network-0.1.0-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.1.0-alpha-x64-update.exe)，大小 `3072669` bytes，SHA-256 `0AC983FB3AC6E5FFC88B6593DE0ACFD42CC2BE65718B9E0CC20D3DF39ADE228B`。
- standalone 客户端 [pmman-client.exe](../artifacts/standalone/pmman-client.exe) SHA-256 为 `B7F909E30EB5B2BF20D26594F4B43E53EAB86E9FD78476363AC9C4D55CB0918C`。

## Windows 客户端用户自助修改密码 — 2026-10-06

- 客户端“网络账户”弹窗新增“修改密码”入口，支持当前密码、新密码和确认密码校验，提交期间锁定重复操作，并显示成功或失败原因。
- 新增 `ChangePassword` IPC 协议和 Tauri command。Windows service 只通过主控 HTTPS `/v1/auth/password` 修改密码，不把用户凭据发送给节点；改密成功后保留当前会话和现有隧道。
- 改密成功后 service 使用 Windows DPAPI 原子更新本地自动登录凭据；本地保存失败会明确提示下次手动登录，不伪装成完全成功。改密失败不会把正在连接的 UI 状态错误地改成“未连接”。
- 当前功能版本为 `0.0.7-alpha`。前端生产构建、protocol 6 项测试、service 20 项测试和 Tauri Windows 检查通过。
- 最新全量安装包 [Pmman Network_0.0.7-alpha_x64-setup.exe](../artifacts/windows-installer/Pmman%20Network_0.0.7-alpha_x64-setup.exe)，SHA-256 `119032AF7FFD441BE58C02D80FE6989DC4E776094F5ABE80F03E937F8847F342`；最新轻量更新包 [Pmman-Network-0.0.7-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.0.7-alpha-x64-update.exe)，大小 `3072945` bytes，SHA-256 `C8AC761965DD621DD4428F1E0302C9571C36EE98717C4B821E333A46F83A6E5D`。standalone 客户端 [pmman-client.exe](../artifacts/standalone/pmman-client.exe) SHA-256 为 `DC5BDFAA564C8CDFAACD1F95D29CC46563A7E6B14A770E03EAAAE365FA957E15`。
- 最终更新包 Hyper-V 验收 `executable-test-20261006T072017072Z-8ed7f685` 通过：`HarnessSucceeded=true`、`TestEvaluated=true`、`TestPassed=true`、`VmFinalState=Off`、更新器退出码 0、新版本 `0.0.7-alpha` UI/IPC 健康检查通过、配置更新前后 SHA-256 一致、进程和载荷清理成功。

## Windows 客户端用户自助修改密码 — 2026-10-06

- 客户端“网络账户”弹窗新增“修改密码”入口，支持当前密码、新密码和确认密码校验，提交期间锁定重复操作，并明确显示成功或失败结果。
- 新增 `ChangePassword` IPC 协议和 Tauri command。service 只向主控 HTTPS `/v1/auth/password` 发起请求，不把用户凭据交给节点；改密成功后保持当前 Bearer 会话、隧道和连接状态不变。
- service 使用 Windows DPAPI 将新密码原子更新到本地登录状态，后续启动自动登录使用新密码；本地凭据保存失败时明确提示用户下次手动登录，不伪装成完整成功。
- 前端生产构建通过；protocol 6 项测试、Windows service 20 项测试、Tauri Windows `cargo check --locked --offline` 通过。

## 客户端自动更新最终修复与隔离验收 — 2026-10-06

- 修复更新器等待旧 UI 进程的竞态：Windows 使用原生进程句柄和 `WaitForSingleObject` 等待旧客户端退出；客户端启动更新器后使用 `ExitProcess(0)` 立即释放自身映像锁，避免旧 UI 仍存活导致更新器超时并重新打开旧版本。
- 更新器继续从 `%TEMP%\Pmman Network\updater\<pid>` 的独立副本运行，校验清单签名、包大小和 SHA-256，使用 NSIS `/S` 和原安装目录 `/D=` 静默替换；更新失败保留备份并自动恢复旧客户端。
- 修复 Hyper-V 验收脚本的日志序列化缺陷：只保留限长纯文本，避免 PowerShell 远程对象元数据递归序列化造成 `OutOfMemoryException`。
- Hyper-V 最终验收 `executable-test-20261006T062331512Z-96edb300` 通过：旧安装退出码 0、更新器退出码 0、最终阶段 `complete`、新版本健康探针 `ui=true/ipc=true`、配置更新前后 SHA-256 均为 `48912c4c4ebf270c9b892d28abb738ac8bfd2afa4439a04828d572be72e885e2`、`HarnessSucceeded=true`、`TestEvaluated=true`、`TestPassed=true`、`VmFinalState=Off`、进程/载荷清理完成；截图已人工检查，桌面无安装向导且保留 Pmman Network 快捷方式。
- 交付包：全量安装包 [Pmman Network_0.0.6-alpha_x64-setup.exe](../src-tauri/target/release/bundle/nsis/Pmman%20Network_0.0.6-alpha_x64-setup.exe)，SHA-256 `03FA25BA4AABAB7B17952CF50069763FA688752D80226D1627CAF8A1D9C90951`；轻量更新包 [Pmman-Network-0.0.6-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.0.6-alpha-x64-update.exe)，SHA-256 `440EC1CC82AB6AB01DF7D4189021F5F79915AEA9DCAF908258BAFA933C3B203A`。
- 本轮仅完成本地构建和隔离更新验收，未自动修改主控版本清单；管理员需在主控「客户端版本」中显式创建对应版本并上传轻量更新包。

## 全量代码审计与修复 — 2026-10-06

- 修复主控认证生命周期：过期会话在认证时清理，管理员停用或重置账户会立即撤销该账户所有 Bearer 会话；新增 `/v1/auth/logout`，客户端退出登录时同步撤销主控会话。管理员尚未修改初始密码时，服务端拒绝进入管理 API，但仍允许通过密码接口完成首次改密。
- 修复节点禁用边界：禁用节点不能继续注册、心跳或下载节点资源；节点资源仍通过节点专属 token 绑定。
- 修复主控状态落盘并发风险：用户、节点、客户端发布清单、发布包和 channel pointer 使用 UUID 临时文件、`sync_all` 和原子提交，客户端发布新包使用摘要绑定的不可变文件名，失败时保留旧发布可用，不再删除旧包制造回滚空窗。
- 移除两个公网验收脚本中的默认账户密码回退；`verify-real-lab.ps1` 要求 `PMMAN_ADMIN_PASSWORD`，`verify-traffic-modes.ps1` 要求 `PMMAN_TEST_USERNAME`/`PMMAN_TEST_PASSWORD`。
- 本轮静态审计尚未执行生产发布或覆盖现有节点；待本地回归通过后，再按 `docs/RELEASES.md` 生成并发布新服务端产物。凭据和临时发布元数据继续只放在 `artifacts/`。

## 客户端更新退出竞态修复 — 2026-10-06

- 修复 `0.0.4-alpha` 更新后旧客户端重新打开、更新未完成的问题。根因是 `app.exit(0)` 只安排 Tauri 的优雅退出，独立更新器等待父进程时旧 UI 仍可能存活并持有主程序映像；更新器遂记录 `old client still running` 并恢复旧版。
- `install_update` 在成功启动临时目录中的独立更新器后立即调用 `std::process::exit(0)`，由更新器负责等待父进程、静默替换原安装目录、健康检查和失败回滚。
- 版本提升为 `0.0.5-alpha`，避免客户端继续使用已缓存的 `0.0.4-alpha` 更新包。`pnpm build`、MSVC release 构建和服务端 18 项测试通过；Tauri 测试二进制因 `requireAdministrator` 清单无法在普通 Cargo runner 中直接执行，已记录为提权测试限制。
- `0.0.5-alpha` 全量安装包：[Pmman Network_0.0.5-alpha_x64-setup.exe](../src-tauri/target/release/bundle/nsis/Pmman%20Network_0.0.5-alpha_x64-setup.exe)，大小 `220802729` bytes，SHA-256 `6817C5D51637C70BB1258173F6DE58BDFD3358276AA4615402790DC4A528C14E`；轻量更新包：[Pmman-Network-0.0.5-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.0.5-alpha-x64-update.exe)，大小 `3063514` bytes，SHA-256 `C1EF7A12D52B243E8BAC80824E5B86AF07A49DCFF3263A630DBCFB2AB7CEF960`。未自动修改主控发布记录。
- Hyper-V 客户端启动验证 `executable-test-20261006T032454406Z-43a7148d` 已执行并回收 VM，但测试框架未能在该 worker 内捕获 `requireAdministrator` 对应的精确提升进程（`SystemPromptAcceptanceFailed`），因此不将其记为应用通过；该结果不表示客户端更新逻辑失败。此前 `0.0.4-alpha` 的安装包隔离验证仍为通过状态。

## 客户端更新失败修复 — 2026-10-06

- 修复“点击立即更新后客户端退出，但更新没有继续，重新打开仍反复提示更新”。根因是更新器由安装目录内的 `pmman-client.exe` 自身启动，更新器进程仍然持有该 exe 文件锁，NSIS 无法可靠覆盖正在运行的主程序；失败发生在 UI 已退出之后，用户看不到错误。
- 更新流程现在为：校验包体和签名 → 将当前 exe 复制到 `%TEMP%\Pmman Network\updater\<pid>` → 退出旧 UI → 独立更新器等待父进程退出 → NSIS `/S /D=<原安装目录>` 静默覆盖 → 新客户端 IPC 健康检查 → 成功后完成，失败则恢复旧文件并重新启动旧客户端。
- 新增 `%LOCALAPPDATA%\Pmman Network\update.log`，记录更新阶段、父进程退出、安装器退出结果、健康检查和回滚原因；不记录密码、token 或私钥。健康探针退出前会停止它临时启动的后台服务，避免更新后残留 service。
- 更新包 NSIS hook 不再因组织策略拒绝 P2P 防火墙规则而中止整个软件更新，只记录安装详情；管理员仍可按策略单独放行规则。
- 版本提升为 `0.0.3-alpha`。全量安装包：[Pmman Network_0.0.3-alpha_x64-setup.exe](../src-tauri/target/release/bundle/nsis/Pmman%20Network_0.0.3-alpha_x64-setup.exe)，SHA-256 `49677CF4A8E927954E16773E20A6598F4C76B4DDF096F98C34C6B44D8492DA71`；轻量更新包：[Pmman-Network-0.0.3-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.0.3-alpha-x64-update.exe)，SHA-256 `A7DE99C975560BE3A4D12416029416F8D15E45B907AC1D93DC4D0C9C05B056A5`。
- 本地 `pnpm build`、MSVC `cargo check`、WSL service 18 项测试、protocol 6 项测试通过。Hyper-V 隔离测试 `executable-test-20261006T022537026Z-a3f22326` 通过：更新包 UAC 摘要匹配、静默安装退出码 0、目标目录生成客户端/service/卸载程序，`HarnessSucceeded=true`、`TestEvaluated=true`、`TestPassed=true`、`VmFinalState=Off`、进程和载荷清理通过。该隔离测试未连接生产主控，也未替代用户在主控发布后的真实 UI 更新验收。
- 随后发现本机仍运行 `0.0.1-alpha`，而主控 `0.0.3-alpha` 指针已是未包含兼容桥的旧包；因此再次点击更新仍执行旧版锁文件路径。更新包增加旧版 NSIS Restart Manager 兼容桥，并提升发布版本为 `0.0.4-alpha`，避免客户端继续复用旧 `0.0.3-alpha` 缓存。
- `0.0.4-alpha` 全量安装包：[Pmman Network_0.0.4-alpha_x64-setup.exe](../src-tauri/target/release/bundle/nsis/Pmman%20Network_0.0.4-alpha_x64-setup.exe)，大小 `220825323` bytes，SHA-256 `929599095E1094C4A4A4DB05FEBF88EEFCA75F703BF5E7E50AEA907228A1A07D`；轻量更新包：[Pmman-Network-0.0.4-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.0.4-alpha-x64-update.exe)，大小 `3063930` bytes，SHA-256 `4AC5485EE24E79EB25261F838754A30F7C5B3A5A378563CAD8B7A33837C0411D`。未自动修改主控发布记录。
- 新包 Hyper-V 隔离验证 `executable-test-20261006T030047933Z-2e1cc690` 通过：UAC 摘要与包体匹配、静默安装退出码 0、`HarnessSucceeded=true`、`TestEvaluated=true`、`TestPassed=true`、`VmFinalState=Off`、进程/载荷清理成功；截图已人工检查。

## Latest production correction

- 2026-10-06：修复生产连接 `控制面注册失败 HTTP 502`。根因是生产节点把 WireGuard 和 UDP 探测配置成同一端口，节点探测 socket 占用端口后导致专用 WireGuard 接口无法 UP，主控等待节点安装最终返回 502。服务端现在透传控制面错误正文，并拒绝保存相同的 WireGuard/UDP 探测端口；本地构建发布 `0.3.1` 后，txgz 主控保持健康。生产节点已迁移到项目专属 `pmman-dev-node-tencent-gz-02.service`，WireGuard 使用 51830、UDP 探测使用 51840，`wgnetport` 已恢复 UP，公网 UDP challenge RTT 约 11ms。清理的 `pmman-dev-a/b` 是项目遗留测试接口，未触碰宿主机其他 WireGuard/BIRD/路由/DNS 配置。
- 2026-10-06：客户端登录状态改为首次登录后持久保存。Windows service 使用当前用户 DPAPI 加密保存用户名和密码到 `%LOCALAPPDATA%\\Pmman Network\\login.json`，启动时自动恢复 session；主动退出登录会清除凭据，自动恢复失败不会删除凭据。并加入自动恢复与手动登录/退出的并发保护。Windows MSVC service check、WSL musl service 18 项测试和客户端 Vue/Tauri 构建通过。
- 2026-10-06：按发布流程生成客户端 `0.0.2-alpha` 全量安装包和轻量更新包，未自动创建主控版本。全量安装包：[Pmman Network_0.0.2-alpha_x64-setup.exe](../src-tauri/target/release/bundle/nsis/Pmman%20Network_0.0.2-alpha_x64-setup.exe)，SHA-256 `A9F4291F787FD0CD0D649BC8F6A6D1FC099DF89D8E9808121C2A261FBA901ACF`；更新包：[Pmman-Network-0.0.2-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.0.2-alpha-x64-update.exe)，SHA-256 `60EB891501AD5A6F2EA629871412CDD18D75B94CB9E73416E16EA5E6E7836B8B`。请在主控「客户端版本」中创建 `0.0.2-alpha` 版本并上传更新包/更新日志。
- 2026-10-06：发布 `0.0.6-alpha`。移除生产主控中的两条历史客户端 demo 版本，stable/beta 现在默认为空，只能由管理员显式上传发布；公网客户端更新接口返回 `available=false`。
- 2026-10-06：生产发布 `0.0.7-alpha` 到 txgz，修正节点 WireGuard 接入流程：主控不再要求预填公钥，节点启动时自动生成私钥、创建接口并上报公钥；公网 `/healthz`、`/v1/version`、主控/节点资源 SHA-256 核对全部通过。旧版本回滚快照：`/opt/pmman-network-dev/release-backups/20261006-003243-0.0.7-alpha-3d60b9c5`。
- 2026-10-06：发现 `0.0.7-alpha` 更新重启后地址池和内网 CIDR 回到默认值，原因是该配置此前只保存在内存。已新增独立的 `network-settings.json` 原子持久化、启动加载、写入失败保护和重启回归测试；当前生产现场没有旧配置副本，无法从主控恢复更新前的自定义值，修复发布后重新保存即可永久保留。
- 2026-10-06：生产发布 `0.0.8-alpha` 到 txgz，公网健康检查、版本和远程二进制/节点资源哈希核对通过。主控已创建权限为 `600` 的 `/opt/pmman-network-dev/network-settings.json`；当前文件内容为默认地址池和默认内网 CIDR，管理员重新保存后会在后续更新中保留。
- 2026-10-06：修正节点 WireGuard 身份流程。主控创建待接入节点时不再要求公钥；一键部署启动的 `pmman-node` 会在节点本地生成并保护私钥、自动创建/配置专用 WireGuard 接口和双栈地址，再将派生公钥注册到主控。管理页公钥改为注册后的只读状态，主控不再接受管理员手工覆盖公钥；旧持久化覆盖值在节点重新注册时自动失效。管理页、节点文档、架构文档和 txgz 本地节点夹具已同步更新。
- 删除源码中的 `admin123`、`pmman123` 等默认密码，主控未配置 `PMMAN_ADMIN_PASSWORD` 时拒绝启动；发布验收不再依赖管理员密码，避免管理员改密后发布回滚。
- 0.0.6-alpha 服务端归档：`artifacts/server-releases/0.0.6-alpha/pmman-server-node-0.0.6-alpha.tar.gz`，SHA-256 `0726ff086c23181ffba883c0c1aeb6214c3646f655bc6211df7f9923c555c331`。
- 2026-10-06：发布 `0.0.4-alpha`。生产主控现在是空节点部署：不自动创建、更新或覆盖节点，不再注入测试账户；主控仅承载节点资源、接入凭据和管理 API。公网验证通过，节点列表为空，账户仅保留管理员 `admin`，首次登录必须修改初始密码。
- 节点接入改为管理员在「节点管理」显式创建并部署；节点编辑支持名称、区域、WireGuard 公网地址、UDP 探测地址、返回路由表、启停和限速，节点公钥仅展示注册状态，不允许手工覆盖。
- 新增 `/v1/auth/password` 和「账号安全」页。账户变更写入 `PMMAN_USER_STORE`，管理员重置密码后会强制目标账户下次登录修改；初始管理员密码不再是长期有效的 demo 凭据。
- 0.0.4-alpha 服务端归档：`artifacts/server-releases/0.0.4-alpha/pmman-server-node-0.0.4-alpha.tar.gz`，SHA-256 `21663fdd731a1d09251d19278837692ee435fd40bb74c8dc5a155ad7401e649c`。

## Completed

- 2026-10-06 首次发布 `0.0.1-alpha`：本地 WSL 构建并测试主控/节点 musl 产物，Windows MSVC 构建 Pmman Network 安装包。服务端归档为 `artifacts/server-releases/0.0.1-alpha/pmman-server-node-0.0.1-alpha.tar.gz`，SHA-256 `E21E7546A6D78F659D92CE1FB556C8A01A04EB00D283C12BDC3C3D0A3DEB7FDC`；主控和节点 SHA-256 已与 txgz 远端逐项核对；Windows 安装包为 `src-tauri/target/release/bundle/nsis/Pmman Network_0.0.1-alpha_x64-setup.exe`，SHA-256 `2EA75D9389053877F4A20ED9AC9DD64A7005CEDBECC323410D7DC6D204E23868`。
- txgz 已原子切换到 `0.0.4-alpha`。主控仅监听 `127.0.0.1:8787`，公网管理入口为 `https://network.pmman.tech/admin`，公网 `/healthz`、`/v1/version`、空节点列表和远端二进制哈希验证全部通过。nginx 反代配置由运维管理，项目 bootstrap 不再修改 nginx。

- 2026-10-06：新增主控可配置的独立内网 CIDR 列表。管理页支持多行 IPv4/IPv6 CIDR，主控负责格式校验、规范化和去重，地址池仍只负责客户端地址分配；最终列表作为所有客户端配置的内网 `AllowedIPs`。连接到内网时节点 DNS 始终优先，Windows 服务将连接前读取的本地 DNS 去重后追加为备用。产品可见品牌统一为 `Pmman Network`，未发布产品显示版本重置为 `0.0.1a`，Cargo/Tauri 内部使用合法的 `0.0.1-alpha`。
- 2026-10-06：本轮本地构建通过：admin Prettier/ESLint/vue-tsc/生产构建、客户端 Vue 构建、Windows MSVC service/Tauri release、NSIS 零准备安装包。安装包为 `src-tauri/target/release/bundle/nsis/Pmman Network_0.0.1-alpha_x64-setup.exe`，SHA-256 `6D6DF70D152CD33938AD8FEB0084EB7A45E1E06E0882FE9A7E6BD59951527C03`。Hyper-V 隔离 smoke 请求 `executable-test-20261005T230256880Z-029d1c30` 已完成 VM/载荷回收，但 UAC broker 未捕获精确提升进程，故不记为应用验收通过；主控/节点测试和 service DNS 顺序测试均通过。

- Prepared the WSL Debian and native Windows Rust/Tauri build toolchains.
- Implemented the Rust/Axum control plane and dual-stack development allocator.
- Implemented Tauri 2 + Vue 3 UI, automatic sibling service startup, and Windows Named Pipe IPC.
- Implemented user-triggered real WireGuard flow: local key generation, public-key registration, WSL gateway peer update, Windows tunnel installation, handshake validation, dual-stack addresses, and cleanup.
- Added peer removal on disconnect, failed install/handshake, and admin revoke; revoke is authorized by the registering public key.
- Reused the same user's lease across key rotation while reserving revoked leases to prevent address collisions.
- Added `/admin`, device listing, revoke API, heartbeat, and repeatable WSL real-lab scripts.
- Fixed the Windows startup path: Common Controls v6 and `requireAdministrator` remain in the manifest, production binaries embed Vue assets, and the sidecar uses `CREATE_NO_WINDOW`.
- Fixed Named Pipe error 231 and elevated-service access by removing readiness probes and applying an authenticated-user ACL to each pipe instance.
- Moved blocking IPC and WireGuard waits to Tauri's blocking pool. The WireGuard service startup race now waits for the asynchronously created interface before validating the current tunnel public key.
- Updated verification to require the current peer's non-zero handshake and encrypted traffic plus an IPv4 TCP probe. ICMP is diagnostic only.
- Added heartbeat-driven remote revocation: WebUI revoke removes the gateway peer, the client uninstalls its local tunnel on the next heartbeat, and the UI reports the revoked state.
- Added `PMMAN_CONTROL_PLANE` as an optional service setting; the local lab continues to default to `http://127.0.0.1:8080`.
- Added development Bearer Token login, admin user management, enable/disable revocation, password reset, and configurable dual-stack address pools.
- Replaced the anonymous admin page with an authenticated management UI for accounts, pools, leases, and device revoke; fixed async form reset handling.
- Heartbeats now treat both unauthorized accounts (`401`) and revoked devices (`404`) as remote revocation and remove the local tunnel.
- Split the backend into a 1:N master/node model: the master owns accounts, authentication, pools, leases, node health, and admin APIs; nodes own client-facing APIs and local WireGuard peers.
- Added automatic lowest-latency node selection and manual node selection in the Windows client, with node-aware registration, heartbeat, revoke, and status display.
- Added node heartbeat reconciliation for active peers, so restarting a node restores active local WireGuard peers; replaced client public keys also remove the old local peer.
- Added node API regression tests and made the real-lab startup wait for both nodes to register with the master, not just bind their HTTP ports.
- Made WireGuard tunnel cleanup idempotent even when a stopped old tunnel service has no visible interface; the client now always requests uninstall and accepts the Windows "service does not exist" response before a fresh install.
- Added node return-route synchronization: configurable `PMMAN_ROUTE_TABLE`/`PMMAN_IP_CLI` installs and removes client IPv4/IPv6 host routes alongside local WireGuard peers, including active-peer recovery after node restart and route cleanup after revoke.

## Verification

- `cargo test --manifest-path server/Cargo.toml`: 5 passed.
- `cargo test --manifest-path service/Cargo.toml`: 1 passed.
- Vue type check and production build: passed.
- Windows MSVC release build of the service and Tauri client: passed.
- Standalone artifact: `artifacts/standalone/pmman-client.exe` and `pmman-network-service.exe`; no `localhost:1420` development URL.
- Fresh real-lab connection: `state=connected`, `tunnelMode=wireguard`, IPv4 `10.88.0.10/32`, IPv6 `fd88:1000::a/128`, configuration version 2.
- Second connection reused the same IPv4/IPv6 lease and advanced the configuration version to 3. Windows and WSL reported the same newly generated client public key, non-zero handshake timestamps, and non-zero encrypted transfer counters.
- Release client build after authentication/admin changes passed; standalone was rebuilt and launched without a console window. Admin UI login and account creation were exercised in the browser.
- Disabling the connected `pmman` account through the admin UI changed the device to `revoked`; the next heartbeat reported `连接已被服务端撤销，WireGuard 隧道已清理` and `wg show pmman-client` reported no interface. Re-enabling and reconnecting reused `10.88.0.10` and advanced the configuration version to 4.
- IPv4 TCP traffic to the disposable gateway HTTP service returned `200`.
- A third connection was revoked through the admin API (`204`), removed from the WSL peer list, and then disconnected through the client IPC. The Windows WireGuard tunnel service and WSL client peer were both absent afterward.
- Latest build: WebUI/API revoke was tested while the client remained connected; after one heartbeat the Windows tunnel service and WSL peer were absent, and IPC status reported `连接已被服务端撤销，WireGuard 隧道已清理`.
- Node split verification: master discovered `node-a` and `node-b`; automatic selection connected through the lowest-latency node, manual selection connected through `node-b`, and a second run switched back to `node-a` with the same dual-stack lease and fresh handshake.
- Master/node API tests: 9 server tests passed (7 control-plane tests and 2 node route-validation tests), including active-peer reconciliation, node-scoped registration/heartbeat/revoke, and old-key cleanup metadata.
- Final clean-artifact acceptance: first connection after a fresh lab/client start succeeded through automatically selected `node-b`, with `config_version=1`, non-zero Windows/WSL handshake and TCP `200`; final disconnect removed the Windows interface and WSL peer and marked the master device `revoked`.
- Route-sync acceptance is now part of the verifier: active client routes are required in WSL table `51820`, and the lab stop path flushes only that configured lab table.
- Remote revoke acceptance: the admin API returned `204`; within the node heartbeat interval the client changed to `disconnected`, removed the Windows tunnel, removed both WSL return routes, and reported `连接已被服务端撤销，WireGuard 隧道已清理`.
- IPv4 and IPv6 ICMP timed out in this WSL firewall configuration; the result is recorded as a warning and does not invalidate the WireGuard/TCP result.

- Traffic mode verification passed after a fresh lab restart: global status reported 0.0.0.0/1, 128.0.0.0/1, ::/1, and 8000::/1; split status reported the IPv4 and IPv6 cnipgen style fixtures plus 10.88.0.1 and fd88:1000::1 DNS. Both runs completed a real handshake and disconnected cleanly.

## Manual Acceptance

1. For first deployment run `scripts/deploy-txgz.ps1 -Version <version>`; for later releases run the separate local build and artifact-only publish scripts in `docs/RELEASES.md`.
2. Double-click `artifacts/standalone/pmman-client.exe` and approve the normal UAC prompt.
3. Click `连接内部网络`; wait for `已连接` and the assigned dual-stack addresses.
4. Close the client, then run `scripts/verify-txgz.ps1`; it requests UAC elevation automatically and launches the real service for acceptance.
5. Click `断开连接` and confirm the tunnel service and WSL peer disappear.
6. Keep the public demo running; WSL lab scripts are historical diagnostic tools.

## Current Scope

- Control-plane registry and leases are in memory for development only.
- Production password storage/authentication, persistent storage, service SCM registration, signed installer, and forwarding to real internal networks are not implemented.
- The historical mirrored-WSL lab uses a transparent UDP relay at `127.0.0.1:51821`; current public acceptance connects directly to txgz's reachable WireGuard endpoints.
## 按需 P2P 开发 — 2026-10-05

- 已开始实现主控授权目录、节点认证 UDP relay、客户端按需传输层及默认开启的在线开关。方案与完整准出要求记录在 `docs/P2P-DESIGN.md`。
- 复用现有 WireGuard 加密/驱动，客户端只与 HTTPS 主控交换身份及 peer 信息，节点无 HTTP API。中转端只持有独立传输授权，不持有用户 bearer token 或客户端 WireGuard 私钥。
- 客户端使用本地 peer UDP 端点捕获实际隧道发包需求；首包中转并进行双向探测，直连包缺失确认后经节点补发同一密文。业务活动与探测保活分开，避免探测阻止空闲回收。
- 当前仅原有主控/节点回归与初步客户端状态测试通过，前端类型检查/生产构建通过。尚未完成多虚拟机真实 P2P、持续传输、失效恢复和安装器验收；功能未准出。
- 按最新指令改用 Hyper-V SYSTEM broker 隔离待测 Windows 程序，禁止将测试失败回退为物理主机执行。计划以三个同时运行的客户端执行真实公网主控/节点验收。
- 2026-10-05：两组真实安装器 Hyper-V 验收完成隔离执行和清理，但节点 UDP 探测失败，未到 P2P 验收阶段。独立诊断发现 VM 公网 DNS UDP 成功、节点 51840 UDP 超时，同期服务器 eth0 抓包为零；已委派独立代理审查 harness 定向网络限制。不能记为产品通过。
- 进一步审查修复主控目录/节点同步锁顺序，补充 Windows peer /32、/128 实际系统路由（`wg set` 本身不会创建系统路由）、网络接口周期刷新、固定内存反重放窗口和随 RTT 调整的断路补发。增强验收为持续超过空闲期限的同一 TCP、已建立 TCP 的直连阻断与显式双端结束同步；继续等待真实验收。
- 当前发布候选安装器：`src-tauri/target/release/bundle/nsis/Pmman Network_0.1.0_x64-setup.exe`，SHA-256 `A0106336595F691137DFAF3B57729C251DCC7BAC738581E0E7A066BE2C5E6395`；其服务 sidecar 已包含隐藏子进程、周期网络接口刷新、P2P 探测失败时 relay 降级和 peer 主机路由修复。Hyper-V 三客户端请求 `12cf216c2685444a93846b9c1ab0278d` 因 broker 当前 `PoolMaxWorkers=2` 按契约取消，未产生产品验收结论。
- Hyper-V skill 代理完成了两份 runtime reference 与 harness source reference 的一致性维护，并新增受控 UDP 诊断脚本；未修改 broker 池、项目代码或远程主机。上一轮诊断证明 PrivateNat guest pktmon 可见 51840/51841 出站包，但 txgz `eth0` 未见包；当时 broker 因 `windows11-harness-base.vhdx` 被占用而处于 Maintenance，`PoolMaxWorkers=2`、无 ready worker，三客户端计划尚未应用。该限制和失败证据不能转化为产品通过。
- `scripts/run-p2p-hyperv.ps1` 现会在创建测试账号/提交请求前读取 broker 健康状态和 `PoolMaxWorkers`，不足以容纳客户端组时直接失败，避免形成永远排队的假测试。
- 2026-10-05 PrivateNat 复核：当前 broker 为 `PoolMaxWorkers=3`、3 个 ready worker；请求证据显示 guest 使用 `172.30.250.0/24`、默认路由 `172.30.250.1`、DNS `1.1.1.1/1.0.0.1`，扩展 ACL 及边界/清理证明均通过。txgz `eth0` 上节点 `51840/51841` 正在监听，未发现端口级拒绝；现有 UDP 失败仍定位在 PrivateNat 到公网节点的出口路径，不能记为产品通过。
- 测试环境已收紧为只接受 `PrivateNat`；独立诊断同时覆盖两个节点探测端口和两个 DNS UDP 端点，并分别记录目标、源端口、发送时间、响应时间和结果。安装器验收改为使用主控返回的实际 `probeEndpoint`，避免用固定节点 A 端口替代所选节点的真实探测。一次新的单 VM 诊断在 broker 启动/清理证据完整的情况下于 guest 应用证据阶段超时，未产生新的应用通过结论。

# Admin panel review and node configuration

- Reworked admin forms and tables to prevent account authorization controls and timestamps from overlapping; added responsive grid layouts and explicit horizontal scrolling for wide data tables.
- Added master API and admin controls for a node's return-route table and client-selection enablement. Route table names are validated before they reach node `ip` command arguments.
- Master-owned node settings survive node re-registration and are returned on heartbeat. Nodes apply new route-table settings dynamically, remove routes from the previous table, and reconcile active client routes in the configured table.
- Added regression tests for admin authorization, route-table validation, persistence across node re-registration, and heartbeat synchronization.
- Validation: server tests pass in WSL (10 control-plane tests and 3 node tests); Vue type-check and production build pass in WSL.

## Public txgz deployment — 2026-10-04

- Current and future master/node development-test runtime moved to `ssh txgz`, isolated under `/opt/pmman-network-dev`. Persisted this policy in `AGENTS.md`.
- Per the local-build requirement, Linux tests/releases run in local WSL with `x86_64-unknown-linux-musl`; Windows service/client use local MSVC. Remote host only receives binaries and runs systemd services. Static PIE binaries' SHA256 hashes match local build outputs.
- Master/admin: `https://pmman.tech:8443/admin`. Two distinct node HTTPS endpoints on ports 8444/8445, WireGuard UDP 51830/51831, interfaces `pmman-dev-a`/`pmman-dev-b`, return tables 51830/51831. Existing host WireGuard/BIRD/web services preserved.
- Added repeatable project-scoped provisioning, atomic binary publication, public readiness polling, random credentials, node registration token whitelist, reboot-restored project NAT, and project-only old peer/route cleanup when deploying the in-memory master.
- Standalone `pmman-client.json` now points to the public master; the real Windows service loads configuration beside its executable, with environment override and explicit malformed-file errors. Packaging includes this configuration file.
- Local Linux tests: 10 master and 3 node tests passed. Local Windows service test and MSVC release build passed. Latest deployment readiness check passed with two healthy nodes.
- Actual Windows public acceptance passed for automatic lowest-latency selection, manual TXGZ-A and TXGZ-B, split/split and global/global. Every connection matched Windows/master/node public keys, fresh per-side handshake timestamps and both IPv4/IPv6 return routes; dual-stack HTTP gateway traffic succeeded. Gateway DNS and global IPv4 HTTPS forwarding passed. Every disconnect removed the Windows tunnel service and both remote nodes' peer/route state.
- The verifier accounts for differing host clocks, pins SSH to the public IP while retaining the original hostname's host-key verification, and avoids checking a wrapped PowerShell array as one node. DNS probes use cloud DNS with upstream fallback.
- Administrator remote-revoke acceptance also passed: the master revoked the connected device, the client's normal heartbeat changed status to disconnected, and Windows service plus both remote nodes' peers/return routes were absent. SSH child processes now close standard input, have a hard timeout, and retry only stalled read-only observations. Old transcripts are timestamp-archived.
- Playwright public admin login succeeded; two healthy nodes and the return-route-table configuration entry are present. Desktop and narrow-viewport screenshots saved under `output/playwright/txgz-admin-*.png`. The initial logged-in browser check had zero console errors; deployment resets sessions, requiring login again.
- Acceptance logs: `artifacts/verify-txgz.log`; deployment metadata and credentials remain in ignored `artifacts/`. No secrets copied into documentation.
- Final combined acceptance completed at 20:32 on 2026-10-04 with `public_wireguard_verification=passed`: automatic, manual A/B, split, global, both IPv4/IPv6 gateway DNS servers, IPv4 HTTPS forwarding, and administrator revoke all passed in one run, with cleanup after every connection. Release executable has no `localhost:1420` and includes Common Controls and `requireAdministrator` manifest markers.
- Limits: both nodes share one physical public host; registry/settings are in memory; split routes are fixtures; IPv6 tunnel traffic passed but IPv6 public internet egress has not been accepted. Current acceptance drives the real service through the same IPC as the UI; native GUI automation was unavailable, so no new GUI button-click result is claimed.

## Standard admin web rewrite — 2026-10-04

- Replaced the hand-written `server/src/admin.html` with the independent `admin/` application: Vue 3, strict TypeScript, Element Plus (Chinese locale and icons), Vue Router, Pinia and Axios. Preserved all existing account, capability, password, node, lease-revoke and address-pool APIs.
- Added a reusable sidebar/header/breadcrumb layout, overview statistics from actual API data, five business routes, a login page and authenticated 404 route. Lists use search/filter controls, stable row keys, fixed action columns, empty states and pagination. Forms use validated dialogs and drawers; destructive actions use library confirmation dialogs.
- Centralized typed API requests, error mapping and timeouts. Session expiry/401 clears data and returns to login; ordinary accounts are rejected. Refreshes use a request generation guard to prevent stale data or old sessions overwriting new state. Editing drafts are independent of refreshed data; address-pool drafts survive refresh.
- Added ESLint 10 with Vue/TypeScript recommended rules, Prettier and vue-tsc. Production build enforces formatting, zero lint warnings and type checking. Standards are recorded in `docs/ADMIN-DEVELOPMENT.md` and referenced by `AGENTS.md`.
- Admin assets are built on local WSL, embedded in the static musl master with include_dir and served under `/admin/`. `server/build.rs` tracks frontend output changes. Hashed assets receive immutable caching, HTML receives no-cache, and missing assets return 404. Deployment and historical lab build scripts now build the admin frontend first.
- Validation passed: frontend formatting/lint/type check and release build; 12 master + 3 node tests, including new embedded-resource MIME/cache and missing-file tests; local static release and txgz deployment readiness with both healthy nodes.
- Public Playwright acceptance passed for empty login validation, ordinary-user rejection, administrator login, overview data, user search/empty state/reset, real user creation with IPv4 global capability, capability editing with confirmation, disabling the test account, node route-table validation and real save, address-pool validation/draft preservation/reset and real save, browser-local injected 503 recovery, authenticated 404, and 390px navigation/table layout.
- Logout clears the stored session and protected routes return to login. The final accessibility adjustment gives every sidebar menu item an explicit readable label; its local build and txgz deployment also passed. A frontend-only rebuild regenerated asset hashes and recompiled the Rust master, verifying the asset change tracking.
- Final deployed browser verification passed with all five navigation labels, real overview node data, and zero console errors/warnings in a clean session. Remote master SHA256 matches the local release artifact; master and both nodes remain active. Final screenshot: `output/playwright/admin-v2-overview-final.png`.
- Screenshots are saved under `output/playwright/admin-v2-*.png`; no passwords are included in deliverable screenshots. Removed one credential-bearing temporary tool snapshot and avoided snapshots while password inputs are populated thereafter.
- Current limitations: list filtering/pagination is client-side because APIs return full lists; the control plane remains an in-memory development service. This change does not implement persistence or production authentication, and does not modify the Windows client/WireGuard connection protocol.

## 节点接入与保留地址修复 — 2026-10-04

- 新增 Element Plus「接入节点」表单、专属凭据一次性展示、systemd 环境配置下载、待接入状态和取消确认；列表与概览区分待接入和在线。配置弹窗补充 WireGuard endpoint。
- 主控必须先由管理员预建节点，再以绑定 ID 的凭据注册。删除旧的开放注册/未绑定 token 白名单路径，校验节点 ID、HTTPS 根地址、UDP endpoint、WireGuard 公钥和路由表；取消待接入记录立即作废凭据，已注册节点仍通过停用撤销连接。
- `PMMAN_NODE_STORE` 持久化节点接入记录和管理员节点设置，Linux 文件权限 600，先同步临时文件再原子替换；保存失败不提交内存变更。重启恢复记录但重置在线证据，首次心跳才可进入客户端候选列表。节点注册/心跳请求有 10 秒上限，心跳失败后尝试重新注册。
- txgz 部署改用权限 600 的、按 ID 绑定的 bootstrap 配置，保留已有记录优先；部署就绪检查兼容后续增加节点及管理员主动停用。历史 WSL 脚本同步使用随机凭据和 bootstrap，但本轮运行环境仍为 txgz。
- 地址分配、租约复用、池变更校验和容量统计统一排除 IPv4 网络/广播地址及首个主机网关地址；IPv6 无广播，双栈使用相同偏移并保留 0/1。默认 /24 客户端容量 253，/30 仅一个客户端地址；/31、/32 被拒绝。已撤销异常旧租约重新分配，活动异常租约要求先撤销。
- 本地前端 Prettier/ESLint/vue-tsc/生产构建通过；本地 WSL musl Cargo tests 16 项主控 + 3 项节点全部通过，覆盖地址池穷尽、保留地址、池大小、旧租约、权限、跨 ID 凭据、取消和存储恢复。musl release 已部署；本地/远端主控及节点 SHA256 一致。
- 真实 txgz 节点进程验收：管理员预建专属凭据、拒绝错误凭据/跨 ID、节点注册和真实心跳、待接入与已注册节点的主控重启恢复、取消作废均通过。可重复脚本 `scripts/txgz/verify_onboarding.py` 仅创建自身禁用验收节点，结束停止进程并清理自身记录，不操作真实节点 peer/路由；运行时会重启项目主控，应在无活动连接时执行。
- 公网页面 Playwright 验收通过：必填校验、实际创建待接入节点、下载配置内容校验、取消确认/记录移除、/31 表单拒绝、桌面与 390px 弹窗/导航。浏览器控制台零错误/警告。截图 `output/playwright/node-onboarding-*.png`、`address-pool-reserved-validation.png`。
- Windows 全套真实公网回归 `artifacts/verify-txgz.log` 通过（2026-10-04 21:53:42）：自动最低延迟、手动 A/B、split/global、每次新公钥和双方新握手、双栈 HTTP 网关、双栈网关 DNS、IPv4 全局 HTTPS、所有断开清理及管理员撤销。结束无活动设备，两个正式节点在线，两个网关接口无 peer。
- 凭据下载仅保留在忽略目录 `artifacts/`，移除工具额外生成的下载副本并忽略 `.playwright-cli/`；最终扫描该工具目录未发现已知凭据。验收节点均已清理，用户业务记录未删除。
- 开发与部署指引落盘 `docs/NODE-ONBOARDING.md`，更新 README、PUBLIC-DEMO 和 ADMIN-DEVELOPMENT。节点接入已持久化；用户、会话、设备租约和地址池仍为 demo 内存模型，IPv6 公网出口与跨主机故障隔离尚未验收。

## 主控独占客户端控制面与 UDP 节点探测 — 2026-10-04

- 客户端登录、节点列表、连接配置、心跳与撤销全部直连 HTTPS 主控；删除节点 HTTP 服务及旧节点代理 API，不再向节点发送用户 Bearer。节点主动向主控注册并每秒同步期望 peer、安装确认、错误和清理结果。
- 新增 `POST /v1/client/connections`。主控负责能力授权、双栈地址及路由/DNS 配置生成，等待设备 ID、公钥、配置版本和返回表精确安装确认后才返回配置，最长 8 秒。每次连接递增版本，避免旧 ACK 误放行；失败/超时撤销该版本，断开等待新清理确认（204 已确认、202 已排队）。
- 节点仅监听 WireGuard UDP 和独立 UDP challenge echo。24 字节固定包包含协议标识及随机 nonce，不放大回包；客户端校验来源、长度和 nonce，每节点三次、每次 400ms 超时，并发最多 16 节点，取成功样本中位 RTT 自动选择最低延迟节点，不使用 HTTP 降级。
- 节点接口必须独占；同步同时核对内存及内核 peer，清除重启残留和旧公钥，安装双栈主机返回路由。接入验收改用自身临时 WireGuard 接口，避免触碰正式节点；旧 WSL 共享接口/HTTP 实验脚本停用。
- 管理后台接入表单、下载配置、节点表格改为 WireGuard/UDP 探测地址，移除节点 HTTPS 地址、证书和反向代理要求。新环境变量为 `PMMAN_PROBE_ENDPOINT`/`PMMAN_PROBE_LISTEN`，旧 `PMMAN_NODE_ENDPOINT`/`PMMAN_NODE_LISTEN` 退役。
- 本地验证：19 项主控、4 项节点、3 项 Windows 服务测试通过，覆盖安装版本匹配、安装错误/超时撤销、路由策略、UDP 长度/nonce 校验及有限等待。管理后台格式/lint/类型检查/生产构建、客户端 Vue 检查、Windows MSVC 与 Linux musl release 通过；均在本地构建。
- txgz 新版已部署，主控及两个节点 active；WireGuard UDP 51830/51831、探测 UDP 51840/51841 正常监听，旧节点 TCP 18090/18091 和 HTTPS 8444/8445 无监听。仅主控保留 `https://pmman.tech:8443`。
- 真实接入脚本通过注册/心跳、凭据绑定、取消和主控重启恢复，结束清理自身接口及记录。Playwright 公网页面通过 UDP 端口范围校验、实际创建、下载新环境配置及取消；桌面/390px 截图 `output/playwright/node-udp-*.png`，控制台零错误/警告，额外凭据下载副本已移除。
- 首轮真实 Windows 公网回归于 22:34:38 完成：UDP 自动最低延迟、手动 A/B、split/global、双方本次公钥和新握手、双栈网关业务 HTTP、双栈 DNS、IPv4 全局 HTTPS、断开清理及管理员撤销全部通过。HTTP 验收为隧道内业务流量，不是节点控制 API。日志 `artifacts/verify-txgz.log`，GUI 按钮未新增自动化验收声明。
- 最后补齐主控登录/节点列表 12 秒请求超时并修正公网握手故障提示，重新测试、编译并打包 Windows 产物。最终 standalone 全套真实公网回归于 22:40:07 完成，`public_wireguard_verification=passed`；自动、手动 A/B、split/global、双栈访问/DNS、IPv4 出口和管理员撤销全部通过，每轮断开清理通过，结束无残留 peer/路由/Windows 隧道服务。
- 发布产物 `artifacts/standalone/pmman-client.exe` 及相邻后台服务、配置已更新。检查不含 `localhost:1420`、保留 Common Controls v6 与 `requireAdministrator`；打包服务与 release、远端主控/节点与本地 musl 的 SHA256 均一致。工具目录已知凭据扫描无匹配，下载测试节点已取消，仅保留忽略目录中的配置。
- 当前限制仍为 demo 内存账户/会话/租约、同主机双节点、夹具分流表；IPv6 公网出口和跨主机故障隔离尚未验收。

## Windows 客户端现代化前端 — 2026-10-05

- 重写 `src/App.vue`：登录欢迎页、网络连接主屏、连接节点页、偏好设置页、连接详情和账户弹窗，改为侧栏/工作区布局与单一主连接操作。首页只呈现状态、节点、流量策略和双栈地址，详细路由/DNS/服务消息收进连接详情。
- 新增 `src/composables/useClient.ts` 与 `src/types/client.ts`，集中处理真实 IPC 状态、请求互斥、连接计时、错误恢复、远程撤销和轮询版本保护；连接成功仍严格要求 `connected + wireguard`，没有模拟成功路径。
- 新增 `src/composables/useTheme.ts` 与 `src/styles/client.css`：浅色/深色/跟随系统、Windows 系统字体、统一 token、键盘焦点、可读标签、原生 modal dialog、reduced-motion、小窗口响应式和 390px 无横向溢出。
- 连接节点页支持自动 UDP 最低延迟、手动选择、刷新探测、不可用状态和空状态；设置页只显示主控授权的 IPv4/IPv6 模式，连接期间锁定网络设置，失败时恢复真实选择。
- 更新 Tauri 默认窗口为 1040×760，最小 760×620；更新标题、语言、favicon 和 PMMan 图标资源。发布包仍保留无黑窗口、UAC 和后台服务模型。
- 本地 WSL `pnpm build`、Windows MSVC Tauri release 编译和 standalone 打包通过。发布二进制不含 `localhost:1420`、`__PMMAN_UI_FIXTURE__` 或测试调试内容；`scripts/ui/client-fixture.js` 仅供浏览器注入验收，不被应用导入或打包。
- Playwright UI 验收通过：登录/导航、节点自动与手动选择、UDP 刷新、空节点恢复、授权模式过滤、深色主题、主题持久化、详情 Escape、连接失败按钮恢复、重复点击防护、旧轮询不覆盖连接、远程撤销提示、断开状态和 760/390px 响应式无横向溢出；截图 `output/playwright/client-v2-*.png`，控制台零错误/警告。
- UI 夹具不代表真实网络连接；前端未改变服务协议。最终交付包使用新生成的 `artifacts/standalone/pmman-client.exe` 于 23:26:12 完成真实公网回归：自动/手动 A/B、split/global、双方新握手、双栈网关 HTTP/DNS、IPv4 HTTPS 出口、断开清理及管理员撤销全部通过。

## Windows 零准备交付 — 2026-10-05

- NSIS per-machine 安装器已接入 WireGuard for Windows MSI 和 WebView2 Evergreen Standalone x64 离线安装器，依赖包下载前固定 SHA256，安装器完成依赖安装后才允许用户启动客户端。
- WireGuard 驱动不在客户端卸载时删除，以避免破坏其他隧道；`scripts/verify-windows-prerequisites.ps1` 用于安装后核对 `wg.exe`、`wireguard.exe` 和 WebView2 Runtime。
- `artifacts/standalone` 明确为已有运行时的开发验收目录包；零准备 Windows 必须验收 `scripts/build-windows-installer.ps1` 生成的 NSIS 安装器。
- 为支持真正的零准备 Windows，Windows MSVC 服务和客户端构建显式启用 `-C target-feature=+crt-static`，避免新系统缺少 Visual C++ Redistributable 时后台服务出现系统错误；`scripts/build-client.ps1` 固定注入该参数，并保留 `.cargo/config.toml` 的 target 级默认值。
- 上一版安装器已更新到 `artifacts/windows-installer/PMMan-Network-Client-Setup.exe`，SHA256 为 `6E1A6501489EC578BAE0A110446DA137790925F1F884DA1FDFD9464D110D5F4B`；本轮离线 WebView2 版本重新生成同名安装器，新的 SHA256 为 `16B099FE116C58E73330A15B005D3CC2B85B4787CA4B9C296E934E4C1927046B`，大小约 209.96 MiB。构建阶段通过 MSVC、前端生产构建、WebView2/ WireGuard 依赖哈希校验和 NSIS 打包。
- WireGuard 改为 PMMan 内部运行时：安装器只把 WireGuard 驱动/命令组件作为 PMMan 依赖，删除新安装产生的 WireGuard 快捷方式和卸载列表入口；PMMan 后台服务仍负责临时隧道的安装、握手校验、心跳撤销和断开清理。
- Windows Sandbox 零准备实机验收通过：`artifacts/windows-sandbox/acceptance.json` 为 `state=passed` 且 `userFacingEntryOnly=true`，PMMan 是唯一用户入口；安装器退出码 0，WireGuard/WebView2/主控配置就绪，后台服务 IPC 与登录成功；真实 WireGuard 模式连接并完成本次握手，分配 `10.88.0.10/32` 和 `fd88:1000::a/128`，IPv4/IPv6 网关探测均通过，断开后 Windows 隧道清理通过。脱敏过程日志保存在 `artifacts/windows-sandbox/acceptance.log`。
- 验收脚本记录本轮新建的 Sandbox ID，并在成功/失败路径通过 `wsb stop --id` 清理，避免遗留虚拟机导致后续测试重叠；脚本 PowerShell 5.1 语法检查通过。

## WebView2 完全离线安装 — 2026-10-05

- 将安装包内的约 1.85 MB WebView2 Bootstrapper 替换为微软官方 Evergreen Standalone x64 安装器 `MicrosoftEdgeWebView2RuntimeInstallerX64.exe`（212,272,848 bytes）。安装钩子仍只在检测不到 WebView2 Runtime 时执行 `/silent /install`，但不再依赖安装阶段联网下载 Runtime。
- 下载入口使用官方 `LinkId=2124701`，构建脚本固定 SHA256：`F6DF8E4BC857786FF641CD01DA1449169EAF8236C936CED485BA4DA40`；哈希不匹配时构建失败。WebView2 安装完成后临时安装器会从应用目录删除，PMMan 仍是用户唯一可见入口。
- 公网主控、节点 UDP 探测和真实 WireGuard 连接仍需网络；“完全离线”仅指 WebView2 依赖安装，不改变业务网络需求。
- 已完成本地 MSVC release、NSIS 打包和 Windows Sandbox 回归。最终安装器 `artifacts/windows-installer/PMMan-Network-Client-Setup.exe` SHA256 为 `16B099FE116C58E73330A15B005D3CC2B85B4787CA4B9C296E934E4C1927046B`，Sandbox 安装退出码为 0；WebView2、WireGuard、IPC、登录、真实双栈握手、`10.88.0.1`/`fd88:1000::1` 网关访问及断开清理全部通过。结果保存在 `artifacts/windows-sandbox/acceptance.json`，结束时 `wsb list --raw` 为 0 个运行实例。
- 用户机器反馈后的首轮修复纠正了 Runtime GUID，但新增的 NSIS 跨目录通配检查不受支持，导致已安装 Runtime 被错误拒绝并弹窗；静默测试因此等待弹窗直至超时。提高超时不是该错误的解决方案，先前对时长和根因的判断过早。旧验收仅检验文件目录和后台 IPC，不能证明客户端 WebView 可加载。
- `pmman-client.exe --check-webview2` 直接调用 Tauri 自身加载器，仅用于客户端启动自检和 Sandbox 验收；NSIS 不再用 Tauri 进程或跨目录通配路径阻断安装。Sandbox 增加加载器成功、客户端窗口及其 WebView 子进程检查。
- Sandbox 诊断发现 WebView2 实际目录为 `154.0.4258.53`，EdgeUpdate 注册表 `pv` 为 `154.0.4258.48`，因此系统默认加载器按注册表解析失败。客户端现在在 Tauri 初始化前扫描实际 Runtime 可执行文件并设置 `WEBVIEW2_BROWSER_EXECUTABLE_FOLDER`，避免补丁版本错位导致“找不到 WebView2 Runtime”。
- 最终安装器 SHA256 为 `E0099ADF2461751A9D0D14201E966CC1E79D41601E2571A617125564246D0FB5`。最新 Sandbox 结果为 `state=passed`、`installerExitCode=0`、`webviewLoader=true`、`uiWindow=true`、真实双栈 WireGuard 连接/握手/网关访问/断开清理通过；即使复现注册表 `pv` 与实际目录错位，PMMan UI 也能正常启动。`wsb list --raw` 为 0 个运行实例。
-
## 按需 P2P 客户端闭环与 Hyper-V 公网验收 — 2026-10-05

- 服务端/客户端已落地按需 P2P：首包触发、UDP 直连优先、认证节点 relay 回退、断路补发与恢复、活跃 TCP/UDP 连接不重建、空闲回收、双栈 peer 路由、P2P 默认开启且可持久化关闭；服务与协议测试通过。
- 客户端节点选择继续优先使用 UDP challenge/ICMP RTT；新增严格回退：当所有探测协议都被网络策略丢弃时，保留主控最近 heartbeat 的健康节点并继续尝试 WireGuard，不伪造 `latencyMs`。有任一有效探测时仍只选择测得节点。
- 本地 Windows MSVC：service 8 项单元测试全部通过；WSL musl 主控/节点 24 项测试全部通过；前端类型检查、生产构建、Tauri release 和 NSIS 打包通过。最新零准备安装器为 `src-tauri/target/release/bundle/nsis/Pmman Network_0.1.0_x64-setup.exe`，SHA256 `022E6B8DDDDF27712B8012DC55DB5B66C7A50E138650169C5481D61E8562DF20`。
- Hyper-V SYSTEM broker 三客户端组 `0ff77806afc1453991b6596894300098` 已真实执行安装器、UAC、后台服务 IPC、主控登录和隧道安装；broker `HarnessSucceeded=true`、VM 清理/载荷清理/网络租约清理通过，但应用断言在公网 WireGuard 握手阶段失败：PrivateNat guest 到 txgz `1.14.226.49:51830/51831` 及 UDP 探测 `51840/51841` 均无响应，WireGuard handshake 为 0；同一环境到公网 DNS UDP 成功，物理主机直接访问节点探测 UDP 成功，远端节点监听正常。证据目录为 `D:\VMs\Codex-Harness\Live\Broker\Results\executable-test-20261005T004951755Z-96e378f6` 等三成员目录。
- 因此本轮不能把多客户端 P2P Hyper-V 公网闭环标记为准出；已复现 Hyper-V PrivateNat 到该公网 UDP 目的端口的无响应，具体丢包位置尚未确定，不能用 TrustedLan、物理机运行或 loopback relay 替代真实验收。测试侧新增 `scripts/p2p/udp-diagnostic.ps1`，分别记录节点 UDP 与 DNS UDP，`scripts/run-p2p-hyperv.ps1` 固定只允许 PrivateNat，避免绕过隔离掩盖问题。

### Hyper-V PrivateNat 出口继续定位 — 2026-10-05

- 单客户端 `NetworkProof.exe` 通过 SYSTEM broker 在 `PrivateNat` 中发送 txgz 探测；guest 获得独立地址 `172.30.250.81`，DNS UDP 会话正常，但 `1.14.226.49:51840/51841` 仍无响应。WinNAT 会话取证显示该 guest 存在 UDP `1.1.1.1:53`/`1.0.0.1:53` 会话，没有 `1.14.226.49` 会话。
- 同步远端只读 `tcpdump` 未观察到本轮四个 txgz UDP 端口的入站包；远端 `ss -lun` 确认 `51830/51831/51840/51841` 正在监听。这只能确定请求没有到达节点，不能单凭远端未收到包区分宿主出口与上游丢包。
- harness 自带计划指纹绑定的 `Trace-RequestUdp.ps1` 已完成 120 秒 PktMon 取证：无事件丢失，但目标 UDP 过滤器没有产生可解析事件；结果目录为 `D:\VMs\Codex-Harness\Diagnostics\UdpTrace-20261005T022055587Z`。
- 宿主机只读网络盘点发现 Kaspersky `KL_KLIM6` NDIS 过滤器仍对 `vEthernet (Codex Test NAT)` 处于 `Enabled=true`，`avp` 进程及相关驱动仍在运行；关闭界面不等于移除该数据面过滤器。Hyper-V VFP 扩展原为 `Running=true/Enabled=false`。
- 曾按精确基础设施计划临时启用该 VFP 扩展；broker 随即在设置 PrivateNat PVLAN 时失败并返回 Hyper-V `0x80070057`，未形成可用修复。已将该扩展精确回滚为 `Enabled=false`，broker、池和清理状态恢复健康；未修改 Kaspersky。
- 当前仍阻塞真实公网 UDP/P2P 准出验收。Kaspersky 过滤器是待验证因素，尚无证据证明必须更改它；继续通过受控 harness 维护收集交换机、NAT、物理出口和 WFP 证据，在定位前不扩大网络权限或宣称验收通过。

### P2P 准出断言补齐 — 2026-10-05

- 独立诊断调用已补齐启动 UAC 接受参数；此前缺失此参数导致的取消/无结果不能用于判断 guest 脚本失败。请求 `executable-test-20261005T040039098Z-f659fac8` 正常产生结果：节点两个 UDP challenge 超时，公共 DNS 两个 UDP 请求响应成功；`HarnessSucceeded=true`、`TestPassed=false`，保留源端口和发送时间。
- IPC peer 状态新增公开的 WireGuard `publicKey`，不暴露密钥或 token。guest 验收按本轮精确公钥断言 gateway 和已访问 peer 的新握手，并检查未访问 pair 无握手，避免只统计非零行误判。
- guest 验收补齐 IPv6 UDP、echo listener readiness、TCP 控制标记跨 Read 分片处理、逐步 `progress.json`；阻断直连限定目的地址和端口，以免同地址的 relay 被一并阻断。安装阶段准确称为内置依赖安装，联网场景不声称离线安装验收。
- 三客户端 runner 增加公网主控设备与节点只读握手/路由采集、Windows/节点公钥比对和本组断开清理；创建账号失败也进入关闭测试账号的 finally 路径。
- 本地 service 8、protocol 4、WSL musl master 20/node 4 共 36 项测试通过，Vue 类型/生产构建和 MSVC release 构建通过；真实三客户端公网 P2P 尚未准出。

### P2P 中转 MTU 与断网安装回归 — 2026-10-05

- 发现原两层认证头让满尺寸 1280 字节内层 IP 包的中转 UDP 超过 1500 字节链路 MTU。中转头现在复用内层 source/destination，节省 32 字节；IPv6 UDP underlay 总长度为 1490 字节，协议标识升级为 `PMMP2P02`，需节点/客户端一起升级。拒绝嵌套 relay 和身份不匹配，保留内外层 HMAC 与 WG 加密。
- 新增最大 IPv6 包尺寸测试，protocol 5 与 service 8 项测试通过；WSL musl master 20/node 4 与 admin format/lint/type/build 通过。通过标准部署脚本更新 txgz 项目主控/节点；未修改宿主现有其他服务。guest UDP 验收改为填满 IPv4/IPv6 1280 字节隧道 MTU，覆盖分片风险。
- 断网 broker 请求 `executable-test-20261005T042006305Z-c3b8dde5` 检查安装器 SHA256 `2E725BB6CC75D51B6F989C3F1B04B9E0E3A47E8D33CD57B20C45A61518B5B881`，应用与 harness 断言均通过：WireGuard 内部运行时、实际 WebView 加载、可见 PMMan 登录窗口、IPC、默认 P2P、唯一产品卸载入口。已查看 `offline-client.png`；进程清理通过，VM 关机，payload/host input 子盘删除，网络最终全部断开，无证据警告。
- 此 guest 原有 WebView2，原无 WireGuard；本次不能证明缺失 WebView2 的离线安装路径。新增可选 VM fixture 步骤，通过微软运行时卸载器建立缺失 WebView2 环境，再验收内置依赖安装，不删除 Edge 文件、不在物理宿主执行。
- 同步节点 UDP 诊断仍复现无响应，两个 DNS UDP challenge 校验通过；最新同步请求 `executable-test-20261005T042652157Z-aa982f3a` 用 30 秒发送延时对齐受控宿主抓包，实际根因继续定位。三客户端公网 P2P 准出仍待实测。
- 缺失运行时请求 `executable-test-20261005T043026557Z-c86f1f0f` 在 fixture 前置条件失败：微软卸载器未移除镜像内置 WebView2，尚未进入安装器，不是客户端安装失败。fixture 补充在 disposable VM 内可恢复地保留/隐藏精确 Runtime 目录及 WebView2 产品登记；不删除 Edge 浏览器或更改物理宿主。
- 首包队列修复：认证直连建立时立即将未确认的中转握手包送往直连，避免中转暂不可达时等待 WG 五秒重试。实际 UDP 单元测试新增丢弃中转 DATA 的场景，首包两秒内到达；8 项 service 测试通过。
- 最新安装器 SHA256 `7E9E2C79B2E18594A2FAF82C4520DB4AFDF3D7DA7C5AA216472688ACF8697476`，包含中转 MTU 与首包修复；Windows GUI subsystem、Common Controls v6、requireAdministrator 和不含开发 localhost 检查通过。项目主控/节点远端 SHA256 与本地 musl release 一致，打包 sidecar 与 MSVC release 一致。
- 缺失 WebView2 的完全断网验收通过：请求 `executable-test-20261005T044259072Z-b1e483dc` 在安装前确认 Runtime 不可用（fixture 保留原目录/登记，并关闭占用它的 Widgets），随后 NSIS 安装退出 0，微软 Runtime 实际加载成功，PMMan 原生窗口/WebView 子进程、后台 IPC、默认 P2P 与唯一产品入口均通过。已查看 `offline-missing-runtime-client.png`，`HarnessSucceeded=true`、`TestPassed=true`，进程/载荷/host input 清理通过，VM 关机、全部适配器断开，无证据警告。
- guest A 的第二轮连接增加自动节点最低实测 UDP/ICMP 延迟断言；公网 runner 保存两轮公钥，分别检查对应节点的新握手及旧、新 peer/路由清理，避免只验证最终一轮。
- Playwright 浏览器 UI 回归通过：P2P 默认 switch 语义、请求失败保留真实开关并恢复可操作状态、pending 禁止重复键盘提交、已连接时切换保持连接状态、四种 peer 路径标签、最长 IPv6 地址在 390px 无横向溢出。截图 `output/playwright/client-p2p-gate-desktop.png` / `client-p2p-gate-mobile.png`，控制台零错误/警告；明确使用浏览器注入夹具，不作为真实 WG/P2P 网络验收。

### 会话隔离与有效 UDP 数据路径取证 — 2026-10-05

- 修复旧 heartbeat 撤销响应与断开重连的竞态：连接创建、断开、远程撤销和目录 peer/路由更新共用 session gate；后台 HTTP 返回后在持有 gate 的条件下重新检查该会话的 stop，旧会话不能卸载或改写新会话的同名隧道。新增并发迟到响应测试，Windows MSVC service 9 项测试通过。
- 直连故障验收在同一 TCP 连接中持续发送，检查中转回退、移除精确阻断规则以及恢复直连；恢复阶段不再用新建 TCP 连接替代原连接。
- 新增可信宿主编排 `scripts/p2p/run-udp-trace.ps1`：canonical broker 提交与抓包 PlanOnly/apply 在同一 PowerShell 进程内按真实提交 ID 自动绑定，避免模型/API 延迟消耗采集窗口；待测 diagnostic launcher 仍仅在 PrivateNat guest 中执行。维护仅启动有界 header/ETW 记录器，不更改 NAT、WFP、交换机或安全策略。
- 用户关闭宿主防火墙后读取到 Private/Public `Enabled=false`，Domain 仍启用。请求 `executable-test-20261005T110347449Z-a14ed8b5` 三轮节点 UDP 全部超时、六个 DNS UDP 请求全部成功，不能将此问题归因为普通 Private/Public 防火墙开启。
- 本次真实有效采集目录 `D:\VMs\Codex-Harness\Diagnostics\UdpTrace-20261005T110349161Z`，窗口 11:05:09–11:07:09 UTC，覆盖节点全部六次发送 11:06:04–11:06:34 UTC。节点包到达宿主 TCP/IP并以未转换源地址出现在 WSL mirrored/FSE 路径；未发现节点 WinNAT 会话或物理网卡出口，DNS则有 NAT 与物理网卡往返。继续关联 WFP/FLSNPI 分类，尚未证明负责拦截的具体组件，未擅改 WSL 或 Kaspersky。
- 诊断 harness 成功、应用断言失败；进程/载荷/host input/网络租约清理通过，VM 关机，最终所有适配器断开。真实三客户端公网 P2P 准出仍未完成，不用单元测试、DNS 成功或同 NAT 直连代替。
- 会话隔离修复后的 NSIS 安装器 SHA256 为 `5E3D95E42C257D4FFFF74D88966349326458065F490361C984591558A7E09E94`，delivery 副本同步到 `artifacts/windows-installer/PMMan-Network-Client-Setup.exe`。断网请求 `executable-test-20261005T111456597Z-1aea22e4` 通过安装、内部 WireGuard 运行时、实际 WebView 加载/子进程、原生窗口、后台 IPC、默认 P2P 与唯一 PMMan 入口检查；已查看 `offline-client.png`。本 guest 原有 WebView2，缺失 WebView2 仍以先前缺失运行时 fixture 的独立证据为准。进程、载荷、host input 清理全部通过，VM 关机、网络全断开，无证据警告。

### UDP 端口矩阵与节点授权租约 — 2026-10-05

- PrivateNat 请求 `executable-test-20261005T111722786Z-c719bb20` 将相同 probe 和 DNS 两种载荷分别发送到节点地址、两个 DNS 地址及交叉端口；有效采集 `D:\VMs\Codex-Harness\Diagnostics\UdpTrace-20261005T111724521Z` 的 11:18:16–11:20:16 UTC 窗口覆盖全部十二次发送。三个地址上发往 53 的六个包均建立 NAT 并到达物理 NIC；发往 51840/51841 的六个包进入 FSE NIC310，无 NAT/物理出口。本轮差异按目标端口区分，与这两种载荷及节点地址无关；交叉服务超时不是拒绝证据，具体分流组件仍需验证。
- 节点补齐独立主控授权 watchdog，30 秒无新认证快照即停用专用 WireGuard 接口、撤销 relay 并清理 peer/返回路由；失败清理保留重试，恢复先清理后启用。所有认证同步命令共享租约截止时间，单命令最多 3 秒，过期清理每批最多 3 秒；慢批次不能越过租约后恢复授权。
- WSL musl 节点 12 项测试和格式检查通过，覆盖主控请求阻塞期间过期、进程重启残留、路由失败重试、恢复顺序、内核命令限时及剩余 400ms 租约遇到八个慢安装操作。真实节点主控失联/恢复与三客户端公网 P2P 仍待实测；本节不代表功能准出。
- 标准 `scripts/deploy-txgz.ps1` 完成 admin format/lint/type/build、musl 主控 20/节点 12 测试、release 与项目部署。三服务 active、两专用接口 UP；远端/本地 master SHA256 `308352B29F7DCD817925F79CF2F3D3F152D7C7E7DAF8ADB1807310413077A6EF`、node `B95AA3792D2E31E123103EC9DC4677012B14212F5DC77D8BBE81EF482BC7CC1F` 一致，未在低内存远端编译。
- 又发现节点 heartbeat 原先只按 `active` 状态同步，客户端崩溃后其网关 key 可永久残留。主控现在对节点模式执行 60 秒客户端心跳授权租约（客户端每 10 秒续租），过期设为 revoked，保留真实 lastSeen 和地址以供重连复用；node 快照、目录、配置和 heartbeat 读取均先执行过期检查，迟到 heartbeat 不恢复旧授权。新增快照撤销、到期边界、迟到续租拒绝和新公钥复用地址测试，主控 21 项测试通过。
- 上述客户端租约修复已再次经标准部署脚本完成本地 admin 校验、musl release 和 txgz 发布；三服务恢复 active。此次使用 `-SkipTests`，因为本次 master 21 项、未变 node 12 项测试已独立通过；未改变 Windows 安装器，SHA256 仍为 `5E3D95E42C257D4FFFF74D88966349326458065F490361C984591558A7E09E94`。
- 仅临时停用 pinned NAT gateway 的 KL 绑定的对照请求 `executable-test-20261005T113004614Z-fc8ecaed` 被系统 `Access is denied` 拒绝，未生成禁用成功标记，因此没有有效 KL 因果对照。canonical 维护恢复收据确认该绑定 Enabled=true、其他绑定零漂移，owned recorders 全部停止；broker 正常清理、VM Off、载荷删除。继续沿端口分流和接口转发定位，不绕过安全组件保护、不停用无关 WSL 服务。
- 修复可信编排的 PowerShell argument-mode 布尔传参：`([bool]$switch)` 才是布尔值，未加括号的强制转换会成为 truthy 字符串。受此影响的请求 `executable-test-20261005T114114741Z-1038c01d` 在 PlanOnly guard 阶段失败，未执行网络变化；编排现会继续观察 broker 至清理完成再报告维护失败，并只输出一次该错误。
- 正确参数下的单因素接口转发对照 `executable-test-20261005T114331430Z-5edcf19f` 已实际执行：11:44:27 UTC 仅启用 pinned gateway122/compartment1 的 ActiveStore IPv4 Forwarding，11:46:28 恢复 Disabled，weak-host 状态不变、全部原接口状态零漂移。采集目录 `D:\VMs\Codex-Harness\Diagnostics\UdpTrace-20261005T114335947Z` 覆盖十二次发送 11:45:21–11:45:40；节点 UDP 仍无响应，正确 DNS 请求成功。因此单独开启该转发参数没有解决公网 UDP，未保留此变化。维护成功、broker 清理成功、VM Off、载荷删除、无证据警告；三客户端 P2P 公网准出仍未完成。

### 节点路由回归与宿主机第三客户端验收 — 2026-10-05

- 修正节点路由恢复错误匹配：只对 Linux 实际返回的 `Device for nexthop is not up` 触发专用接口临时拉起并重试，不再把字面量通配符当作正则；注册响应显式消费 body，路由删除对 `RTNETLINK answers: No such process` 保持幂等。
- 节点回归测试现在为 14 项全部通过，覆盖 down-interface 路由恢复、缺失路由清理、过期授权 fail-closed、慢内核命令和新快照恢复。
- 新增 `scripts/p2p/host-acceptance.ps1` 和 `scripts/run-p2p-hyperv.ps1 -HostThirdClient`。该模式保留 A/B 两个 Hyper-V PrivateNat guest，把 C 放在物理宿主机，以节省一个 VM 的内存；C 使用已安装 PMMan 客户端的真实 IPC、真实 WireGuard 隧道和真实 P2P 回显，只绑定自己的 `10.88.x.x`/`fd88:...` overlay 地址，不修改宿主机防火墙，也不覆盖已有登录会话。
- 实际运行组 `27b703bda3e645e6a1e5b29f619ac3f1`：宿主机 C 登录、WireGuard 双栈连接、四个 overlay-only TCP/UDP listener 均通过；两台 VM 安装器、UAC、后台 IPC 和主控登录均通过，但 A/B 到公网节点 `1.14.226.49:51830/51831` 的 WireGuard handshake 均为 0，且独立 UDP challenge `51840/51841` 超时。broker `HarnessSucceeded=true`、VM 与网络资源清理通过，应用结果失败，证据目录为 `D:\VMs\Codex-Harness\Live\Broker\Results\executable-test-20261005T142611059Z-2b6c8433` 和 `...142611212Z-24039b82`。
- 该组再次证明宿主机公网 UDP 路径可工作而 Hyper-V PrivateNat 节点端口路径不可工作；不能把失败归因于普通 Windows 防火墙（用户已关闭防火墙），也不能用同 NAT、loopback relay 或 TrustedLan 替代准出。三客户端真实公网 P2P 仍未准出，下一步阻塞点是 Hyper-V PrivateNat 到项目节点 UDP 端口的宿主出口/NAT/过滤组件定位。

### 双物理公网客户端 P2P 验收 — 2026-10-05

- 用户在另一台真实 Windows 机器安装当前构建并使用专用账户 `manual-p2p-20261005224157` 接入主控；主控记录设备 `10.88.0.14` / `fd88:1000::e`、节点 `txgz-b`，状态为 active。
- 本机使用第二个专用测试用户建立真实 WireGuard 隧道，地址为 `10.88.0.15` / `fd88:1000::f`，自动选择 `txgz-b`；两端均通过公开主控目录发现对方。
- 首次访问远端双栈地址时，客户端先经认证 relay 建立可用路径，随后真实 UDP 打洞切换为 `direct`，观察到公网直连端点 `118.253.48.14:55008`、RTT 约 21–22ms，`directTxBytes` 和 `relayTxBytes` 均非零，`fallbackCount` 记录了真实路径切换。
- 持续 IPv4/IPv6 各 20 个 ICMP 请求全部成功，IPv4 0% 丢包（平均约 35ms，单次 206ms 峰值），IPv6 0% 丢包（平均约 29ms）。
- 空闲约 35 秒后 peer 正确回收为 `idle`；再次访问先走 relay，约 10 秒内恢复为 `direct`。关闭 P2P 后访问仍保持 relay；重新开启后访问恢复 direct，业务路径没有失效。
- 本轮证明真实双机“按需建立、relay 保底、直连优先、空闲回收、再次激活、开关切换和双栈数据”闭环；Hyper-V PrivateNat 三 VM 失败仍只记录为测试基础设施 UDP 出口限制，不覆盖本次真实双机通过结果。两端当前连接保留用于人工查看，未把账号密码写入文档或日志。
- 双机验收后的回归审计：WSL 主控/节点 21+14 项测试通过，Windows MSVC service 9 项测试通过，protocol 5 项测试通过；Vue 类型检查和生产构建通过。当前源码的节点路由回归与宿主机第三客户端编排脚本均通过 PowerShell 语法检查。
- 随后完成一次真实本机生命周期回归：断开后 IPC 状态为 disconnected、隧道为空、主控设备变为 revoked 且 peer 列表清空；同一用户重新连接复用 `10.88.0.15` / `fd88:1000::f`，配置版本升至 v2，随后再次从 relay 恢复为 direct。两端当前仍保持连接。

### UPnP IGD 公网打洞增强 — 2026-10-06

- 按用户决定停止并跳过 Hyper-V P2P 测试；此前 broker 运行已完成 VM、载荷和网络租约清理。Hyper-V PrivateNat 的 UDP 出口限制保留为测试基础设施限制，不再作为本轮产品准出阻塞项，也不再启动新的 Hyper-V P2P 请求。
- 客户端 P2P 传输新增可选 UPnP IGD：异步发现家庭网关，为客户端随机 UDP 打洞端口申请 5 分钟 UDP 映射，把外部 endpoint 加入目录候选；每 2 分钟续租，关闭 P2P/断开时尽力删除，异常退出由短租约自动回收。
- UPnP 只增强直连候选，不映射 WireGuard 隧道端口，不开放控制面，不传递用户 token；发现、SOAP、续租、删除任一步失败都保持节点 relay 和已有反射地址路径，且 IGD 网络调用不阻塞数据包循环或 UI 断开流程。
- 本地 WSL service 测试 9 项通过；Windows MSVC service `cargo check --target x86_64-pc-windows-msvc` 和 release 编译通过；Vue 生产构建、PowerShell 脚本语法检查通过。新增依赖使用开源 `igd 0.12.1`。
- 最终安装包已重新构建：[PMMan-Network-Client-UPnP-Setup.exe](../artifacts/windows-installer/PMMan-Network-Client-UPnP-Setup.exe)，SHA-256 `3C88F9AFE483FC81E4D9502133966B38C5367375C2B5FD82BD8C667A0B87A55E`；Windows sidecar SHA-256 `7341AC03331BF0279FEE15248EA54B6F256109E687ABA94955BBE86E780958A3`。产物静态检查通过：无 `localhost:1420`、无 UI fixture、保留 UAC/Common Controls manifest，sidecar 包含 UPnP 标记。
- 当前宿主机原有真实双机连接未被构建打断，仍为 `connected`、P2P 开启、节点 `txgz-b`；新包尚未覆盖正在运行的旧 sidecar，因此本轮不虚构“UPnP 网关实际映射成功”运行时证据。没有 UPnP 时既有真实 relay/direct 闭环仍是产品兜底路径。
- 2026-10-06 回归：修复 P2P 测试调用缺少限速器参数，并兼容旧设备引用已删除节点时回退到健康 relay。WSL service 10/10、protocol 5/5、主控 21/21、节点 14/14 全部通过；使用独立 target 目录完成 Windows MSVC release service 构建，确认 `igd 0.12.1` UPnP 路径可编译。未停止宿主机现有 PMMan 服务，未宣称本机路由器实际下发映射。
- 2026-10-06 P2P 稳定性修复：直连租约从 800ms 调整为 3s；单个直连数据包/ACK 丢失不再立即拆除直连，连续 3 次活动数据失败才回退 relay；有效的 direct DATA、ACK 或探测会清零失败计数。目标是消除公网 UDP 瞬时丢包导致的 direct/relay UI 横跳，同时保留活动流量的自动回退。
- 为不打断当前宿主机连接，生成独立验收目录 `artifacts/standalone-p2p-stable/`；client 使用现有发布 UI，sidecar 为本轮修复版本。sidecar SHA-256：`AFFAC29C02591B64C50E6BB30402628EBF351E6DA5E499609BF151801D7BAA`。启动前需先关闭当前旧 PMMan 服务，否则新客户端会复用旧 IPC 服务。
- 2026-10-06 P2P 路径状态稳定性修复：UDP PROBE 回复不再直接提升为 `direct`；同一候选端点需先连续通过探测，再完成真实加密 DATA/ACK 交换后才显示 `direct`。relay 修复收到 ACK 时会计入直连失败，连续三次失败后回退并进入 5 秒候选冷却，避免瞬时丢包或 stale NAT 候选导致 `direct`/`relay` 反复横跳。新增 probe-only、relay-repair 冷却回归测试；WSL P2P 测试待本轮完整回归后记录。
- 2026-10-06 本轮回归：WSL service 16/16 通过，Windows MSVC release sidecar 构建通过；真实 txgz 验收通过自动选择、txgz-a/b 切换、双栈 WireGuard、split/global、DNS、远程撤销和断开清理。新 sidecar SHA-256 为 `FF89CC62D8ECFC68EE881D2A6CD2814E722C1EBAB692C04EDA200794DC04712F`；更新后的 NSIS 安装包为 `artifacts/windows-installer/Pmman Network_0.1.0_x64-setup.exe`，SHA-256 为 `454B44D52D5368BBECE30EE7ECE42FC8679235D61979E579949F9E793A2D4C1D`。Hyper-V P2P 按用户要求跳过。
- 2026-10-06 节点限速最终修复与验收：发现 WireGuard 握手等待逻辑会把本地限速代理 endpoint 改回公网 endpoint，导致业务绕过限速；同时 NodeProxy 原先用 `127.0.0.1` socket 发公网 UDP，真实代理握手无法建立。现改为 loopback 接入 socket + wildcard underlay socket，并保持 peer endpoint 为 `127.0.0.1:<port>`。WSL service 17/17、protocol 5/5、主控 22/22、节点 14/14、admin/client 生产构建通过。txgz 限速实测 2/3 Mbps 为上行 1.879、下行 2.875 Mbps；在线改为 1/1 Mbps 后为上行 0.936、下行 0.952 Mbps，日志结果 `txgz_rate_limit_verification=passed`；txgz-a 配置恢复为 2/3、probe/systemd 服务 active、临时进程清理完成。
- 本轮新增 P2P 直连绕过限速回归：耗尽节点 relay 预算后，direct 数据仍在 500ms 内到达；节点 relay 使用限速器。最终 sidecar SHA-256 为 `6D4375D360D5CEC15726DBC8FD8085CE8090631F5F0289028CE0D7A96EB39F77`，最终 NSIS 安装包 SHA-256 为 `FB46811F54315919A248326FFB0419436D5EBB3B0964A9F864815139925FAE9D`。标准 txgz 回归的连接、双栈、节点切换、split/global 均通过，末段远程 SSH 观测 txgz 路由时超时，已清理本地服务；不将该次末段观测失败记为限速失败。

### 主控承载节点资源：公网部署与一键更新验收 — 2026-10-06

- 本地 WSL 已构建静态 musl `pmman-node`，计算 SHA-256 后由 `scripts/deploy-txgz.ps1` 上传到主控的 `/opt/pmman-network-dev/node-resources/`，并生成/同步 `manifest.json`；未在低内存远端机器编译。
- txgz 主控通过 `GET /v1/admin/node-release` 正常返回当前资源清单；节点资源下载接口要求绑定的 `X-PMMAN-NODE-TOKEN`，错误节点 token 无法下载其他节点资源。
- 在公网 txgz 环境完成资源下载和更新闭环：管理员下发期望版本后，节点通过下一次心跳收到更新指令，主动从主控 HTTPS 下载资源，校验大小和 SHA-256，原子替换本地二进制并重启自身 systemd 服务。
- 本次资源版本为 `9e34c2994e75fe5416967f98d016bf2b67b5e5701783ae65341930c5b95db23b`；节点 A 更新后恢复 `healthy` 且实际版本与期望版本一致，节点 B 保持 `healthy`，主控、节点 A/B 当前 systemd 状态均为 `active`，主控 `/healthz` 返回 `ok`。
- 管理端 `pnpm lint`、类型检查、生产构建及 `git diff --check` 通过。节点接入弹窗可生成带资源校验的一键部署脚本；节点更新不依赖主控反向 SSH，也不新增节点 HTTP 管理面。

### Windows 通知区常驻与快速控制 — 2026-10-06

- 客户端已加入原生 Tauri 通知区图标。主窗口关闭按钮现在只隐藏窗口，客户端进程、后台服务和已建立的 WireGuard 隧道继续运行；左键托盘图标或“打开主界面”可恢复窗口并聚焦。
- 托盘菜单显示实时连接状态，并提供“连接网络/断开连接”和“退出 Pmman Network”。托盘操作使用与 Vue 主界面相同的 IPC 快照；连接/断开运行在阻塞线程，避免菜单和主窗口卡顿，重复点击会被忽略。
- 退出菜单会先尽力发送断开请求，再停止当前客户端拥有的后台服务；网络不可达时也不会把用户卡在退出流程中。托盘图标使用应用内置资源，不增加独立后台可见应用。
- `scripts/build-client.ps1` 已完成本地前端、Windows MSVC 服务和 Tauri release 构建；本轮后续验收采用用户明确要求的宿主机人工检查，Hyper-V 不用于该托盘 UI 测试。
- 宿主机人工验收：用户确认可以正常从通知区托盘图标重新打开主窗口；本轮托盘恢复主界面功能通过。

### 主控承载节点资源与一键部署/更新 — 2026-10-06

- 主控新增版本化节点资源目录 `PMMAN_NODE_RESOURCE_DIR`：通过 `manifest.json` 管理 `pmman-node` 版本、文件大小、SHA-256 和发布时间；节点资源下载只接受绑定节点 token，待接入节点首次注册前也可下载。
- 节点接入弹窗新增「下载一键部署脚本」。脚本在目标 Linux 主机上检查 root、WireGuard、iproute2、curl 和 sha256sum，从主控 HTTPS 下载资源并校验 SHA-256，写入 600 权限环境文件、project-scoped systemd 单元并启动 `pmman-node`。
- 节点心跳增加资源版本上报和更新指令。管理员在节点列表点击「一键更新」后，节点下一次心跳主动下载当前主控资源，校验后原子替换自身二进制并重启 systemd；不需要主控反向 SSH，也不开放节点 HTTP 服务。管理列表显示实际版本和待更新版本。
- 节点原有返回路由表、启用开关和客户端上下行限速编辑保持不变，继续通过心跳同步；主控部署脚本将本地 WSL musl 节点二进制和资源清单同步到 txgz 的 `/opt/pmman-network-dev/node-resources`。
- 本地回归：主控 22 项、节点 14 项测试通过，管理端 Prettier/ESLint/TypeScript/生产构建通过；新增资源下载 token 绑定测试。真实 txgz 部署回归将在本地发布产物重新构建后执行。

### Windows 客户端更新机制实现与公网验证 — 2026-10-06

- 主控新增 `/v1/admin/client-releases/{channel}` 版本管理 API：管理员可列出、创建、编辑、删除 stable/beta 版本；创建/编辑支持更新日志、最低支持版本、强制更新时间和 `.exe` 轻量更新包上传。主控对上传清单重新签名，并把版本清单和安装包按版本目录原子落盘。
- 管理后台新增「客户端版本」页面，使用 Element Plus 表格、表单、上传组件和确认对话框，支持版本切换、创建、编辑、删除、更新日志和轻量包上传；上传上限为 64 MiB。
- 客户端 Tauri 注册了 `check_update`、`download_update` 和 `install_update` IPC command。启动时执行一次检查；发现更新自动弹窗显示版本和更新日志；下载在 service 后台线程执行，UI 通过真实 IPC 状态显示进度；下载完成后弹出第二次确认；关于区域显示版本、最近状态时间和检查入口。
- 轻量更新配置 `src-tauri/tauri-update.conf.json` 使用 `resources: []`，不重复携带首次安装 WebView2/WireGuard 依赖。更新器校验签名、文件大小和 SHA-256，等待旧客户端退出，运行 NSIS，启动新客户端健康探针验证版本/UI/IPC，失败时恢复备份文件。
- 初始 `0.2.0` 版本的本地与公网验证记录保留在本节历史中；最终发布包和当前公网版本以本节后面的 `0.2.1` 记录为准。
- txgz 已重新部署主控并配置仅主控可读的签名私钥；当前 stable 指针为签名版本 `0.2.1`。`scripts/verify-client-update.ps1` 公网验证通过：公开清单/下载、IPC 检查、IPC 后台下载、缓存文件 SHA-256 均通过。
- Hyper-V 隔离安装器测试确认 UAC 提示绑定到精确包摘要并被接受，安装器欢迎页截图已收集；随后因测试 broker 的 guest 进程清理/点击辅助异常报告 `HarnessSucceeded=false`，没有把该次结果宣称为安装器应用通过。真实 WireGuard/P2P 不在该隔离测试范围内。
- 收尾修复：管理页展示发布包 SHA-256；主控创建非最高 SemVer 版本时不再错误标记为当前发布；删除版本严格校验 stable/beta 和 SemVer，主控读取发布包时再次核对实际 SHA-256。更新器固定使用当前客户端目录、NSIS `/S` 和最后的 `/D=<install-dir>` 参数，更新时不显示安装向导，仅在 per-machine 权限需要时显示 UAC。
- 本轮回归：本地 WSL protocol 6、service 17、master 24、node 14 项测试通过；客户端和 admin 的 Prettier、ESLint、vue-tsc、Vite 生产构建通过；本地 Visual Studio 2022 MSVC 环境下 Windows Tauri `cargo check --target x86_64-pc-windows-msvc --locked` 通过。
- 按静默替换修复重新构建并发布 `0.2.1` 轻量包：大小 3,046,070 bytes，SHA-256 `57c67c9df2ebff7503d7016a9e5979288bdfd25d759a5df4146e8e203272e26d`。公网版本清单、下载、IPC 检查、后台下载和缓存摘要校验全部通过。
- Hyper-V 隔离验证最终包请求 `executable-test-20261005T220057405Z-af10802d` 通过：精确包 UAC 接受、`/S /D=<outdir>\pmman-update-target` 安装进程退出码 0、目标目录生成客户端/service/卸载程序，完成后桌面无安装向导；`HarnessSucceeded=true`、`TestEvaluated=true`、`TestPassed=true`、`VmFinalState=Off`、进程/载荷清理通过。截图已查看：`D:\VMs\Codex-Harness\Live\Broker\Results\executable-test-20261005T220057405Z-af10802d\silent-update-finished.png`。此前同包一次 UAC 绑定失败是 harness 瞬态失败，随后干净 worker 重试通过。

### 服务端/客户端独立发布流水线 — 2026-10-06

- 新增 `scripts/build-server-release.ps1`：只在本地 WSL 构建 admin、主控和节点 musl 产物，生成同一 `releaseVersion` 的主控、节点、节点资源、部署 helper、`release.json` 和 `pmman-server-node-<version>.tar.gz`；远程机器不参与编译。
- 新增 `scripts/publish-server-release-txgz.ps1` 和 `scripts/txgz/activate-server-release.py`：发布只消费已有产物，远端再次校验 SHA-256，在项目专属 staging 目录原子替换主控/节点/节点资源，保留回滚快照，只重启 `pmman-dev-*` 服务；版本或健康检查失败时恢复上一版。
- 新增 `scripts/verify-server-release.ps1`：验证公网主控 `/v1/version`、主控/节点运行摘要、两个节点健康状态、共同资源版本和节点 desired 版本；新增 `/v1/version` 作为服务端发布身份检查。
- `scripts/deploy-txgz.ps1` 改为首次部署兼容入口，后续推荐明确执行构建和发布两个阶段；不再把远程上传、构建和发布逻辑混成一个不可审计步骤。
- 客户端发布改为 `scripts/publish-client-update-txgz.ps1` 直接消费指定的已构建 `.exe`，通过主控管理员 API 创建/编辑并签名版本；新增 `scripts/verify-client-release.ps1` 验证主控清单、包体大小、SHA-256 和公网下载。客户端发布不重启主控/节点，服务端发布也不修改客户端版本目录。
- 本地服务端 `0.3.0` 产物构建、manifest/SHA dry-run 通过并已部署到 txgz；公网主控、两个节点、远端二进制摘要和节点资源版本全部验证通过。随后重新发布客户端 `0.2.1` 并再次验证服务端版本不变，客户端清单/下载校验通过。
- 新增 `docs/RELEASES.md`，记录产物目录、构建/发布命令、CI job 边界、远端禁止编译、回滚和凭据存储规则。

### 生产硬编码审计修复与节点网络配置闭环 — 2026-10-06

- 节点接入记录、节点注册、主控持久化、节点编辑和客户端 `DeviceConfig` 现已贯通实际 IPv4/IPv6 网关、DNS、IPv4/IPv6 分流路由；Windows service 不再猜测固定网关地址。
- 节点编辑页新增网关、DNS 和双栈分流路由入口；管理员覆盖配置在节点重新注册/心跳后仍保持生效，页面提示同步目标 Linux 节点本机配置。
- 节点接入配置不再预填生产网关地址；一键部署脚本继续生成节点 ID 专属 systemd 服务，并写入 DNS 和双栈分流文件。
- 生产硬编码审计报告已更新为已修复状态，剩余边界（活动租约运行时状态、txgz 部署参数外置）单独列明在 `docs/AUDIT-HARDCODING.md`。
- WSL master 28 项、node 14 项、service 18 项测试通过；admin Prettier、ESLint、vue-tsc、Vite 生产构建通过。

### 生产硬编码与配置持久化审计（初始发现，已由下一节修复）— 2026-10-06

- 完成主控、节点、Windows 客户端、管理后台、安装/更新包及 txgz 发布脚本审计，报告见 `docs/AUDIT-HARDCODING.md`。
- 确认发布二进制未包含 UI fixture、测试节点或默认测试密码；网络配置原子持久化和客户端更新配置备份路径通过源码核对。
- 当时确认的待修复问题已在前述“生产硬编码审计修复与节点网络配置闭环”节完成；本节仅保留初始发现记录。
- 初始审计本身没有修改或重启生产主控/节点，也没有发布新版本。
### 全面 code review 收尾修复 — 2026-10-06

- 发现并修复用户自助修改密码的持久化一致性缺陷：此前写盘失败时内存密码已经变更，重启后会恢复旧密码；现在只有原子持久化成功后才替换运行时用户快照。
- 管理后台退出登录现在调用主控 `/v1/auth/logout` 撤销当前 Bearer token；即使主控不可达，也会完成本地退出。
- 新增主控回归测试：退出后旧 token 立即返回 401；管理员重置密码后目标用户旧 token 立即失效，新密码可以重新登录。
- 最终回归：protocol 6、主控 31、节点 15、Windows service 19 项测试通过；admin/client Prettier、ESLint、vue-tsc、Vite 生产构建通过；Windows MSVC service `cargo check --target x86_64-pc-windows-msvc --locked` 通过。
- Windows 目标 `cargo clippy --all-targets -D warnings` 通过；修复了严格 Clippy 暴露的无效 unit、循环写法、未使用代码和复杂类型告警。
- 修复客户端版本传播：`scripts/build-client.ps1` 从 Tauri manifest 读取 `0.0.5-alpha`，同时传给 service 的更新比较逻辑；未发布产品 UI 标签继续为 `0.0.1a`，不再使用 service 自身 `0.0.4-alpha` 造成版本错配。
- 按版本传播修复重新构建 Windows service、Tauri release 和 NSIS 全量安装包。最新安装包：[Pmman Network_0.0.5-alpha_x64-setup.exe](../src-tauri/target/release/bundle/nsis/Pmman%20Network_0.0.5-alpha_x64-setup.exe)，大小 `220839116` bytes，SHA-256 `8F7B74A52EBF8CDBF743A5B5C2A95534D53B46A83FC2F01072603EE8EE10D0A9`。
- 新增节点代理启动失败撤销租约、自动恢复登录迟到响应防覆盖、密码写盘失败一致性回归；最终 WSL protocol 6、master 32、node 15、service 20 项测试通过，admin/client 构建和 Windows service Clippy 通过。
- 本轮未重新发布或重启 txgz 生产服务；未运行 Hyper-V P2P 测试，遵循当前环境约束。为完成本地构建，按用户授权停止了宿主机旧 `pmman-client`/`pmman-network-service` 进程，并确认 `WireGuardTunnel$pmman-client` 已不存在。

### 宿主机自更新故障复现与闭环验证 — 2026-10-06

- 宿主机复现旧版 `0.0.3-alpha` 更新失败：`%LOCALAPPDATA%\Pmman Network\update.log` 连续记录 `update_failed=old client still running`；确认这些记录来自旧更新器，不能代表当前源码逻辑。
- 当前更新器已使用 Windows 原生进程句柄等待旧 PID；超时只终止本次更新拥有的旧 PID，然后执行静默 NSIS、健康探针和自动重启，避免重复启动旧客户端。
- 发现并记录构建陷阱：`scripts/build-client.ps1` 只编译客户端和 sidecar，不重新生成 NSIS；本地测试曾误用同名旧安装器。已用 MSVC/真实 Tauri NSIS 重新构建唯一包 `Pmman Network_0.0.9-alpha_x64-setup.exe`，SHA-256 `9FE798A58AC908D9FDF2AFB8D737EA59580FE6EA25E5DCDE2CDF3264CCEC0D68`。
- 宿主机真实替换验收通过：`0.0.9-alpha` 更新器替换签名 `0.0.7-alpha` 包，结果为 `phase=complete`，日志包含 `old_client_terminated`、`installer_exit_success=true`、`health_check=passed`、`new_client_restarted`；配置 `pmman-client.json` 保持不变。
- 验收后宿主机已恢复并启动 `0.0.9-alpha` 修复版，确认新等待逻辑和健康探针仍在安装后的二进制中。`0.0.3-alpha` 用户需要先运行一次全量安装包完成更新器引导，之后进入可自动静默更新的链路。

### 主控生产发布 `0.3.2` — 2026-10-06

- 本地完成 admin 生产构建、主控 32 项测试、节点 15 项测试和 `x86_64-unknown-linux-musl` release 构建；未在 txgz 远程编译。
- 生成不可变产物目录 `artifacts/server-releases/0.3.2/`，主控 SHA-256 `6a6f5557f065bb3bca1225d9b14653c1f900e883ab003af2d4b383898618a81d`，节点/节点资源 SHA-256 `e48f1d0b39e517c032bc41b1bae4d72fd32af7b2b7a13419376f9ebfa706848a`，归档 SHA-256 `dd548a15b87be271e5cf0b6560a570cfac2d536f743ff4a2b1b974ee1d2721c5`。
- 通过 `publish-server-release-txgz.ps1` 原子发布到 txgz，主控和节点服务均为 `active`，公网 `/v1/version` 返回 `0.3.2`，`/healthz` 返回 `ok`，远端二进制和节点资源哈希核对通过。
- 远端回滚快照：`/opt/pmman-network-dev/release-backups/20261006-100453-0.3.2-8ab77b0a`；未覆盖节点注册记录或宿主机其他网络配置。

## 客户端主控重启自动恢复与授权刷新 — 2026-10-07

- 修复客户端点击「刷新节点」在主控重启后直接显示 HTTP 401 的问题：节点刷新、节点选择和连接现在共用一次自动重新登录并重试流程，继续使用 Windows DPAPI 保存的登录凭据，不要求用户重复输入密码。
- 自动恢复成功后同步主控返回的 IPv4/IPv6 接入模式授权；如果当前未连接且已选模式不再被授权，自动回退到「仅内网」模式，避免界面显示一个下一次连接必然被拒绝的选项。
- 客户端每 15 秒在未连接且无用户操作时刷新一次节点和授权状态；连接中不发起后台刷新，避免并发改变连接流程。手动「刷新节点」仍立即执行同一流程。
- 本地 WSL protocol 17 项、Windows service 22 项测试通过；客户端和 admin 的 Prettier、ESLint、vue-tsc、Vite 生产构建通过；Windows MSVC service/Tauri release 构建通过。
- 最新本地验收目录已更新：`artifacts/standalone/pmman-client.exe` 与相邻 `pmman-network-service.exe`。本轮服务端 release `0.3.10` 已本地构建通过，但尚未发布到生产主控；本轮修复尚未声称公网验收完成。
- 已生成轻量升级包 `artifacts/client-updates/Pmman-Network-0.1.4-alpha-x64-update.exe`，包体 `3,090,051` bytes，SHA-256 为 `8167D0D0C073ABD670CB5344E8FC593C1CBB455CCF7A2557C4C2AFB63A1B15F6`；包内产品名和文件版本均为 `Pmman Network / 0.1.4-alpha`，更新配置不包含首次安装所需的额外资源。
- 修正版本递增：之前按 `0.1.4-alpha` 重建的包与已安装版本相同，更新器不会触发升级。现已将 package、Tauri、service 和更新配置统一为 `0.1.5-alpha`，重新生成升级包 `artifacts/client-updates/Pmman-Network-0.1.5-alpha-x64-update.exe`，包体 `3,092,145` bytes，SHA-256 为 `287F3CA7E096347B4E4DA9FF16B8B894A76B7E57B046139F14AE0C485869A7D4`；产品元数据校验通过。
## geosite 主控下发与节点分流 — 2026-10-07

- pmmandns 分流主路径改为主控管理的 V2Fly/Xray `geosite.dat`，默认分类为 `cn` 和 `geolocation-!cn`。
- 主控通过 `PMMAN_GEOSITE_DIR` 管理带版本、大小和 SHA-256 的 geosite 清单；节点使用节点凭据下载，不向客户端暴露文件。
- 节点支持 geosite 的 full、根域名、关键字和正则匹配，下载、校验、解析失败时保留上一份有效规则。
- 旧手工域名规则保留兼容，显式规则优先于 geosite 规则。
- 协议测试 13/13、主控测试 32/32、节点测试 15/15 通过。

## IPv6 全局出口与客户端 DNS 延迟修复 — 2026-10-07

- 修复 Windows 节点限速代理的外层 UDP 出口：IPv4/IPv6 socket 现在根据连接前的物理默认路由设置 `IP_UNICAST_IF` / `IPV6_UNICAST_IF`，并在心跳期间重新绑定，避免全局 IPv6 的 `::/1`、`8000::/1` 把节点 WireGuard 外层包再次送入隧道形成路由循环。
- DNS 转发同一组上游改为并发查询；节点 DNS 仍然优先，本地 DNS 仍作为回退。增加按响应 TTL 的正/负缓存，并对已收到的负响应缩短死上游等待，避免一个不可达 DNS 拖慢全部查询。
- Windows service release 本地 MSVC 构建通过；WSL protocol 17/17、service 24/24 测试通过。
- 宿主机真实验收：IPv6 全局模式实际生成 `::/1` 和 `8000::/1`；`2a0f:1cc5:4700:beef::1` ping 成功；公网 IPv6 `2606:4700:4700::1111` ping 成功；txgz peer 最新握手非零且收发计数增长；本地 `nslookup example.com 127.0.0.2` 首次约 90ms、重复约 40ms，原先约 0.8 秒的延迟不再出现。
- 本机运行中的后台服务已替换为修复版，当前测试隧道保持真实连接。生产主控和节点未修改；完整客户端发行版尚未替换主控版本目录。
- 生成修复版轻量升级包 `artifacts/client-updates/Pmman-Network-0.1.6-alpha-x64-update.exe`，大小 `3104080` bytes，SHA-256 `79C1315BA8362951A05BD8D5C0AE9D1100D5E9DA7479D3F2E59CEFC4B48C7B0E`；包含 IPv6-only 出口边界修复。

## DNS 失败路径再次收敛与客户端升级包 — 2026-10-07

- DNS 转发器继续保持节点 DNS 优先、本地 DNS 回退、同组并发和 TTL 缓存；将单个上游超时收敛到 `450ms`、单次解析总预算收敛到 `2s`，负响应的分流正响应等待窗口收敛到 `60ms`。IPv6 上游不可达或节点 DNS 暂时失效时，不再逐组长时间等待。
- WSL 回归：protocol `17/17`、Windows service `24/24` 全部通过；Windows MSVC service、Vue 前端和 Tauri release 构建通过。
- 版本统一为 `0.1.7-alpha`，生成轻量升级包 [Pmman-Network-0.1.7-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.1.7-alpha-x64-update.exe)，大小 `3104461` bytes，SHA-256 `C3596CFA5371278A45C2F64D0B020C3ADDFFCB6EBC6807F6B9D0EA780ADD6AD5`。同一文件也位于 Tauri NSIS 输出目录，可静默替换现有安装目录，不携带首次安装依赖。
- 这次构建使用轻量更新配置，`src-tauri/target/release/bundle/nsis/Pmman Network_0.1.7-alpha_x64-setup.exe` 是同一份更新包，不把它当作包含 WebView2/WireGuard 依赖的全量首次安装包；service sidecar SHA-256 为 `D54CFE9E57B0928737E1CC6D88D37EC3B57B869ABEC8F6C78183D433D48A47EA`。

## 本机 IPv6 全局模式路由循环排查 — 2026-10-07

- Windows 当前隧道地址为 `2a0f:1cc5:4700:beef::a`，`::/1` 和 `8000::/1` 已下发；`Find-NetRoute` 确认节点公网 IPv6 `2a0f:1cc5:4701::2` 也选中 `pmman-client`（ifIndex 130），不存在底层出口豁免。
- 同一节点 IPv6 的 TCP 22 只读连通性对比：默认路由 4 秒超时；设置 socket `IPV6_UNICAST_IF=19` 走物理以太网后成功连接。因此不能将默认路径超时解释为节点 AAAA 不可达。
- 源码确认节点限速代理 `service/src/node_proxy.rs` 的 wildcard UDP socket 未绑定底层接口；P2P 传输已有 `IP_UNICAST_IF` / `IPV6_UNICAST_IF` 逻辑，但未应用到独立 NodeProxy。全局路由会截获节点代理的外层 IPv6 UDP，导致递归进隧道。
- txgz 当前客户端 peer 最新握手为 0，计数为 RX 148 / TX 92 bytes；网关及公网 IPv6 ping 均未通过。当前证据定位外层出口问题，不代表节点转发和返回路由已完成验收。
- 本轮是故障诊断，未替换本机安装、修改生产路由或重启主控/节点。排查初期尝试的 DNS 多地址回退源码已撤回，避免将回退误作为根治；本轮中间构建的 service release 文件不是交付产物。
- 后续修复应共享底层接口选择，将节点代理 UDP 固定在物理出口，并支持网络切换刷新；再验证真实握手、节点解密 IPv6 包及外网回包。TCP 出口对比不能代替完整 WireGuard 验收。

## 客户端软件路由管理改造 — 2026-10-07

- Windows WireGuard 配置新增 `Table = off`，并固定将 `::/0` 作为加密 peer 的 IPv6 接收范围；主控授权路由不再由 WireGuard 自动写入 Windows 路由表。
- 新增 `service/src/route_manager.rs`，使用 Windows IP Helper `CreateIpForwardEntry2/DeleteIpForwardEntry2` 管理网关、内网/分流/全局路由、非全局 IPv6 高 metric 默认路由及 P2P 主机路由。
- 连接安装路由后才开始本次握手；路由失败、握手超时、用户断开、后台心跳撤销和 P2P peer 移除均执行回滚/清理，避免遗留路由覆盖用户原有 IPv6 默认路由。
- Windows MSVC route manager 单元测试 3/3、WSL service route manager 单元测试 3/3、Windows target `cargo check --locked` 通过；随后生成轻量更新包，但尚未宣称公网业务验收完成。
- 随后将客户端版本递增为 `0.1.8-alpha` 并生成轻量升级包 [Pmman-Network-0.1.8-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.1.8-alpha-x64-update.exe)，大小 `3113138` bytes，SHA-256 `3FD503F58DCBB7ACBBC7D7965CFB446BEF619D18B009CF9D67481D7F93A6B4D0`。包内版本元数据正确，未发现 `localhost:1420` 或 UI fixture 标记；该包仍是更新包，不替代包含 WebView2/WireGuard 依赖的首次安装包。
- Hyper-V 隔离 smoke 已确认 UAC 绑定/接受成功，安装器欢迎页截图位于 `D:\VMs\Codex-Harness\Live\Broker\Results\executable-test-20261007T074431108Z-d90d3838\update-installer-smoke.png`；三次尝试均因当前 harness 在安装器进程退出/点击清理阶段留下 PID 或 `ClickLeft` 环境错误而未达到 `HarnessSucceeded=true`，因此不记为应用测试通过，也不替代真实更新流程验收。VM 已回收为 Off，payload child 已删除。

## WireGuard 接口启动时序与 Windows 错误 87 修复 — 2026-10-07

- 修复 `Table = off` 路由管理改造引入的启动竞态：WireGuard 隧道服务安装成功后，客户端最多等待 20 秒再读取 `pmman-client` 网卡索引，避免接口尚未注册时直接报错。
- `ConvertInterfaceAliasToLuid` 返回 Windows 错误 87 时，增加 `Get-NetAdapter` 的隐藏窗口回退查询；成功取得网卡索引后继续使用 IP Helper 安装路由，不降低后台服务权限。
- 连接流程继续只接受本次注册公钥的非零新握手；接口等待、握手超时或路由失败都会卸载本次隧道并撤销租约。
- 已停止残留的本地 `pmman-client`、`pmman-network-service` 和 `WireGuardTunnel$pmman-client`，重新构建 Windows MSVC service/Tauri release 和轻量更新包。
- 最新更新包 [Pmman-Network-0.1.8-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.1.8-alpha-x64-update.exe)，大小 `3113730` bytes，SHA-256 `43778681B7108D179E70F4857B4B957621B06807F58AEE2ECC77800633B65D15`；该包包含本轮接口等待与错误 87 回退修复，不是首次安装依赖齐全的全量安装包。
- 前端生产构建、Windows MSVC release 构建和更新包 NSIS 打包通过；尚未在 Hyper-V 中宣称 WireGuard 真实连接验收通过，因隔离环境不提供当前公网节点和 WireGuard 驱动条件。

## 按 mrouter pmmandns 3.2.0 语义修复国内 AAAA 回退并发布 — 2026-10-07

- 只读核对 mrouter `/usr/sbin/pmmandns`：二进制版本标记为 `3.2.0`；`baidu.com AAAA` 返回 `NOERROR / ANSWER=0`，`www.google.com AAAA` 正常返回 IPv6，确认 mrouter 配置和行为符合预期。
- 定位项目缺陷：节点或客户端把合法的 `NOERROR + NODATA` 当成“没有结果”，继续请求默认国外/本地 DNS，导致国内域名 IPv6 泄漏。
- 修复 `protocol/src/dns.rs`：分类 DNS 返回 NODATA/NXDOMAIN 时立即结束；客户端仅在上游超时、连接失败等无响应场景回退本地 DNS。新增回归测试覆盖分类 NODATA、不查询国外默认 DNS、客户端负响应不回退和超时回退。
- 协议测试 `19/19`、主控测试 `32/32`、节点测试 `16/16`、admin 格式/类型检查及生产构建通过。
- 本地构建并发布 `0.3.14` 到 txgz；主控、节点和节点资源远端 SHA-256 校验通过，主控/节点服务 active，备份目录为 `/opt/pmman-network-dev/release-backups/20261007-090223-0.3.14-4ac3bc92`。
- txgz 节点当前已加载 geosite `2026-10-07-mrouter`，pmmandns 正在监听 `172.20.60.97:53` 和 `[2a0f:1cc5:4700:beef::1]:53`；本轮未复制 mrouter 的闭源二进制，生产节点继续使用主控可管理的项目节点实现，并对齐其 DNS 响应语义。

## Windows 节点双栈 endpoint 偶发无握手修复 — 2026-10-07

- 生产错误表现为本地 WireGuard 接口已安装，但 `cn-gz02.pmman.tech:51830` 偶发没有本次握手；随后重试又可连接。只读核对确认节点 UDP 51830 正常监听、其他客户端持续有新握手，故障不属于节点端口未监听。
- 根因是客户端 `NodeProxy` 对同时存在 A/AAAA 的节点域名直接使用 DNS 返回的第一个地址。Windows 外层 IPv6 路由虽存在，但到该节点的 IPv6 路径并不稳定；DNS 返回顺序改变时，客户端会在同一个节点上表现为“接口已启动但没有握手”。
- 修复 `service/src/node_proxy.rs`：解析到双栈地址时稳定优先 IPv4；IPv4 不存在时仍使用 IPv6-only endpoint，并保留物理出口接口绑定。新增双栈优先级和 IPv6-only 回归测试。
- WSL 静态目标 service 测试 `29/29` 通过；前端生产构建、Windows MSVC service/Tauri release 和 NSIS 更新包构建通过。
- 生成更新包 [Pmman-Network-0.1.9-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.1.9-alpha-x64-update.exe)，大小 `3118862` bytes，SHA-256 `2D52A75D113B0FDC8058A5C6C8BA0D49946057F5C8CAC364962C11AD8758191B`。该包尚未发布到主控。
- txgz 期间出现的节点日志 `master heartbeat request failed: http status: 502` 与主控服务同时重启时间一致，是发布切换窗口的瞬时反代/主控不可用；节点随后可自动重新注册。当前不能把这类发布窗口日志作为 WireGuard 51830 端口故障依据。

## Windows WireGuard 握手误判修复与实机验收 — 2026-10-07

- 在宿主机用已安装的 `0.1.9-alpha` 重现了连续 3 轮连接失败，并同时观察 txgz：节点确实安装了本次客户端 peer，且出现了非零握手和双向流量；客户端仍因本机 `wg.exe` 握手 Unix 时间戳与进程 `SystemTime` 的比较结果不满足 `timestamp >= installed_at` 而报错。
- 修复 `service/src/main.rs`：隧道公钥已严格校验为本次新生成的客户端公钥后，握手成功条件改为匹配本次节点公钥且 handshake 非零，不再依赖跨服务/系统时钟的先后比较；新增解析回归测试。
- 重新构建并安装本地 `0.1.10-alpha` 后，真实连接 4 次均成功（配置版本 v15、v16、v17、v18），服务返回 `state=connected`；txgz `wgnetport` 观察到客户端 peer、endpoint `14.145.7.178:*`、非零握手及持续增长的收发计数。
- 断开验收通过：本机隧道服务移除，txgz peer 数量为 0，52001 表中的客户端 IPv4/IPv6 回程路由均已清理。
- 交付更新包 [Pmman-Network-0.1.10-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.1.10-alpha-x64-update.exe)，大小 `3119632` bytes，SHA-256 `D96ACEC23944918BF644C1DF4D49A522DAAE8B7DAC5CA315D37C2D068221A85D`。尚未发布到主控。

## 用户固定地址租约 — 2026-10-07

- 主控用户账户新增持久化地址策略：`automatic` 自动分配和 `fixed` 固定双栈地址；旧账户 JSON 缺少字段时兼容为自动模式。
- 新增管理员接口 `POST /v1/admin/users/{username}/address-lease`，管理台用户页面新增“地址租约”编辑入口。
- 固定地址区分“从地址池分配”和“手动指定”：前者选择池内可用双栈地址并排除 IPv4 网络/网关/广播地址，后者允许池外合法单播地址；只手动指定池内一侧时按偏移补齐另一侧。固定保留会计入地址池占用，即使用户尚未上线。
- 自动分配会避让所有固定保留和历史设备租约。地址池缩小时，活动租约和固定租约在池外会阻止保存；池外的离线自动租约允许保留，并在下次连接时重新分配。
- 固定租约冲突、活动连接在线迁移、池耗尽和双栈地址不匹配均返回明确的 `400`/`409`/`502` 错误，不静默覆盖其他用户地址。
- WSL 静态目标主控测试 `35/35`、节点测试 `16/16` 通过；admin Prettier、ESLint、vue-tsc 和 Vite 生产构建通过；`x86_64-unknown-linux-musl` release 构建通过。`0.3.18` 已通过 `publish-server-release-txgz.ps1` 原子部署到 txgz，公网 `/v1/version` 返回 `0.3.18`、`/healthz` 返回 `200/ok`，远端哈希和发布验证通过；回滚快照为 `/opt/pmman-network-dev/release-backups/20261007-141440-0.3.18-26b36199`。
- 详细规则与 API 示例见 [ADDRESS-LEASES.md](ADDRESS-LEASES.md)。

## 地址池固定租约语义修正与生产发布 — 2026-10-07

- 明确区分“从地址池分配”和“手动指定”：前者由主控选择池内可用双栈地址，继续排除网络、网关和广播地址；后者允许合法单播 IPv4/IPv6 位于地址池之外，也允许两侧地址分别指定。
- 手动指定池外地址不会因后续地址池缩小被拒绝；自动租约仍按地址池变更规则处理。池占用统计会分别计入固定租约落在池内的地址，避免自动分配冲突。
- 新增回归覆盖池外固定地址连接、池变更、冲突和自动租约边界；主控 `35/35`、节点 `16/16` 全部通过。
- 本地构建并部署服务端 `0.3.19` 到 txgz；admin 检查/构建、musl release、远端哈希、发布验证和公网 `/v1/version`、`/healthz` 均通过。主控与生产节点服务 active，节点未重启。
- 回滚快照：`/opt/pmman-network-dev/release-backups/20261007-151124-0.3.19-ebc93760`。

## 端口转发模块规则落盘 — 2026-10-10

- 新增 [PORT-FORWARDING.md](PORT-FORWARDING.md)，记录端口转发模块的正式设计约束；当前仅完成设计文档，尚未宣称功能实现或生产可用。
- 明确 TCP 和 UDP 可以共享同一个入口公网端口，并按一个“入口 + 公网端口”计一个用户活动额度；资源唯一键为 `UNIQUE(entry_id, public_port)`，不能按协议拆分。
- 明确所有节点视为同一内网并具备到达任意客户端地址的路由，入口节点直接向目标客户端转发；不同入口拥有独立端口范围、保留端口、冲突域和容量统计。

## 端口转发第一版实现 — 2026-10-10

- 主控新增独立 SQLite + WAL 存储，包含入口、端口范围、保留端口、用户授权、租约和事件表；数据库路径由 `PMMAN_PORT_FORWARD_DB` 指定，未指定时从用户存储目录推导，不改变原有用户、地址池和节点 JSON。
- 完成客户端和管理员 API：用户只能为当前在线设备的 WireGuard 地址申请转发，公网端口由主控随机分配；入口、协议、地址族、目标端口和并发额度均由主控再次校验。
- 完成管理后台“端口转发”和“转发授权”页面，使用 Element Plus 表格、筛选、编辑弹窗、危险操作确认和 5 秒轮询；生产主控没有自动创建入口或测试节点。
- 节点通过心跳接收完整 desired forwards，使用项目专属 `pmman_network_port_forward` nftables 表执行 IPv4/IPv6 DNAT、conntrack mark 和 SNAT/MASQUERADE；节点应用失败按条回报，成功租约才进入 `active`。
- 修复边界：TCP/UDP 双协议共用一个入口端口和额度；不同入口端口池独立；并发分配不会重复；保留端口排除；入口编辑不能让活动租约落出范围；撤销用户/设备、停用节点和客户端断开会清理或进入可恢复状态；同一设备在 300 秒内重连可恢复原租约。
- WSL 验证：主控端口转发单元测试 7/7（含并发、TCP/UDP 共端口、不同入口同端口、保留端口、授权撤销、断开恢复、入口编辑边界）通过；主控完整测试 41/41、节点完整测试 16/16、service 检查通过；admin 格式、lint、类型检查和生产构建通过；Tauri Windows MSVC `cargo check` 通过。
- 使用 WSL `nft -c` 验证 IPv4 和 IPv6 DNAT/SNAT 规则语法。尚未完成 txgz 公网真实端口转发流量验收，因此当前不能把生产入口配置或公网 TCP/UDP 可达性描述为已验收。

## 端口转发边界收敛与客户端入口选择 — 2026-10-10

- 修复主控租约状态竞态：节点延迟返回的旧 `applied_port_forwards` 不再把已经进入 `releasing`、`released` 或 `expired` 的租约恢复为 `active`；释放操作改为幂等的条件更新，未知租约会明确返回错误。
- 修复入口容量统计：只统计端口范围内的保留端口和活动公网端口，TCP/UDP 共用端口仍只占一个资源。
- 主控客户端入口列表现在只返回当前策略允许的地址族、协议、启用且健康的入口；入口保存会校验保留端口必须位于端口范围内，并阻止 SSH、DNS、HTTP、HTTPS、WireGuard 和 UDP 探测端口在未保留时进入可分配池。
- 客户端自动入口选择按已探测的节点延迟排序，并跳过已耗尽端口池；手动入口继续由主控做最终授权和容量校验。
- 回归验证：主控端口转发专项 10/10（主控完整套件现为 45/45）、节点 16/16、协议 19/19、Windows service 30/30；客户端和管理后台格式检查、lint、类型检查及生产构建通过。
- 端口转发生产公网 TCP/UDP 流量仍需在管理员创建真实入口和用户授权后进行验收；代码不会自动创建入口、节点或测试租约。

## 主控与节点发布 0.3.22 — 2026-10-10

- 本地 WSL 构建 `x86_64-unknown-linux-musl` 主控和节点，主控 45/45、节点 16/16 测试通过；管理后台构建、格式检查、lint 和类型检查通过。
- 通过 `publish-server-release-txgz.ps1` 原子部署到 txgz，公网 `/v1/version` 返回 `0.3.22`，`/healthz` 返回 `200/ok`，远端主控、节点和节点资源哈希校验通过。
- txgz 当前运行 `pmman-dev-master.service` 和 `pmman-dev-node-tencent-gz-02.service`；`deployment.json` 仍为生产配置且节点列表为空，未创建或覆盖任何生产入口、节点、地址池和端口租约。
- 发布归档：`/opt/pmman-network-dev/release-backups/20261010-153446-0.3.22-751182ca`；本地归档 SHA-256：`ce404b07731f378f43b8108275ef44d18bc777e859e375dcaf5682dc4d2f618c`。

## 管理后台白屏修复与生产发布 0.3.23 — 2026-10-10

- 定位到管理后台白屏根因：Vite 8/Rolldown 的 Element Plus 手工 code-splitting 产物在浏览器初始化时抛出 `TypeError: e is not a function`，导致 Vue 根节点未挂载；不是主控登录接口、生产数据或反向代理问题。
- 删除 `admin/vite.config.ts` 中针对 `element-plus` 的自定义拆包规则，改用 Vite 默认依赖打包，避免生成有问题的 Element Plus 运行时代码。
- 本地管理后台 Prettier、ESLint、vue-tsc、生产构建通过；主控测试 `45/45`、节点测试 `16/16` 通过。
- 发布 `0.3.23` 到 txgz，公网健康检查、版本检查、远端哈希校验和发布验证全部通过；主控与节点仍为管理员管理模式，没有创建或覆盖生产节点、入口、地址池及端口租约。
- 生产后台重新加载后已正常显示管理员登录页，新的入口脚本为 `index-D6TqBNe4.js`；旧的浏览器控制台错误来自发布前缓存的 `element-plus-CyJsmZ37.js`，不影响新版本。
- 发布备份：`/opt/pmman-network-dev/release-backups/20261010-155322-0.3.23-e8af135f`。

## 端口转发授权后台重设计 — 2026-10-11

- 将“端口转发授权”从单用户长表单改为标准中台的“策略列表 + 右侧编辑抽屉”模式：列表支持用户名搜索、状态筛选、授权状态、真实租约并发用量、协议/地址族和入口数量概览。
- 新增启用用户、配置待完善、可用入口和在线设备四项概览指标；并发用量基于主控端口转发租约的 `active/applying/pending` 状态统计，不再用在线设备数替代。
- 编辑抽屉按基础权限、协议与地址族、公网入口范围、目标端口限制分区；入口选择支持搜索、IPv4/IPv6 筛选、批量选择、节点健康状态、端口容量和已选入口回显。
- 增加启用前置校验、目标端口列表校验、失效入口提示和变更影响说明；继续复用原有策略 API，不改变生产数据和服务端协议。
- 管理后台 Prettier、ESLint、vue-tsc 和生产构建通过；尚未发布生产，待确认页面验收后再生成新的主控发布版本。

## 端口转发授权后台生产发布 0.3.24 — 2026-10-11

- 本地完成主控/节点构建前检查，主控测试 `45/45`、节点测试 `16/16`、管理后台 Prettier、ESLint、vue-tsc 和生产构建全部通过。
- 通过 `publish-server-release-txgz.ps1` 原子发布到 txgz，公网版本为 `0.3.24`；健康检查、远端主控/节点/节点资源哈希校验和发布验证通过。
- 生产后台已加载新的入口脚本 `index-B53LKRvq.js`，登录页正常显示；未修改地址池、节点、入口、授权策略或端口租约数据。
- 发布备份：`/opt/pmman-network-dev/release-backups/20261010-164150-0.3.24-fc56d99e`。

## 客户端升级包 0.1.12-alpha — 2026-10-11

- 使用本地 Windows MSVC 构建 Vue 前端、Windows 后台服务、Tauri 客户端和 NSIS 轻量升级包；构建脚本全流程通过。
- 更新包：[Pmman-Network-0.1.12-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.1.12-alpha-x64-update.exe)，大小 `3189994` bytes，SHA-256 `1487E75852F5AAD52D2D1A1379E1755E9B7C2189268F9B8119582654EF0C8846`。
- 临时更新配置确认版本为 `0.1.12-alpha`，使用静默更新 NSIS hook；包体未包含 `localhost:1420`。
- 本轮只生成本地升级包，未自动上传或创建主控客户端版本记录。

## 端口转发 502 修复与生产验收 0.3.25 — 2026-10-11

- 定位 502 根因：生产主控已运行 0.3.24，但广州节点 systemd 实际执行的
  `/opt/pmman-network-dev/nodes/tencent-gz-02/pmman-node` 仍是 0.3.19；此前发布只更新了主控资源目录中的 `/opt/pmman-network-dev/bin/pmman-node`，未更新正在运行的节点实例，因此旧节点不会处理端口转发心跳字段。
- 节点同步链路增加端口转发快照、应用数量和 nft 失败原因日志；保留注册后必须完成心跳才算健康的语义，避免仅注册成功就被客户端选中。
- 本地构建并发布主控/节点 0.3.25；主控测试 `45/45`、节点测试 `16/16`、管理后台检查和生产构建均通过。
- 已安全备份旧广州节点执行文件并更新实际 systemd 执行路径；主控、资源节点和实际节点 SHA-256 均与 0.3.25 发布清单一致，主控 `/v1/version` 返回 `0.3.25`，`/healthz` 返回 200，两个项目服务均 active。
- 使用当前 Windows 客户端真实连接设备验证：创建 TCP+UDP、入口 `cn-gz02`、目标 `172.20.60.107:22` 的端口转发返回 `active`；节点日志确认 `desired=1 applied=1 errors=0`，生产 IPv4 nftables 已生成 DNAT、conntrack mark 和 SNAT 规则。
- 已释放本轮验收租约并确认数据库状态为 `released`、节点规则表已清空；此前失败的测试租约也由节点重连后的撤销同步完成释放，未删除任何生产配置或历史审计记录。

## Windows 路由 1168 修复与诊断日志 — 2026-10-11

- 修复新建 WireGuard 隧道后偶发的 `Pmman 路由 172.20.60.97/32 配置失败：Windows 错误 1168`：WireGuard 安装器返回成功后，Windows IP Helper 路由表可能仍未完成适配器注册；路由安装现在对 `ERROR_NOT_FOUND (1168)` 和短暂的 `ERROR_INVALID_PARAMETER (87)` 重试最多 12 秒，并在每轮重新解析接口索引，兼容隧道替换时索引变化。
- 路由安装、接口等待、路由清理和每次 Windows API 返回码写入 `%LOCALAPPDATA%\Pmman Network\service.log`，日志不包含认证信息、WireGuard 私钥或 Bearer Token。连接失败时可直接收集该文件进行排查。
- Windows MSVC service `cargo check --locked`、Windows route-manager `3/3`、WSL musl service `30/30` 测试通过；客户端修复版本提升为 `0.1.13-alpha`。
- 已生成轻量升级包 [Pmman-Network-0.1.13-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.1.13-alpha-x64-update.exe)，大小 `3196044` bytes，SHA-256 `14DD3850D243974FED8C35A4FD5F7723169898A8864A0F549B051D7BBD76D81B`；standalone service SHA-256 `1F2799013AC0789AA8E62A83553C723DC12053326C2038F3C3A3581366BFD0E4`。本轮未上传主控或创建版本记录。
- Hyper-V broker 已完成载荷隔离与清理，但该 runner 未能捕获本程序的精确 UAC 提权进程（`SystemPromptAcceptanceFailed`），因此不把它记为客户端应用验收通过；这不影响静态构建、前端构建或服务单元测试结果。

## 端口转发错误文案修复 — 2026-10-11

- 修复端口转发失败时错误字段复用问题：后台原来只更新 `port_forward_message`，Tauri IPC 的通用错误通道仍返回上一次连接成功的 `message`，导致页面显示“已通过节点…建立真实 WireGuard 隧道”。现在入口查询、转发查询、创建、释放及未登录/参数错误分支都会同时更新端口转发错误和通用错误字段。
- 新增回归测试 `port_forward_errors_replace_stale_generic_connection_message`，验证 HTTP 502 等端口转发错误不会再被旧的隧道成功信息覆盖。
- 已重新生成 `0.1.13-alpha` 升级包；未修改生产主控、节点或端口租约。

## WireGuard 节点代理偶发无握手修复 — 2026-10-11

- 定位到客户端 WireGuard endpoint 实际指向本机 UDP 节点代理；代理在安装隧道前只绑定一次物理默认出口，网络切换或 VPN/DHCP 路由变化时可能导致握手 initiation 发出但节点无回包。
- 握手等待期间每 2 秒重新应用真实物理出口绑定，等待窗口从 15 秒增加到 25 秒；不改变 `Table=off`、路由下发或节点协议。
- 在 `%LOCALAPPDATA%\Pmman Network\service.log` 增加节点代理解析地址、本地/外联 UDP 端口、发包失败、收发计数、物理出口刷新失败和握手轮询日志；不记录私钥、Bearer Token 或数据包内容。
- WSL service 测试 `31/31` 通过；Windows MSVC service release 和 Tauri release 构建通过；Hyper-V 隔离启动测试因测试进程未自动结束且随后被取消，未形成应用行为通过证据，未在宿主机运行客户端。
- 已生成轻量升级包 [Pmman-Network-0.1.14-alpha-x64-update.exe](../artifacts/client-updates/Pmman-Network-0.1.14-alpha-x64-update.exe)，大小 `3197262` bytes，SHA-256 `83004D1391633A90493F3B90EF60CEEB89A4C56F00F783F1005D8A99641DEFA7`。本轮未重启生产主控/节点，未修改生产数据；客户端版本需由管理员在主控创建发布后供用户更新。

## 本机端口转发公网流量实机验收 — 2026-10-11

- 本机客户端通过广州节点建立真实 WireGuard 隧道，地址为 `172.20.60.107/32`；客户端 IPC 创建了临时 TCP+UDP 共用租约，目标为本机 `172.20.60.107:45900`，主控返回 `active`，节点日志确认 `desired=1 applied=1 errors=0`。
- 节点 IPv4 nftables 已生成并实际加载 `1.14.226.49:50636 -> 172.20.60.107:45900` 的 TCP/UDP DNAT、连接标记和 SNAT 规则；WireGuard peer 对应本机公钥并有最新握手，说明控制面、客户端隧道和节点配置下发链路正常。
- 从公网侧 `shixcm` 和 `mrouter` 对 `1.14.226.49:50636` 发起 TCP/UDP 请求：TCP 返回 `Connection refused`，UDP 超时；节点 `tcpdump` 在探测期间捕获 `0 packets`，而同一公网地址的 HTTPS 已可达。因此流量未进入节点，当前不能判定客户端到目标监听器的实际转发已打通，首要阻塞点是该入口端口范围在云安全组、EIP/NAT 或上游防火墙的放行/转发配置。
- 临时租约 `pf_1eb004eec9444601a0908a2076b4069f` 已释放，主控入口分配数恢复为 0，节点 nftables 转发表已清空；本机临时 TCP/UDP 回显监听器也已停止。客户端保持连接状态，未影响已有配置或其它租约。

## 云 EIP/NAT 入口转发修复与公网实机复验 — 2026-10-11

- 根据公网抓包修正节点端口转发规则：云 EIP `1.14.226.49` 在进入 txgz Linux 前已转换为实例地址 `10.1.0.6`，旧规则匹配 EIP 导致 TCP RST/UDP超时。节点现在优先匹配本机实际地址；入口地址未配置在本机时，自动使用内核默认路由选出的 ingress 源地址，并保留本机直连 IPv4/IPv6 地址的匹配行为。
- 新增节点回归测试，验证 EIP 不在本机地址时生成 `10.1.0.6` 作为 nftables 目的地址；本地 WSL 主控测试 `45/45`、节点测试 `17/17` 通过，musl release 构建通过。
- 发布服务端 `0.3.26` 并更新运行中的广州节点实例；主控、节点服务均 active，实际节点执行文件与发布二进制 SHA-256 一致。生产节点日志确认规则从 `1.14.226.49` 切换为 `10.1.0.6`。
- 公网复验使用本机客户端地址 `172.20.60.107` 和临时入口 `1.14.226.49:50644`：从 `shixcm` 发起 TCP `8217` 字节和 UDP `153` 字节回显，TCP/UDP payload 均完整一致；本机回显日志分别记录 `echo_bytes=8217` 与 `echo_bytes=153`，证明流量已完成公网入口 → txgz DNAT/SNAT → WireGuard → Windows 客户端 → 回程。
- 测试租约 `pf_97e85c56e38a4bba9b71776615d8f9f3` 已释放，节点 nftables 表已清空；历史测试租约也均为 `released`，本机临时监听器已停止。

## PostgreSQL 与主控 Compose 收尾 — 2026-10-11

- 主控生产持久化边界固定为 PostgreSQL：主控和端口转发存储不再保留运行时 SQLite/JSON 回退；旧 JSON 与 `port-forward.db` 只由带 `legacy-migration` 特性的 `pmman-migrate` 一次性读取。
- PostgreSQL 访问改为按操作建立短连接，主控或数据库容器重启后下一次请求可以自动重连；数据库不可用时 `/healthz` 返回 `503`，不会生成默认节点、地址池或账户覆盖生产数据。
- 发布激活脚本现在在迁移前保存 PostgreSQL plain SQL 回滚快照和旧 SQLite 只读备份，迁移或新主控健康检查失败时恢复数据库后再恢复 Compose；节点仍由独立 systemd 服务管理。
- 生产验收脚本改为检查 Compose `master`/`postgres` 服务、镜像标签和两个容器健康状态，不再把已废弃的宿主机主控二进制当作运行时证据。onboarding smoke test 改为重启 Compose 主控，并通过 PostgreSQL 文档清理自身测试节点。
- 本地验证：主控 `35/35`、节点 `17/17`、musl 目标构建和迁移工具构建通过；Python 发布/验收脚本语法检查通过。当前 Windows 主机未安装 Docker，因此本地 Compose 镜像实际构建与空环境启动仍需在具备 Docker 的构建机执行，未在 txgz 远程编译。

## 管理端与客户端界面优化 — 2026-10-11

- Admin 导航按工作台、资源管理、访问控制、运维管理和账户分组；公网入口与转发租约拆为独立菜单，统一页面标题、描述、刷新操作和布局宽度。
- 压缩地址池、用户、端口转发、客户端版本等页面的长段落说明，保留字段级提示和必要的风险确认。
- 客户端首页改为结果导向文案：连接中、已连接、连接失败、连接内网；新增独立设备互联页，设置页拆分网络/外观/关于，端口转发改为显式创建操作。
- 新增统一展示层，将后台技术错误转换为简短提示，原始错误、路由、DNS、P2P 和服务日志入口放入高级诊断并支持复制诊断信息。
- 统一客户端 P2P、节点、流量模式、更新和启动状态文案，去除旧的 PMMan 品牌写法和说明书式提示。
- Admin lint/typecheck/build、客户端 Vue 类型检查和生产构建通过；WSL service 测试 `31/31`、主控测试 `35/35`、节点测试 `17/17` 通过。Playwright 本地烟测确认 Admin 登录页正常；并行启动多个 Playwright 默认会话会触发本地 CLI 管道占用，未作为应用失败处理。
- Docker 测试环境改用 `ssh wsl`：Docker Server `29.8.0`、Compose `v5.5.1` 可用。`docker compose config` 已执行，但因未注入必需的 `PMMAN_ADMIN_PASSWORD` 和 `PMMAN_POSTGRES_PASSWORD` 而按预期停止；凭据未写入项目文件或文档。

## PostgreSQL Compose 实机验收与运行时修复 — 2026-10-11

- 使用 `ssh wsl` 在本地构建 `x86_64-unknown-linux-musl` 主控并构建镜像
  `pmman-network/master:compose-test`；Docker Server `29.8.0`、Compose `v5.5.1`。
- 首次启动发现真实缺陷：同步 `postgres` 驱动在 Tokio 请求运行时内创建嵌套运行时，访问
  `/healthz` 会触发 `Cannot start a runtime from within a runtime`。主控普通数据库文档存储和
  端口转发 PostgreSQL 存储现在统一将同步连接与操作隔离到普通 OS 线程，并在多线程 Tokio 中
  使用 `block_in_place` 让出工作线程；新增运行时隔离回归测试。
- `docker-compose.yml` 支持通过环境变量覆盖测试端口、网络名和 PostgreSQL 卷名，避免隔离验收
  与生产 Compose 资源冲突；生产默认名称保持不变。
- 新增 `scripts/verify-compose.py`。脚本使用随机测试项目名、端口和凭据，校验非 root 主控、
  空环境无 demo 节点、嵌入式后台资源、首次登录强制改密、用户/节点/地址池/内网 CIDR/端口入口/
  端口授权写入 PostgreSQL、主控重启恢复、数据库不可用时 503 且不回退默认数据、数据库恢复重连、
  容器重建后的持久化，以及最终日志无嵌套 runtime panic。
- 完成 49 项 Compose 隔离验收，全部通过；脚本随后执行 `down -v`，测试容器、网络和卷均已清理。
  未连接、修改或重启 txgz 生产环境，未改变生产数据库、节点、WireGuard、BIRD、DNS 或路由。
- 最终代码状态下主控与节点完整回归测试 `37/37`、`17/17`，其中包含新增的数据库运行时隔离测试
  `2/2`；`pmman-migrate` 的 `legacy-migration` 检查、musl release 构建和 Docker 镜像构建均通过。

## 生产 PostgreSQL/Compose 迁移 — 2026-10-11

- 生产预检确认 txgz 当时仍运行旧的宿主机 `pmman-dev-master.service`，没有 Compose；旧用户、节点、
  网络设置和 `port-forward.db` 均保留在 `/opt/pmman-network-dev`。
- 首次发布 `0.3.28` 在新 PostgreSQL 容器备份阶段因远端容器 Unix socket 竞态失败；回滚后公网主控
  仍为 `0.3.26`，旧 systemd 服务健康。清理了这次迁移单独创建的 PostgreSQL 容器、卷和网络，
  未删除旧 JSON/SQLite 数据。
- 修复迁移脚本：PostgreSQL 健康检查、`pg_dump` 和恢复显式使用容器内 `127.0.0.1:5432`；旧宿主机
  主控回滚会恢复旧主控/迁移二进制、Compose 文件、环境文件，并清理临时 Compose 后再恢复旧 systemd。
  同时修复 Windows 验收脚本的 SSH IPv4 和 Docker inspect 模板转义。
- `0.3.29` 在本地 WSL 完成前端、pmmandns、主控/节点/迁移工具测试与 musl/Docker 构建后成功激活。
  生产公网 `/healthz` 返回 `ok`，`/v1/version` 返回 `0.3.29`；`pmman-master` 和 `postgres` 均为
  healthy，旧 `pmman-dev-master.service` 为 disabled/inactive。
- PostgreSQL migration `schema_migrations=1,2`；已恢复 `devices`、`network_settings`、`nodes`、
  `users` 文档，端口转发入口 1 条、历史租约 12 条。未生成 demo 节点，旧节点配置未覆盖。
- 现有广州节点实际 systemd 执行路径不在共享 `bin` 目录，迁移后旧节点二进制曾持续返回 HTTP 502；已先
  备份旧执行文件，再原子替换为 0.3.29 并重启项目节点服务。当前节点 active，节点心跳已写入 PostgreSQL，
  实际执行文件与 `pmman-node-0.3.29` SHA-256 一致。
- 发布回滚快照位于生产 `release-backups` 下，包含 PostgreSQL plain SQL 备份；生产迁移最终成功，
  未修改宿主机现有 WireGuard、BIRD、路由、DNS 或网站服务。

## 客户端节点与转发体验、流量统计 — 2026-10-11

- 端口转发增加数据库部分唯一约束 `(node_id, device_id, target_port)`；同一节点不能为同一设备目标端口创建第二条活动/保留中规则，内存测试后端和 PostgreSQL 事务均执行相同校验。
- 新增客户端配额接口和 IPC 状态，端口转发页在创建前显示已用、总额度和剩余额度；达到额度或未授权时提前禁用创建入口，创建/释放后刷新配额。
- 节点增加 `connectable` 能力开关。关闭后节点仍可作为健康的端口转发入口，但不会出现在客户端连接列表或 P2P 中继选择中；停用客户端连接能力会清理该节点上的活动客户端连接，旧节点数据默认保持可连接。
- 连接节点页移除导航旁的节点数量徽标，改按区域分组；修复深色主题断开按钮文字颜色；客户端地址展示和复制时移除 `/32`、`/128` 后缀，高级诊断仍保留 CIDR。
- 新增客户端和管理后台的节点/P2P 直连/P2P 中转流量展示。客户端从 WireGuard 节点 peer 和 P2P 传输累计计数采集，心跳以设备公钥和配置版本绑定后增量持久化到 PostgreSQL，主控重启与客户端重连不会把计数回退。
- PostgreSQL 初始化在创建新的唯一索引前会自动清理历史重复活动规则：每个节点/设备/目标端口组保留最新租约，其余标记为已释放并保留错误说明，避免旧生产数据阻塞主控启动。
- 修复客户端重启后的端口转发空列表：自动登录完成后立即重新获取端口转发配额、入口和当前账户租约，页面进入后的定时刷新仍作为兜底；主控中的活动租约不会因客户端 UI 重启而被误清除。
- 验证结果：WSL 协议测试 `19/19`、service 测试 `31/31`、主控完整测试 `38/38` 加节点 `17/17`；新增端口转发唯一性测试通过；Admin 构建、客户端 Vue 构建、Windows MSVC service/Tauri `cargo check` 均通过。尚未发布生产和生成交付安装包。

## 节点 BIRD 回程路由广播模板 — 2026-10-12

- 节点模型新增 `wireguard_interface`。管理员创建接入记录时可以填写接口名；节点首次注册会主动上报实际接口名，旧节点缺少该字段时继续兼容。接口名限制为最多 15 个字符，并拒绝空白、引号、分号和换行等注入字符。
- 新增管理员接口 `/v1/admin/nodes/{node_id}/bird-config`。它基于 `config/bird/pmman_network.conf` 动态填充 WireGuard 接口和返回路由表，包含 direct、kernel v4/v6、pipe v4/v6 协议、精确 OSPF 过滤片段、安装步骤和只读验证命令；模板不使用 `export all`，也不包含节点 token、私钥或数据库连接信息。
- 数字路由表可生成 `ready=true` 模板；命名路由表会要求管理员先在节点执行 `ip route show table` 和 `grep /etc/iproute2/rt_tables` 获取数字表号，接口未知时生成明确占位符。主控只生成和下载模板，不通过 SSH 修改节点 BIRD、OSPF 或策略路由。
- 管理后台节点配置新增“路由广播”标签，支持查看状态、复制配置、下载 `.conf`、查看 OSPF 片段、安装步骤和验证命令；节点页面继续使用 Element Plus 组件和现有 typed API。
- 新增模板内容、接口名校验、命名表警告、管理员权限和敏感信息回归测试。WSL 主控完整测试 `42/42`、`cargo fmt --check`、Admin format/lint/typecheck/production build 全部通过。
- 只读验证：`shixcm` 的现有 BIRD 配置语法检查通过，五个 `pmman_network_*` 协议均为 `up`；`nbcjp` 的 BIRD 配置语法检查通过且尚未安装 `pmman_network` 集成。本轮未修改生产节点、BIRD、OSPF、路由或主控数据。

## 生产主控发布 0.3.33 — 2026-10-12

- 本地构建 `0.3.33`：Admin 构建、pmmandns 测试、主控 `42/42`、节点 `17/17`、x86_64 musl release 和主控 Docker 镜像全部通过。
- 通过 `publish-server-release-txgz.ps1` 发布到 txgz，主控 Compose 镜像为 `pmman-network/master:0.3.33`；PostgreSQL 保持健康，公网 `/healthz` 返回 `200/ok`，`/v1/version` 返回 `0.3.33`。
- 远端发布清单和节点资源 `pmman-node-0.3.33` 已完成 SHA-256 校验；现有主控数据库中的 5 个用户、3 个节点和网络配置保留，未创建 demo 节点、未重置地址池或覆盖管理员配置。
- 发布回滚备份：`/opt/pmman-network-dev/release-backups/20261011-163006-0.3.33-ca9459fb`。
- 生产节点服务仍由独立 systemd 管理并保持 active；本次只发布主控和节点资源，不自动重启或更新节点，不修改节点 BIRD、OSPF、WireGuard、路由或 DNS 配置。

## 客户端内置 cnipgen 双栈境外分流 — 2026-10-17

- 新增 `tools/cnipgen/generate_split_routes.py` 和固定资源 `service/resources/cnipgen`。IPv4 使用 cnipgen/nchnroutes 非中国前缀，IPv6 使用 APNIC 中国 IPv6 分配前缀对 `2000::/3` 做差集；生成器只输出 CIDR，排除特殊用途网段并禁止默认路由。
- 当前快照包含 IPv4 `14434` 条、IPv6 `15252` 条，metadata 记录生成器版本、源文件哈希、规则数量、规则 SHA-256 和快照时间。普通构建不联网，只有显式 `--refresh` 才更新；候选文件校验失败不会替换有效资源。
- Windows service 新增规则校验和合成：内网模式使用主控内网 CIDR，境外分流追加本地双栈规则，全局模式保持双半默认路由；WireGuard `AllowedIPs` 与 Windows IP Helper 路由使用同一份结果，安装失败仍清理并撤销连接，不降级冒充成功。
- 主控支持 `split_route_source=client_builtin_v1` 能力协商。新客户端的 split 连接不读取节点手工分流表；旧客户端不携带能力字段时保留旧协议和明确的缺少分流表错误。管理端已移除新接入/编辑表单中的手工海外路由入口，旧字段仅兼容保留。
- 连接高级诊断增加规则来源、快照时间、IPv4/IPv6 前缀数量和路由安装耗时；首页不展开长路由列表。主控、service、协议测试和 Admin/客户端生产构建已执行通过，Windows 真实 WireGuard 和公网双栈验收待 MSVC 产物交由 Hyper-V 隔离环境执行。

## 客户端诊断日志上报 — 2026-10-18

- 后台服务新增统一诊断日志模块，连接、登录、节点配置、路由、节点代理、P2P/防火墙操作、更新和断开等关键行为写入结构化分类事件；Status 高频轮询不写日志。日志写入前过滤换行及凭据标记，避免把密码、Bearer Token、私钥或原始数据包落盘。
- `service.log` 单文件达到 512 KiB 后轮转，最多保留 3 个历史文件；更新日志达到 256 KiB 后轮转。客户端上报包最多 768 KiB，只收集最近的服务/更新日志和运行摘要，不包含登录状态或配置文件。
- 新增客户端 IPC/Tauri 命令“上报诊断日志”和连接详情入口。主控新增 `POST /v1/client/diagnostics`，按账户每 60 秒限流、每个账户最多保留 5 份报告，返回报告编号；报告正文保存到 `PMMAN_DIAGNOSTIC_DIR`，不进入 PostgreSQL。
- Docker Compose、txgz 生产编排和客户端开发文档已增加诊断目录配置。WSL 主控测试 `43/43` 与节点测试 `17/17` 通过；Windows MSVC 构建需在加载 Visual Studio 工具链的 shell 中执行。
# 2026-10-18：修复手动固定地址的节点回程与节点更新重试

- 节点为每个活动客户端地址同时维护返回路由表中的 `/32`、`/128` 和优先级 `10000` 的目的地址 `ip rule`，回包不再因只查 `main` 表而误走公网默认出口；释放、过期和路由表变更会幂等清理两类内核状态。
- 节点资源更新增加已下载包校验复用、落盘暂存和指数退避。主控或资源下载异常时不再按每秒心跳重复拉取整个二进制；仅转发入口节点（`connectable=false`）仍可通过已注册节点凭据正常更新。
- 本地验证：`cargo test --locked --manifest-path server/Cargo.toml`，主控 43 项、节点 17 项全部通过。生产发布与公网回程验证待完成。
- 生产发布验证：主控已运行 `0.3.37`，生产容器和 PostgreSQL 健康。排查两台未更新节点时发现：`nbd-sha-01` 的旧节点程序将资源响应体读取超时设为 3 秒，而 10 MB 资源经公网代理下载约需 5 秒，导致每数秒重拉；`nbd-jp-tyo-01` 被设为 `enabled=false`，旧主控错误地禁止其注册和取更新资源，持续返回 403。
- 修复节点资源下载使用独立的有界超时（180 秒响应体、300 秒总时限）；节点版本文件优先于编译时版本，避免引导替换后继续报告旧版本。主控允许使用匹配节点专属凭据的禁用节点注册并获取节点资源，`enabled` 仍控制客户端可用性，不会让未公开节点进入客户端选择。
- 本地完成 Admin 格式/lint/typecheck/build、pmmandns 测试、主控 `43/43` 和节点 `17/17` 测试，并在本地 WSL 构建静态 musl 资源。生产主控依次发布 `0.3.36`、`0.3.37`，最终 Compose 主控和 PostgreSQL 均健康。
- 两台节点先以已校验的本地 `0.3.36` 二进制引导替换，再通过生产心跳自行下载、校验并安装 `0.3.37`；日志确认均只执行一次更新重启，版本文件为 `0.3.37`，systemd 状态 active。上海节点的心跳版本已被主控记录；东京节点完成注册并保持 `enabled=false`，更新流程不再被禁用状态阻挡。原二进制保留为 `.pre-0.3.36` 以便人工回退。
- 本次更新未修改 WireGuard、BIRD、OSPF、路由和 DNS 配置；没有在线客户端时未执行公网回程流量验收。

## 上海节点 DNS 无监听根因与修复 — 2026-10-18

- 只读检查确认 `nbd-sha-01` 的双栈网关 UDP/TCP 53 均拒绝连接，而宿主机 resolved 查询及国内/国外上游直查正常。PostgreSQL 中该节点的 DNS 地址指向网关，但三个上游字段均为空：节点将 pmmandns 静默视为禁用，主控校验却允许下发不存在的 DNS 服务。
- 同时发现 `/var/lib/pmman-node/pmmandns` 是旧版可执行文件，与新版原默认配置目录重名。默认目录改为 `pmmandns-config`，保留旧文件；新增升级兼容测试。
- 协议增加共享 DNS 完整性校验：无上游的网关 DNS、仅配置单组上游、仅启用 geosite 均不得伪装为有效解析器；兼容外部 DNS 与旧默认上游。主控保存时返回具体错误，并在客户端配置下发时校验旧数据。节点即使没有活动 peer 也记录配置错误/恢复，按状态变化去重。
- 生产已备份 PostgreSQL 和节点环境文件，仅为上海节点配置已实测可达的国内/国外上游并启用现有 geosite，显式指定无冲突的配置目录。主控和上海节点短暂重启后，两族 UDP/TCP 监听恢复；16 组国内/国外 A/AAAA 查询通过，首次查询约 32–35 ms、缓存查询低于 1 ms；国内 AAAA 返回 NOERROR 空答案，国外返回真实双栈地址。未修改地址池、用户授权、其他节点配置或 BIRD/OSPF。
- 新增只读诊断脚本 `scripts/txgz/probe-node-dns.py`。首轮全量回归有一个 P2P 用例因节点 15 秒健康窗口超时返回 503；第二轮全量主控 45/45、节点 19/19 通过。未修改生产健康时限或降低断言。
- 按后续要求增加默认生成与持久迁移：三个上游字段全部为空且配置了网关 DNS/已启用 pmmandns 时生成两组可编辑默认值；已有任一自定义上游不覆盖，不为明确未启用 DNS 的节点添加监听。主控启动、注册、心跳输入、接入和管理员保存使用同一规则。新增幂等性、自定义配置保护和落盘重载测试；协议最新 22/22、Admin 格式/lint/typecheck/build 通过。
- 最终回归通过：协议 22/22、主控 46/46、节点 19/19、Windows MSVC service 40/40；Admin 格式/lint/typecheck/build 与客户端类型检查/生产构建通过。修复 Windows loopback 单元夹具错误绑定物理接口导致永久等待的问题，仅在单元测试中使用无物理绑定的 socket seam，并补上 2 秒接收期限；生产外层 UDP 绑定逻辑不变。
- 本地 WSL 使用独立 PostgreSQL 容器和真实 `0.3.39` 主控镜像验收旧节点记录迁移：默认上游实际写入 PostgreSQL，自定义上游和未启用 DNS 的节点保持原样；重启主控后幂等。测试容器、临时数据和专属网络已清理。
- 主控 `0.3.39` 经标准发布脚本部署至生产，公网版本、HTTP 健康、Compose 健康和远程 SHA-256 校验通过。首次发布后验收遇到 HTTP 已就绪但 Docker 状态仍为 starting 的竞态，验收脚本已改为有界等待 Docker 首次探针并重新验证通过；没有回滚健康的主控。发布前后用户和网络设置文档校验和一致，未重置密码、地址池或内网 CIDR。
- 仅将上海节点更新至已校验的 `0.3.39`，保留旧二进制可回退，并移除本次临时目录环境覆盖，确认新默认目录即可启动。双栈 UDP/TCP 53 正常监听，更新后 16 组查询全部通过，最高约 69.5 ms；其他节点未自动更新，上海既有自定义上游未被默认值覆盖。
- 客户端升级包：`artifacts/client-updates/Pmman-Network-0.1.19-alpha-x64-update.exe`，3701443 bytes，SHA-256 `1e62f41743373375a2b8bdceda752a017135c5cb839841e27e576e4b9c7cd623`。package/Tauri/Cargo 版本已同步，安装器 ProductVersion 与 FileVersion 均为 `0.1.19-alpha`；仅构建升级包，未代管理员创建客户端发布版本。
- 本轮未启动/安装 Windows 客户端，也未执行客户端自动升级或隧道内流量验收；节点本机查询、单元夹具不冒充 Windows 客户端公网验收。节点 DNS 修复已独立生效，不要求先升级客户端。

## 隧道停止超时与主控诊断报告入口 — 2026-10-18

- 读取生产报告 `diag_32cc902b-1772-46de-b4f1-1c452eabd277`：上传时为 `0.1.19-alpha`、已连接上海节点；保留记录显示本次真实握手成功、安装约 1.44 万条分流路由，但没有停止超时现场。不能用这份报告断言超时的唯一根因。
- 代码审查发现旧清理只判断 `sc.exe query` 退出成功与否，命令查询超时被当作服务仍存在，且不能区分停止/待删除。新增 Windows SCM 清理模块，精确处理 1060/1072、stop pending、stopped、访问错误，停止后删除并释放服务句柄，确认服务不存在才继续安装。SCM 调用在受串行保护的有界工作线程执行，超时工作尚未退出时禁止新清理/安装竞态；不强杀进程、不操作其他隧道。
- 清理日志新增状态、PID、checkpoint、wait hint、Windows 退出码和耗时。批量路由首次成功不再逐条落日志，保留安装摘要和错误/重试，沿用日志空间上限，避免分流路由刷掉故障记录。
- 主控新增管理员诊断报告列表和下载接口；后台“运维管理 → 客户端日志”支持按用户/报告编号搜索、分页、上报时间和大小、认证下载。兼容现有报告目录；限制下载路径、拒绝符号链接，下载设置 attachment/nosniff/no-store。普通用户不能查看或下载其他用户日志，404 下载错误保留后端实际说明。
- 浏览器夹具验证列表、搜索空状态、真实浏览器文件下载和已清理报告错误；桌面/390px 窄屏截图保存在 `output/playwright/admin-diagnostics-*.png`。同步修复新增菜单后小高度侧栏底部遮挡，菜单独立滚动。夹具不涉及生产管理员登录或 WireGuard 实机验收。
- Windows MSVC service 全量 45/45 通过（新增 5 项清理状态机测试），Admin 格式/lint/typecheck/build 通过；主控新增路径/权限/现有报告下载/清理后 404 测试通过。完整主控回归与发布结果见后续记录。
- 构建升级包 `artifacts/client-updates/Pmman-Network-0.1.20-alpha-x64-update.exe`，3705005 bytes，SHA-256 `e25a3785a8f9a7a572647da6696da0d55a82bd6962aeb30c4dd3bf3e57776b24`；ProductVersion 为 `0.1.20-alpha`。未在宿主机启动、安装或强杀客户端；尚未复现用户的停止超时，需升级后观察真实清理结果。
- 本地 WSL 完整主控 48/48、节点 19/19 通过；最终文件读取/缓存策略调整后诊断接口专项 2/2 再次通过。浏览器实际收到 `.log` 下载，并确认 JSON 404 在 Blob 下载请求中显示具体保留策略错误；未用夹具代替后端授权测试。
- 生产主控已通过标准脚本发布 `0.3.40`，公网健康、Compose 主控/PostgreSQL 健康、资源 SHA-256 验证通过。日志页嵌入资源公网返回 200，未登录访问列表和下载均返回 401；主控容器可读取本次报告，发布前后报告 SHA-256 一致。当前留存的管理员验收密码已过期，未修改密码或伪称完成生产管理员登录/下载验收。
- 发布前后 `users`、`network_settings` 文档校验和一致；没有修改用户、地址池、内网 CIDR 或其他节点配置，没有向运行节点下发自动更新。回滚备份为 `/opt/pmman-network-dev/release-backups/20261017-213106-0.3.40-d433c17d`。升级包仅构建，未代管理员创建客户端发布版本。

## 无 WebView2 全量客户端安装包 — 2026-10-24

- 使用当前 `0.1.20-alpha` 代码通过 MSVC 构建 Windows x64 客户端、后台服务、前端和 NSIS 安装器；构建脚本与源配置恢复正常，未改动其他未提交功能改动。
- 交付包为 `artifacts/windows-installer/Pmman Network_0.1.20-alpha_no-webview2_x64-setup.exe`，包含 WireGuard 运行时，但不包含 WebView2 安装程序；安装时会检查目标系统已存在 WebView2 Runtime，缺失则中止并提示安装。
- 包体大小 `6,355,702` bytes，SHA-256 `8B6F065F4D70E6BCFB1281A2085150A666F5D33445F38F3567A6E31ED6630876`；安装器文件版本和产品版本均为 `0.1.20-alpha`。
- 本轮仅构建和静态校验安装包，未在宿主机安装或启动客户端，也未进行真实 WireGuard/公网验收。

## IPC 单连接传输隔离模块 — 2026-10-24

- 新增 `service/src/ipc_transport.rs`，`IpcTransport::try_spawn` 接收通用 Tokio `AsyncRead + AsyncWrite` 连接和类型化同步 handler，适用于 Windows NamedPipeServer 与 UnixStream。模块先独立验证，随后已接入 `main.rs` 的两个监听器，最终集成结果见本页顶部。
- 默认共享并发上限 16，接入时立即获取 permit；超额连接记录 Busy 并关闭，不创建等待任务。总请求读取期限 3 秒，响应 write_all 与 flush 共用 5 秒期限；单请求最多 64 KiB，包含 LF/CRLF。JSON 解析、handler 和响应序列化均运行在 spawn_blocking 内；即使连接任务被取消，阻塞 worker 仍持有 permit 直到实际退出，防止工作线程持续累积。未设置无法可靠取消同步工作的 handler deadline；handler 内部外部操作仍须自行设置期限。
- 连接错误仅记录固定分类或 io::ErrorKind，不保留/格式化请求体、serde 错误正文、OS 错误正文或 panic payload。畸形、空白、不完整、超大和超时请求关闭连接，不调用 handler；应用业务错误仍由正常协议响应表达。日志回调需快速且不 panic，应用进程自己的 panic hook 应独立避免敏感内容。
- 模块头部给出接入方式：循环外创建同一个 transport；Windows 首个实例使用 first_pipe_instance(true)，接受当前连接后保留该实例，先以相同 ACL 创建 first_pipe_instance(false) 的后继实例，再将当前连接交给 try_spawn。不要在监听循环等待返回的 JoinHandle，也不要用 `?` 将单连接拒绝传播为监听退出；Unix 保留原有 socket 权限并直接 dispatch 已接受连接。
- 模块初期通过独立入口验证 14/14；接入主程序后移除重复入口，测试由 `cargo test --locked --manifest-path service/Cargo.toml` 统一运行，并新增实际 Windows 临时 Named Pipe 后继实例测试。覆盖空/慢连接、总读期限、64 KiB 边界及超大、JSON/schema/UTF-8 错误、单请求与 CRLF、不读响应及 flush 超时、错误脱敏、共享并发拒绝、取消后 worker 占位、handler panic 恢复及慢 handler 不阻塞另一请求。
- 本轮仅运行获准的本机单元夹具；未启动/安装宿主机客户端或服务，未访问生产环境，未将单元结果视为 Windows Named Pipe 实机或公网验收。

## 状态轮询失联保留最后状态 — 2026-10-24

- 调整 `src-tauri/src/lib.rs` 的 get_status：IPC 请求失败及后台请求线程异常均只更新 message，提示后台服务暂时不可达、正在显示上次已知状态；保留 state、tunnelMode、认证和其他已知快照字段，避免仍连接时主界面与托盘误切换为“连接”。成功取得服务快照后仍整体更新。不增加 UI 协议字段。
- 加载 vcvars64.bat 并启用静态 CRT 后，`cargo check --locked --manifest-path src-tauri/Cargo.toml --lib --target x86_64-pc-windows-msvc` 通过。仅进行 Cargo 编译检查，未执行或终止宿主机当前运行的 client/service，未运行打包脚本或修改 precheck，未访问生产环境；运行中的应用尚未加载本次源码修改。
