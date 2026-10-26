# 公网主控部署环境

后续主控和节点开发测试部署到 `ssh txgz`，目录 `/opt/pmman-network-dev`。
构建尽量在本地完成：Windows 使用 MSVC，Linux 主控/节点在本地 WSL 构建静态 musl 产物。
远端只部署和运行，真实网络验收使用 Windows 本地客户端直连公网。

## 入口与隔离

| 组件 | 公网入口 | 本机监听 / WireGuard |
| --- | --- | --- |
| 主控与管理页 | `https://network.pmman.tech/admin` | `127.0.0.1:8787` |
| 节点 | 当前状态 |
| --- | --- |
| 节点资源 | 由主控保存，管理员创建节点后生成部署脚本 |
| 生产节点 | 默认无节点；不自动生成、不自动覆盖 |

节点不提供 HTTP/HTTPS 服务；原节点 8444/8445 入口及 18090/18091 监听已移除。节点二进制由主控
资源目录通过节点凭据提供 HTTPS 下载，更新由节点主动拉取并重启，不需要 SSH 或节点 HTTP API。
客户端只向主控获取信息、申请配置和撤销连接；节点每秒主动同步主控，主控等待安装确认后才返回配置。
自动选择通过三次随机 nonce 的 UDP 探测取中位 RTT，失败节点不会作为自动候选。

生产主控不管理任何预置节点，不改动现有 WireGuard 接口、BIRD 或网站服务。
管理员从主控创建节点后，节点自行向主控注册，客户端才会看到该节点。
主控通过运维手动配置的 nginx HTTPS 反代发布；主控进程自身仅监听 txgz 的 `127.0.0.1:8787`，公网地址为 `https://network.pmman.tech`。

nginx 反代的核心配置如下，证书路径按实际环境替换：

```nginx
server {
    listen 443 ssl;
    server_name network.pmman.tech;
    ssl_certificate /path/to/fullchain.pem;
    ssl_certificate_key /path/to/privkey.pem;
    location / {
        proxy_pass http://127.0.0.1:8787;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto https;
        proxy_read_timeout 30s;
    }
}
```

管理控制台已重写为独立 Vue 3 / Element Plus 应用：概览、用户管理、节点管理、
设备租约和地址池配置分别使用独立路由页面，支持搜索分页、校验弹窗、操作确认和窄屏导航。
开发约定见 `docs/ADMIN-DEVELOPMENT.md`。后台资源在本地构建后嵌入 Rust 主控，远端无需前端工具链。

节点管理新增「接入节点」入口，可创建绑定 ID 的一次性展示凭据、下载一键部署脚本和 systemd 环境配置，
并取消未使用的待接入记录。接入记录与管理员节点配置在主控重启后保留；旧 token 白名单已被替代。
地址池排除网络、广播和网关地址，默认 `/24` 的客户端容量为 253。详见 `docs/NODE-ONBOARDING.md`。

## 部署和客户端

```powershell
& .\scripts\deploy-txgz.ps1 -Version 0.3.0
& .\scripts\build-client.ps1
& .\scripts\prepare-standalone.ps1
```

双击 `artifacts/standalone/pmman-client.exe`。程序旁的 `pmman-client.json` 指向公网主控，
可通过修改 `control_plane` 切换环境。显式设置 `PMMAN_CONTROL_PLANE` 时优先使用环境变量。
配置文件损坏时客户端报错，不会悄悄切回 localhost。

公网部署只生成随机初始管理员密码，不创建测试用户或预置节点。管理员首次登录必须修改密码；节点令牌仅在主控创建节点时一次性展示。
节点主动向 HTTPS 主控注册，使用绑定节点 ID 的凭据。接入记录、节点配置和账户记录持久化，会话仍需重新登录。

## 自动验收

关闭普通客户端和后台服务，再运行：

```powershell
& .\scripts\verify-txgz.ps1
```

脚本使用正常 Windows UAC 提权，调用实际客户端后台服务，仅通过 HTTPS 主控申请配置，以 UDP 探测选择节点，通过 WireGuard
安装/握手建立连接。验证自动最低延迟、手动 A/B、双栈网关 HTTP、回程主机路由、分流/全局模式、
网关 DNS、IPv4 HTTPS 转发、管理员远程撤销，以及断开后的 Windows 服务和远端 peer/路由清理。
结果保存在 `artifacts/verify-txgz.log`，旧日志按时间归档。`-RevokeOnly` 可单独补验管理员撤销，
写入 `artifacts/verify-txgz-revoke.log`。SSH 只读状态检查关闭标准输入、限制每次进程等待为 20 秒，
超时最多重试两次；任何网络断言失败仍会终止验收。这是后台连接流程验收；GUI 按钮另行手动验收。

## 运行和更新

使用 `pmman-dev-*` systemd 服务运行并在故障后自动重启。部署脚本在本地 WSL 测试和构建
静态 musl Linux 产物，上传二进制和部署脚本，再重启项目服务，不在远端编译。
本地 WSL 需要 `musl-tools` 和 Rust `x86_64-unknown-linux-musl` target。
首次远端工具链准备曾因内存不足增加项目目录下的 1 GB `build.swap`，后续构建已迁回本地。

网关 `pmman-dev-gw` 提供 `10.88.0.1`、`fd88:1000::1` 的双栈 HTTP/DNS 探测，
项目独立 NAT 表 `pmman_dev_nat` 只匹配测试地址池，不修改既有 NAT 规则。
DNS 探测服务器依次尝试云内 DNS `183.60.83.19`、`183.60.82.98` 及 `223.5.5.5`，每个上游超时 2 秒，
用于验证隧道 DNS 的实际可达性。验收 SSH 固定公网 IPv4，并沿用原域名主机密钥校验，避免全局 DNS 改变后阻塞诊断。
分流前缀是验收夹具，尚未接入完整生产境外前缀表。
双栈隧道访问和 IPv4 公网转发分别验收；尚未验收 IPv6 公网出口。
