# Pmman Network

可部署的真实连接客户端：Windows Tauri 客户端通过自动启动的后台服务注册 WireGuard 公钥，安装真实 WireGuard 隧道并获取服务端分配的 IPv4/IPv6。主控、节点和客户端发布链路均支持生产配置持久化与签名校验；示例脚本中的账户、节点和地址仅用于本地验收，不会自动注入生产环境。

## 手动验收

### 从源码构建

源码仓库不包含 Windows 服务可执行文件、WireGuard 安装器或 WebView2 安装器。安装 Rust MSVC 工具链、Visual Studio C++ Build Tools、Node.js 和 pnpm 后执行：

```powershell
pnpm install --frozen-lockfile
pnpm --dir admin install --frozen-lockfile
& .\scripts\build-windows-installer.ps1
```

构建脚本会先编译后台服务，再构建客户端；运行时依赖由 `scripts/download-windows-runtime.ps1` 下载并校验 SHA-256。构建输出和本地部署凭据不提交到 Git。

### 1. 使用公网主控和节点

在 Windows PowerShell 执行（首次创建 txgz 项目服务）：

```powershell
& .\scripts\deploy-txgz.ps1 -Version 0.3.0
```

服务端后续发布和客户端独立发布遵循 [发布与 CI/CD 规范](docs/RELEASES.md)。服务端发布先在本地生成主控/节点同步产物，再将同一份产物上传到 txgz；客户端发布脚本只接收已经构建好的轻量 `.exe`，由主控管理员 API 创建和签名版本，不会触发服务端重启。

现在及后续开发测试部署到 `ssh txgz`。主控管理入口为 [https://network.pmman.tech/admin](https://network.pmman.tech/admin)，主控仅监听 txgz 回环地址 `127.0.0.1:8787`，由运维手动配置 nginx 反代。节点 A/B 分别使用公网 UDP `1.14.226.49:51830` 和 `1.14.226.49:51831`，拥有独立 WireGuard 接口、公钥和返回路由表。Linux 服务在本地 WSL 测试和构建静态 musl 产物后上传；Windows 客户端在本地 MSVC 构建，远端只运行服务。完整部署说明见 [docs/PUBLIC-DEMO.md](docs/PUBLIC-DEMO.md)。

### 2. 安装并运行客户端

正式交付使用 Windows 安装器。它会在一次标准 UAC 确认后自动安装/校验 WireGuard for Windows 和 Microsoft Edge WebView2 Runtime，并创建开始菜单快捷方式：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\build-windows-installer.ps1
Start-Process ".\src-tauri\target\release\bundle\nsis\Pmman Network_0.0.1-alpha_x64-setup.exe"
```

安装后用户无需预装 Rust、Node.js、PowerShell 模块、WireGuard 或 WebView2，直接双击 Pmman Network 即可使用。卸载客户端不会删除 WireGuard 驱动，以免影响其他隧道。

开发验收包仍可这样准备：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\prepare-standalone.ps1
Start-Process .\artifacts\standalone\pmman-client.exe
```

直接双击 `pmman-client.exe` 即可。程序会显示标准 Windows UAC 确认框；用户点击“是”后即可正常使用，不需要打开 PowerShell 或手动启动服务。首次打开后使用账户登录，再选择“自动选择（UDP 最低延迟）”或指定节点，最后点击“连接内部网络”。只有本次 WireGuard peer 完成真实握手后才会显示已连接。程序旁的 `pmman-client.json` 指向公网主控，请一起保留；`PMMAN_CONTROL_PLANE` 环境变量可覆盖此配置。standalone 仅用于已经安装运行时的开发验收，不承诺零准备机器可直接运行。公网随机账户密码保存在忽略目录的 `artifacts/txgz-credentials.json`，客户端账户字段为 `client_user`/`client_password`，管理账户为 `admin_user`/`admin_password`。预期地址为 `10.88.0.10/32` 和 `fd88:1000::a/128`。

管理员可在公网管理入口登录，管理账户启用状态、重置密码、设备撤销、IPv4/IPv6 地址池、独立内网 CIDR 列表，以及节点返回路由表和客户端选择开关。地址池只用于分配客户端地址，内网 CIDR 列表会作为所有连接的 WireGuard AllowedIPs。用户、节点和网络配置保存到主控的专用持久化文件，更新或重启不会回退到默认地址池；活动会话和运行时设备状态仍按设计在重启后重新认证/重新接入。管理页表格在窄窗口下可横向滚动。

### 3. 验证公网连接

```powershell
& .\scripts\verify-txgz.ps1
```

先关闭客户端及后台服务。验收脚本自动请求 UAC 提权，调用真实 Windows 后台服务，核对公网两端公钥和新握手、自动与手动节点选择、双栈 HTTP、DNS、分流/全局模式及断开清理，结果写入 `artifacts/verify-txgz.log`。旧 WSL 实验脚本已停用，统一使用公网部署和验收脚本。

## 项目结构

```text
server/       主控管理 API、地址分配、节点接入和 Linux 网关配置
admin/        Vue 3 / Element Plus 独立管理后台
service/      后台网络服务、真实 WireGuard 安装和 Named Pipe IPC
protocol/     客户端和服务共享协议
src/          Vue UI
src-tauri/    Tauri Windows 客户端
scripts/      构建、真实实验环境和打包脚本
docs/         架构、Demo 和开发进度
```

管理后台采用侧栏导航、面包屑、分页表格和编辑弹窗，包含概览、用户、节点、设备租约及地址池配置。
开发规范见 [docs/ADMIN-DEVELOPMENT.md](docs/ADMIN-DEVELOPMENT.md)。后台构建输出会嵌入主控二进制；
直接运行 Cargo 前须先在 `admin/` 执行 `pnpm install --frozen-lockfile` 和 `pnpm build`，部署脚本已自动完成此步骤。

## 1:N 服务端模型

- 主控（master）负责账户、认证、地址池、租约、节点注册/心跳和管理后台。
- 节点（node）仅提供 WireGuard 隧道和 UDP challenge echo 探测，主动向主控注册和同步配置，不提供 HTTP 服务、不接收用户凭据。
- 节点可通过 `PMMAN_ROUTE_TABLE` 配置指定 Linux 路由表；客户端租约的 IPv4 `/32` 和 IPv6 `/128` 会实时写入该表，撤销或断开时删除，供 OSPF/FRR 重新发布回程路由。
- 客户端登录、节点列表、配置获取、心跳和断开仅访问 HTTPS 主控；对候选节点并行发送三次带随机 nonce 的 UDP 探测，自动选择成功样本中位 RTT 最低的节点，也可手动指定。
- 节点每秒向主控拉取期望 peer 并上报安装结果。主控等待本次公钥与配置版本确认后才下发客户端配置；节点重启会恢复活动 peer，删除独立接口中残留的旧 peer。
- 管理后台「接入节点」预建记录并生成绑定 ID 的凭据，可下载节点配置、取消待接入记录。节点接入记录和管理员节点设置持久化；首个心跳后才进入客户端候选列表。详见 [节点接入文档](docs/NODE-ONBOARDING.md)。
- 双栈客户端地址池排除 IPv4 网络/广播地址和预留网关，IPv6 排除零地址及网关；默认 `/24` 的客户端容量为 253，`/31` 和 `/32` 不作为客户端池。
- 公网 Demo 的两个节点位于同一主机，使用独立接口 `pmman-dev-a`/`pmman-dev-b` 和路由表 `51830`/`51831`，验证 1:N 接入流程；尚未验证跨主机故障隔离。

- 主控为用户保存 IPv4/IPv6 全局和境外分流四项能力授权；客户端登录后只显示已授权的模式。
- IPv4 与 IPv6 可以分别选择 internal、split 或 global。主控生成客户端配置：全局模式使用两条 /1 路由，分流模式使用节点注册上报的 PMMAN_IPV4_SPLIT_ROUTES_FILE/PMMAN_IPV6_SPLIT_ROUTES_FILE 内容。
- 只要建立内网连接，客户端会优先访问节点 WireGuard 网关上的 `pmmandns`。主控支持 mrouter 兼容的 `domestic_upstreams`、`foreign_upstreams`、`domestic_rules`、`foreign_rules` 和默认分组；规则按域名后缀匹配，最长后缀优先，未命中使用默认分组。节点无有效结果时返回失败，Windows 客户端再尝试连接前发现的本地 DNS。客户端只查询节点 pmmandns，不直接访问上游 DNS。

## 当前范围

- 用户必须点击连接才会触发注册和 WireGuard 安装。
- `tunnelMode=wireguard` 才是成功状态；不会返回模拟成功。
- 客户端内置 `requireAdministrator` 清单；双击时由 Windows 自动处理权限确认。
- 正式 Windows 安装器自动安装 WireGuard for Windows 和 WebView2 Runtime；standalone 开发包不携带驱动安装逻辑。
- 当前控制面使用 HTTPS Bearer Session；账户密码使用带随机盐的 PBKDF2-HMAC-SHA256 保存，旧哈希首次登录后升级，管理员重置/停用账户会立即使既有会话失效。节点凭据只通过节点专属请求头发送，节点没有 HTTP 管理面。正式生产仍应在部署层补充 TLS 证书轮换、审计日志、备份恢复和发布包代码签名策略。
