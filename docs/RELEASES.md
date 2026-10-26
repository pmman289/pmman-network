# PMMan 发布与 CI/CD 规范

## 发布边界

发布分为两个互不耦合的产品线：

1. 服务端 release 同时包含主控和节点。两者必须使用同一个 `releaseVersion`、同一个 `x86_64-unknown-linux-musl` 目标；节点资源也从这份 release 产生。
2. Windows 客户端 release 独立于服务端 release。客户端轻量更新包由本地 Windows 构建，主控只负责保存、签名、发布和提供下载，不会因为客户端发布而重启主控或节点。

远程 `txgz` 只执行校验、原子替换、重启和健康检查，禁止在远程机器编译 Rust、前端或客户端。

## 产物目录

服务端构建产物：

```text
artifacts/server-releases/<version>/
├── pmman-control-plane
├── pmman-node
├── pmman-node-<version>       # 主控提供给节点自更新的资源
├── release.json               # 主控、节点、资源的大小和 SHA-256
└── pmman-server-node-<version>.tar.gz
```

`pmman-node` release 内嵌本地构建的静态 `pmmandns`（Linux x86_64）。节点首次
收到主控授权时会把它写入 `/var/lib/pmman-node/pmmandns`，按主控下发的
`geosite.dat`、国内/国外上游和规则生成配置并启动；配置或版本变化会安全重启
该子进程。目标节点不需要另外安装 pmmandns，且主控发布节点资源时会随 node
二进制一起更新。

客户端构建产物：

```text
artifacts/client-updates/Pmman-Network-<version>-x64-update.exe
artifacts/client-release-<channel>-<version>.json
```

客户端更新包不携带首次安装所需的 WebView2 和 WireGuard 运行时。客户端安装更新时使用当前运行程序的父目录作为安装目录，以 NSIS `/S /D=<现有目录>` 静默原地替换，用户不会重新看到安装向导；per-machine 安装仍可能显示一次 Windows UAC 确认。

## 服务端发布

本地构建并生成不可变产物：

```powershell
.\scripts\build-server-release.ps1 -Version 0.3.0
```

没有构建时可跳过测试，但仍会重新校验和打包本地构建产物：

```powershell
.\scripts\build-server-release.ps1 -Version 0.3.0 -SkipTests
```

发布阶段只接收已经存在的目录，不会触发构建：

```powershell
.\scripts\publish-server-release-txgz.ps1 -Version 0.3.0
.\scripts\verify-server-release.ps1 -Version 0.3.0
```

首次创建项目专属 txgz 服务时，兼容入口只创建主控 systemd 和资源目录；不会创建预置节点、节点 token、节点接口或节点 systemd：

```powershell
.\scripts\deploy-txgz.ps1 -Version 0.3.0
```

之后不要重复使用初始化入口；直接构建并发布即可。发布器行为如下：

- 本地校验三份二进制与 `release.json` 的大小/SHA-256；
- 上传到 `/opt/pmman-network-dev/.staging/`，不覆盖当前版本；
- 远端再次校验后备份当前主控和节点资源；
- 只操作项目主控 service；不读取、覆盖或自动注册节点记录；
- 原子替换主控和节点资源清单，重启主控；
- 主控 `/healthz`、systemd 状态和 `/v1/version` 通过后自动完成验收；
- 节点必须由管理员在主控中创建后单独部署，发布器不对节点做在线升级。

可以只检查本地产物而不产生远端变化：

```powershell
.\scripts\publish-server-release-txgz.ps1 -Version 0.3.0 -DryRun
```

## 客户端发布

先独立构建 Windows 轻量更新包：

```powershell
.\scripts\build-client-update.ps1 -Version 0.3.0
```

发布脚本必须明确消费一个已经生成的 `.exe`；它不会隐式调用构建脚本：

```powershell
.\scripts\publish-client-update-txgz.ps1 `
  -Version 0.3.0 `
  -Channel stable `
  -ArtifactPath .\artifacts\client-updates\PMMan-Network-Client-0.3.0-x64-update.exe `
  -ReleaseNotes '修复连接稳定性','改进更新回滚'
```

发布器在本地 WSL 使用受保护的 Ed25519 私钥签名清单；私钥不上传。主控采用版本目录和临时文件 `rename` 发布，最后才切换 stable/beta 的 `manifest.json` 指针。验证发布包与公网下载：

```powershell
.\scripts\verify-client-release.ps1 `
  -Version 0.3.0 `
  -Channel stable `
  -ArtifactPath .\artifacts\client-updates\PMMan-Network-Client-0.3.0-x64-update.exe
```

客户端发布不调用服务端发布器、不替换服务端二进制、不重启节点。反过来，服务端发布也不改变主控中已经发布的客户端版本目录。

## CI 阶段建议

推荐将流水线拆为四个 job：

1. `build-server`: 在本地/受信任 Linux 构建环境执行 `build-server-release.ps1`，上传整个版本目录和 SHA-256 manifest。
2. `publish-server`: 下载同一不可变目录，执行 `publish-server-release-txgz.ps1` 和 `verify-server-release.ps1`，禁止 checkout 后重新编译。
3. `build-client`: 在 Windows MSVC 构建完整安装器和轻量更新包，上传更新包。
4. `publish-client`: 仅执行 `publish-client-update-txgz.ps1 -ArtifactPath ...` 和 `verify-client-release.ps1`。

凭据、签名私钥和 txgz 部署元数据只允许存在于被 `.gitignore` 忽略的 `artifacts/` 或发布机密存储；日志中不得打印 token、密码、私钥或用户数据。
