# Pmman Network PostgreSQL 部署与迁移

多地址池从 schema 5 开始使用独立关系表；数据迁移、API、分配及回滚规则见 [ADDRESS-POOLS.md](ADDRESS-POOLS.md)。旧网络文档继续保存内网 CIDR 和迁移前的兼容字段，不再作为新地址池的权威来源。

## 运行拓扑

主控和 PostgreSQL 由项目专属 Docker Compose 管理；节点不进入 Compose，继续在节点宿主机上运行项目专属 systemd 服务。主控容器只负责控制面，不直接操作 WireGuard、nftables、BIRD 或 pmmandns。

业务数据保存在 PostgreSQL。客户端安装包、节点二进制、geosite 文件和日志保存在主控挂载目录，数据库保存业务记录，以及这些文件的版本、路径、大小和摘要。客户端主动上报的诊断包保存于 `PMMAN_DIAGNOSTIC_DIR`，不写入数据库正文。

主控生产环境必须设置 `PMMAN_DATABASE_URL`。缺少 PostgreSQL 时主控拒绝启动，也不会回退到默认地址池、默认节点或 JSON 文件。

## 本地构建

Windows 主机只负责调度；主控和节点二进制在本地 WSL 使用 musl 目标构建，镜像在本地 Docker 构建：

```powershell
.\scripts\build-server-release.ps1 -Version 0.4.0
```

发布产物包含主控镜像 tar、主控二进制、节点二进制、节点资源和一次性迁移工具。txgz 不编译 Rust，只加载镜像并运行 Compose。

## 生产迁移

发布脚本会依次执行：

1. 备份旧 JSON、旧端口转发数据库、发布目录和 PostgreSQL；
2. 启动 PostgreSQL 并等待健康检查；
3. 运行带 `legacy-migration` 特性的 `pmman-migrate`；
4. 导入用户、节点、网络设置、端口入口、授权、租约和事件；
5. 校验重复主键和已有数据冲突；
6. 迁移成功后启动主控容器；
7. 通过 `/healthz`、管理员登录、节点心跳和公网 HTTPS 验证。

旧 `port-forward.db` 只作为一次性只读输入，主控运行时不链接 SQLite。

发布前会在回滚快照中保存 PostgreSQL plain SQL dump。若迁移或新镜像健康检查失败，发布脚本先停止项目主控、恢复数据库快照，再恢复 Compose 配置并启动主控；节点宿主机服务不会被发布脚本替换。

主控和端口转发存储使用短连接访问 PostgreSQL。数据库容器重启后，下一次请求会重新建立连接；`/healthz` 在连接不可用时返回 `503`，不会回退到 JSON、SQLite 或内置默认配置。

账号删除使用同一事务修改账号和设备文档、端口转发授权/租约、邀请授权/邀请码及审计，
事务提交前不修改内存或使现有会话失效。节点仍通过已有心跳完成规则回收。
Migration 4 移除邀请注册历史的用户名唯一约束，保留不可变 `user_id` 唯一性，并建立用户名查询索引；
这允许删除后重新注册同名账号，不删除或重写旧邀请历史。旧备份回滚仍需按发布周期保留与验证。

迁移工具发现 PostgreSQL 中已有内容且内容不一致时会停止，禁止覆盖生产数据。迁移切换会清空旧服务端内存会话；客户端仍可使用 Windows DPAPI 保存的凭据自动重新登录。

## 节点部署

节点由管理后台生成绑定节点 ID 的部署脚本。脚本在节点宿主机上安装独立 systemd 服务，节点通过主控公网 HTTPS 注册和心跳。主控只下发期望状态，不通过 SSH 或节点 HTTP API 管理节点。

## 运维约束

- 不修改宿主机已有 WireGuard、BIRD、路由、DNS 和网站服务；
- 不提交 `.env`、数据库密码、管理员密码或发布私钥；
- PostgreSQL 数据卷和发布文件目录必须纳入定期备份；
- 主控镜像升级不会覆盖节点配置和 PostgreSQL 数据；
- 回滚前必须确认目标镜像兼容当前数据库 migration 版本。
