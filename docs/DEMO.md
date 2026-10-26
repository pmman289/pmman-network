# 公网 WireGuard 手动 Demo

当前及后续服务端运行环境为 `ssh txgz`。本地 Windows 与 WSL 负责构建，
旧 WSL 共享接口 / HTTP 节点实验已停用；`scripts/start-real-lab.ps1` 会提示使用公网流程。

## 构建与部署

```powershell
& .\scripts\deploy-txgz.ps1 -Version 0.3.0
& .\scripts\build-client.ps1
& .\scripts\prepare-standalone.ps1
```

这是开发用 standalone 验收流程，Windows 需预先安装 WireGuard for Windows 和 WebView2 Runtime；零准备用户请使用 `scripts/build-windows-installer.ps1` 生成的 NSIS 安装器。双击 `artifacts/standalone/pmman-client.exe`，
接受标准 UAC 提权，使用忽略目录 `artifacts/txgz-credentials.json` 中的客户端账户登录。
默认自动选择最低 UDP RTT 的节点，也可手动选择 TXGZ-A/B；点击连接才会生成密钥和安装隧道。
只有本次 peer 完成真实握手才显示已连接。客户端仅向主控 HTTPS 获取信息，
与节点直接通信只有 UDP 探测及 WireGuard 数据隧道，节点不提供 HTTP API。

管理入口：[主控管理面板](https://network.pmman.tech/admin)。
节点接入在后台创建专属配置，详见 [NODE-ONBOARDING.md](NODE-ONBOARDING.md)；
公网地址与部署约束见 [PUBLIC-DEMO.md](PUBLIC-DEMO.md)。

## 验收

```powershell
& .\scripts\verify-txgz.ps1
```

脚本自动请求 UAC，直接调用实际 Windows 后台服务。验收覆盖 UDP RTT/自动与手动节点选择、
Windows/节点公钥及双方新握手、双栈网关业务流量、回程路由、DNS、split/global、
管理员撤销与完整清理。日志保存在 `artifacts/verify-txgz.log`。
双栈 HTTP 测试目标是 WireGuard 隧道内部的业务网关，用于验证加密数据传输；
它不是客户端到节点的控制 API。

执行前关闭客户端和后台服务；完成后可再次双击 standalone 程序手动查看。
程序、后台服务和 `pmman-client.json` 应一起保留，后者指向公网主控。
发布版嵌入 Vue 资源，不使用 localhost 开发服务器，也没有黑窗口。
