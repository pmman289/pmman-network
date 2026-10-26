# Windows 客户端前端规范

## 设计与工程

- Vue 3 / strict TypeScript / Tauri 2，使用 `@lucide/vue` 的统一矢量图标；资源本地打包，不依赖公网字体或 CDN。
- `src/App.vue` 组织登录、连接、节点、设置与详情弹窗；`src/types/client.ts` 定义真实 IPC 状态契约，`src/composables/useClient.ts` 管理请求与状态，`useTheme.ts` 管理外观，`src/styles/client.css` 维护统一设计变量。
- 以单一明确连接操作为中心：待连接、连接中、已连接、断开中、失败均有文字反馈，不只用颜色。节点信息和高级详情分层呈现，不在首页堆叠技术参数。
- 默认窗口 1040 × 760，最小 760 × 620。小窗口可纵向滚动，不能横向溢出；浏览器 390px 额外验证响应式布局。保留系统标题栏、窗口操作与系统缩放行为。
- 支持浅色/深色/跟随系统，主题使用共享 token；字体优先 Windows 系统字体，标题、正文、说明分层，统一圆角、边框、间距和交互状态。
- 图标按钮提供可读名称，输入框有 label，使用原生表单支持 Enter；导航提供当前页标识和焦点迁移。弹窗使用原生 modal dialog，限制焦点并支持 Escape，提交期间禁止重复操作。支持可见键盘焦点和 reduced-motion。

## 真实状态与交互

- 邀请注册与账户邀请窗口使用 `invitation_request` typed IPC，前端不直接请求 HTTP。邀请码创建结果只存在当前组件和一次 IPC 响应，不进入持久化或常规状态缓存；关闭窗口即清除。邀请操作错误不得覆盖连接状态。注册成功清空密码、确认密码及邀请码，返回登录页并预填用户名；不自动重试注册。策略、额度及发布边界见 `docs/INVITATIONS.md`。

- P2P 默认开启，设置页通过 `set_p2p` typed IPC 在线切换，偏好由服务保存在当前用户配置目录。前端不缓存 peer 身份/密钥，也不自行判断直连。peer 状态使用 `idle`、`probing`、`direct`、`relay`；打洞期间明确显示“中转承载”。关闭 P2P 不断开隧道，双方均允许时才能直连。
- Windows 服务会异步尝试 UPnP IGD，为 P2P 专用随机 UDP socket 申请 5 分钟短期 UDP 映射并周期续租；映射失败或网关不支持时自动降级到节点反射地址和认证 relay。UI 可展示服务返回的 `p2pMessage`，但不得把 UPnP 成功当作业务连接成功，真实成功仍以 WireGuard 握手和业务流量为准。
- 新 Windows 待测程序必须通过 `hyperv-test-executables` SYSTEM broker；多客户端使用 request group 和明确网络 profile。构建工具、单元测试和可信浏览器验证可以本地运行。`scripts/run-p2p-hyperv.ps1` 默认在三个同时工作的隔离 Windows 中安装规范 NSIS 产物、连接公网主控/不同节点并断言真实 peer 业务，包括 IPv4/IPv6 TCP 和 UDP、本轮准确公钥的新握手、未访问 pair 零握手，不将浏览器夹具当成网络证据。

- 所有登录、节点信息、连接、配置、心跳与撤销经已有 Tauri/Named Pipe 服务访问主控。前端不直接请求节点或 HTTP API；UDP RTT 与真实 WireGuard 握手仍由服务处理。
- IPC 监听须在分派请求前保留下一管道实例；阻塞业务与状态读取分离。共享快照锁不得覆盖 HTTP、SCM、路由操作或 worker join。变更操作单路执行且明确拒绝并发；请求读写有期限和大小限制，单连接异常不终止后台监听。
- 主控会话 401 与设备撤销分开处理：心跳、P2P 和上报使用共享认证上下文，DPAPI 恢复后最多重试一次；网络失败可重试，明确凭据失效则结束连接并要求重新登录。退出登录必须先完成设备撤销，再注销 bearer；迟到续期不得恢复已退出的账号。异步防火墙清理需在生命周期锁内确认没有替换连接。
- 客户端版本发布、签名、下载、安装替换与回滚遵循 `docs/CLIENT-UPDATES.md`；客户端更新接口不携带用户 bearer token。
- `state=connected` 且 `tunnelMode=wireguard` 才显示已连接。等待期间只有不确定进度和阶段说明，不能制造百分比、流量、握手或延迟数据。
- 真实连接或主控端口转发操作已成功时，后续 Windows 防火墙同步失败仍返回成功快照，不更改连接状态、不释放映射。独立 `portForwardFirewallWarning` 以警告展示目标端口放行提示及可展开详情；普通状态刷新不能清除，后续同步成功才清除（退出账户时重置）。记录 `firewall/sync_failed`、`sync_recovered` 及操作类型，不能将警告归纳成连接失败。浏览器回归脚本 `scripts/ui/verify-firewall-warning.js` 只验证 IPC 界面，不执行防火墙命令。
- Windows UI 使用 `Global\\PmmanNetworkClient` 命名互斥体保持单实例；第二次启动不会再启动 UI 或后台服务。更新器健康检查为短进程，明确绕过 UI 单实例互斥体。
- 全局操作锁防止多次连接/登录/切换；连接时保持导航和外观可用，锁定节点和流量修改。退出账户明确提示会断开当前连接。
- 5 秒状态同步不会重叠；请求版本防止旧读取覆盖新结果。连接过程中只读取后台 `connecting` 阶段进度，操作完成时推进请求版本，丢弃迟到的进度响应；其他变更操作期间暂停同步。组件卸载后清理计时器并拒绝写回旧结果。
- 注册遇到明确的地址/旧节点清理 409 时，保持“正在连接”，显示“正在切换地址，等待旧连接清理…”，使用同一公钥、节点和双栈模式每 2 秒重试，注册阶段总预算 90 秒，单请求不超过 12 秒及剩余预算。其他冲突、认证、网络错误不自动重试。等待期间可取消，不绕过主控的旧 peer/回程路由/映射清理确认；若取消时注册成功响应已在途中，先撤销该注册，不安装隧道。取消不作为连接失败，撤销失败则明确报错。P2P worker 仅在注册成功后启动；等待次数、耗时和最终结果写入诊断日志，不记录令牌或密钥。
- 捕获 Promise/IPC 异常，始终恢复按钮；请求失败保留已有数据，错误可关闭并可重试。节点刷新/选择 IPC 错误必须向前端拒绝，不能把失败当作成功状态。
- 流量选项仅展示主控授权能力，服务端继续校验。设置失败将原生 select 回退到真实值；连接后不能修改。节点没有测量结果时显示待探测，不填假 RTT。
- 地址、路由、DNS 和诊断信息来自真实快照；复制失败有手动复制提示。连接计时从本次前端观察到已连接时起算，不声称是服务持久化的会话时长。
- WireGuard 配置使用 `Table = off`。`AllowedIPs` 只负责加密 peer 选择（包括用于接收全球 IPv6 来源的 `::/0`）；网关、主控授权的内网/分流/全局路由、IPv6 低优先级默认路由和 P2P 主机路由由 service 的 `route_manager` 通过 Windows IP Helper API 管理。普通 IPv6 默认路由保留，非全局模式的 Pmman `::/0` 使用高 metric，仅作为分配地址回包的兜底；连接失败、断开、心跳撤销和 P2P peer 移除都会清理本次 service 拥有的路由。
- 隧道 MTU 在连接时按所选节点的实际公网 endpoint 计算：优先用 Windows DF/ICMP 探测做有界路径 MTU 二分搜索，再扣除 WireGuard 外层 IPv4/IPv6 开销，并保证内层 IPv6 至少为 1280；ICMP 不可用时读取当前默认出口接口 MTU，仍不可用则记日志并回退至 1280。低于 IPv6 最小承载能力的已测路径会明确拒绝连接，不会建立表面成功但 IPv6 不可用的隧道。MTU 决策及回退来源写入 `service.log`。
- 境外分流使用客户端内置的 `client_builtin_v1` 双栈 cnipgen 快照。主控只下发内网 CIDR 和授权模式，不再要求节点配置海外路由；IPv4/IPv6 海外前缀随客户端版本发布，运行时不下载规则。规则加载、哈希、地址族、特殊用途排除和默认路由禁止检查在连接前完成，WireGuard `AllowedIPs` 与 Windows 路由由同一份合成结果生成。节点上的 pmmandns 域名分流是独立机制，不参与 IP 选路。
- DNS 域名分流由客户端本机 `127.0.0.2` resolver 执行：主控认证接口提供 geosite 版本元数据和资源，国内分类发往连接前捕获的系统 DNS，海外分类及未分类域名发往所选节点 DNS。海外解析失败时不回退到本机 DNS，避免错误结果和查询泄漏；DNS 转发仅经过 WireGuard 数据面访问节点，不向节点发送用户令牌。DNS 域名分流与 cnipgen IP 路由分流相互独立。
- 密码只在登录和“网络账户 → 修改密码”表单暂存，提交成功或退出后清空；用户 token 仍由服务持有，不写浏览器存储。改密成功后由 Windows service 使用 DPAPI 更新本地自动登录凭据，当前隧道和会话保持不变。仅外观偏好存 localStorage，存储不可用不阻塞启动。

## 构建与验收

```powershell
& .\scripts\build-client.ps1
& .\scripts\prepare-standalone.ps1
& .\scripts\verify-txgz.ps1
```

前端在本地 WSL `pnpm build` 完成类型与生产构建，Windows 使用 MSVC。无需为纯 UI 改动重启公网服务。

- Playwright 验证登录、导航、节点自动/手动选择、模式、主题、详情、退出、空状态、失败恢复、权限及轮询竞态，截图保存在 `output/playwright/client-v2-*.png`。
- `scripts/ui/client-fixture.js` 是仅由测试工具注入的浏览器夹具，应用不导入它，也不打包；夹具状态不得作为真实网络验收依据。
- 实际 Windows release 启动和公网 WireGuard 回归必须独立验证，使用 `scripts/verify-txgz.ps1` 核对真实公钥、双方新握手、双栈、DNS、断开清理与远程撤销。
- 正式发布使用 per-machine NSIS 安装器。WireGuard for Windows 仅作为 Pmman 内部驱动/隧道运行时，Pmman 后台服务负责生成密钥、创建临时隧道和清理路由；安装后删除 WireGuard 快捷方式及新建的卸载列表入口，用户只看到 Pmman Network。Microsoft Edge WebView2 Runtime 由安装器自动处理，安装包内携带官方 Evergreen Standalone x64 离线安装器；没有 WebView2 的机器也能在无外网条件下完成安装。安装器资源 URL 和 SHA256 由 `scripts/download-windows-runtime.ps1` 固定校验，下载失败或哈希不匹配会直接终止构建。不含 localhost 开发地址、浏览器夹具或远程调试端口。保留 Common Controls v6 与 requireAdministrator 清单。
- 桌面生命周期采用通知区常驻模型：点击主窗口关闭按钮只隐藏窗口，不断开隧道、不退出后台服务；Pmman Network 托盘图标的左键重新打开主界面，右键菜单提供实时状态、连接/断开和退出。只有托盘菜单“退出 Pmman Network”才会撤销当前会话、停止本次客户端拥有的后台服务并结束进程。托盘连接操作与主界面共用同一 IPC 状态和并发锁，进行中的连接不会重复提交。
- WebView2 检测以 `pmman-client.exe --check-webview2` 返回码为准：创建窗口和后台服务前调用 Tauri 自身加载器，0 表示 Runtime 可加载，2 表示不可用。客户端启动前会扫描 `Program Files (x86)\Microsoft\EdgeWebView\Application`、`Program Files\Microsoft\EdgeWebView\Application` 和用户目录，选取实际包含 `msedgewebview2.exe` 的最高版本并设置 executable folder；这样可容忍 Evergreen 安装器把注册表 `pv` 暂时写成与实际目录不同的补丁版本。诊断注册表时正确 App GUID 为 `{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}`。NSIS 不支持跨目录的 `Application\*\msedgewebview2.exe` 通配检查。
- `artifacts/standalone` 是开发验收目录包，不是零准备发行物；它要求机器已经有 WireGuard 和 WebView2。零准备 Windows 验收必须使用 `scripts/build-windows-installer.ps1` 生成的 NSIS 安装器。
- 后台服务路由诊断日志位于 `%LOCALAPPDATA%\Pmman Network\service.log`。其中记录 WireGuard 适配器索引、路由添加/删除、Windows 返回码和重试过程，不记录密码、Bearer Token 或私钥；遇到“Pmman 路由配置失败”时可将该文件提供给维护人员。
- “连接详情 → 上报诊断日志”由后台服务整理最近的 service.log、滚动日志和更新日志后上传主控。单次报告最多 768 KiB，服务日志按 512 KiB 轮转并最多保留 3 个历史文件，更新日志最多保留 256 KiB；上传包不包含登录状态、配置文件、密码、Bearer Token、WireGuard 私钥或数据包内容。主控按账户每 60 秒限流，并为每个账户最多保留 5 份报告，成功后显示报告编号。
- 管理员在主控“运维管理 → 客户端日志”按报告编号搜索并下载。批量路由安装仅记录摘要、错误和恢复重试，不逐条记录首次成功，避免一次分流连接挤掉较早的故障。隧道清理直接使用 Windows SCM，区分不存在、停止中、已停止和待删除；停止后删除专属服务并关闭句柄，确认不存在才允许重装。清理运行在有界工作线程中，超时后未结束的清理会阻止下一次安装，不强杀其他 WireGuard 隧道。状态、PID、checkpoint、wait hint、Windows 错误码和耗时写入诊断日志。
- 端口转发创建后，Windows service 会按当前租约的目标端口和 TCP/UDP 协议维护专属防火墙规则。规则只允许进入 `pmman-client` WireGuard 接口，并限制到目标端口；不会开放所有入站流量。列表刷新、创建、释放、断开、服务启动清理和服务端撤销都会同步或清理这些规则。若系统策略阻止配置，客户端会明确提示“Windows 防火墙规则配置失败”以及手动放行范围；映射租约保留，用户修复防火墙后可继续使用。
- Windows 客户端脚本在本机 MSVC 环境构建前端、service、Tauri 和 NSIS；不要让 Windows 与 WSL 共享同一份前端 `node_modules`，否则 Linux pnpm 可能移除 Tauri CLI 的 Windows 原生可选绑定。WSL 仅用于主控/节点 Linux 构建。
- `scripts/build-client-update.ps1 -Version <semver>` 会从更新配置生成按版本覆盖的临时 Tauri 配置，确保 service、客户端诊断版本、NSIS 文件版本和主控发布清单一致；临时配置只写入 `artifacts/`。
- 当前可验收文件为 `artifacts/windows-installer/PMMan-Network-Client-Setup.exe`。安装阶段不需要联网获取 WebView2；完整离线安装器会在缺少 Runtime 时由同一安装钩子以 `/silent /install` 安装。公网主控、节点探测和 WireGuard 业务连接仍然需要网络。
- 可使用 `scripts/build-windows-installer.ps1 -WithoutWebView2` 构建不携带 WebView2 的全量安装包；该包仍包含 WireGuard 运行时，但安装前要求系统已经安装 WebView2 Runtime，缺少时安装器会明确中止并提示，不会联网下载。
- 界面截图不得包含密码/凭据；工具日志不要输出认证参数。验收结果及限制写入 `docs/PROGRESS.md`。
