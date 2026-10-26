# 邀请注册

## 使用入口

- 客户端登录页的“使用邀请码注册”，注册后返回登录并预填用户名，不自动登录。
- 客户端账户窗口的“邀请用户”，查看资格、额度、邀请码和自己邀请的注册记录。
- 公开网页 `/register`，不加载后台布局，不读取管理员会话。
- 后台“访问控制 → 用户 → 邀请授权”，以及“邀请码”“邀请记录”。

普通用户默认没有邀请资格。管理员独立配置邀请授权：未使用码上限默认 10、每日生成上限默认 20、有效期默认 7 天，启用开关默认关闭。日额度按 UTC 自然日重置，页面显示对应本地时间。撤销不退还生成次数；降低配额不删除已有码，但达到任一上限后不能继续生成。

普通用户只能生成单次码，不能指定受邀权限、有效期或次数。管理员码支持单次或无限使用以及可选到期时间；无限使用不扩大受邀权限。撤销不可恢复，已使用单次码只读。所有列表保留历史记录，不提供物理删除。

## 授权规则

受邀权限是单独的 IPv4/IPv6 全局、境外分流和完整端口转发策略，默认仅内网、无端口转发。新用户始终是启用的普通账户，自动分配地址，不继承邀请人角色、固定地址、邀请资格或首次强制改密标志。

用户码兑换时读取邀请人的**当前**策略；管理员码读取该码当前配置的权限。保存授权/编辑时需确认对未使用码的影响，已注册账号不追溯变更。注册记录保留实际授予权限及版本，后台可展开查看完整策略。

邀请人账户被停用，或用户码对应邀请策略关闭后，未使用码暂不可用。恢复后未过期、未撤销码可以继续兑换。失效的转发入口不能静默忽略，需管理员修正策略后再注册。

## PostgreSQL 与事务

增量 migration 3 创建 `invitation_policies`、`invitations`、`invitation_registrations`，不重置现有账号、网络或节点配置。邀请码由两个独立 UUIDv4 组成，提供 244 位随机熵；仅存 SHA-256 摘要和 8 字符显示前缀。

完整码及分享链接仅在创建成功时返回。关闭结果后不能找回，只能撤销并重新生成。分享链接通过 `/register#invite=...` 携带码；片段不会发给 HTTP 服务，网页读取后立即从地址栏移除。不得把链接、密码、认证头或完整码放进日志、截图、浏览器存储或开发文档。

注册复用现有用户注册表写锁，并在一个 PostgreSQL 事务内：

1. 锁定并重新读取 `pmman_documents` 的 `users` 文档。
2. 检查用户名，锁定邀请策略与邀请码，检查邀请人、有效期和剩余使用次数。
3. 校验当前受邀权限和引用的转发入口。
4. 更新用户文档，写入端口转发策略、使用次数、实际授权快照及审计记录。
5. 提交后才替换内存用户注册表。

任何一步失败都回滚；用户名冲突不消耗码。数据库不可用返回 503，不创建临时账号、不降级到默认数据。单次码并发通过行锁和使用次数约束保护。成功但响应丢失时用户应先尝试登录，客户端及网页均不自动重试注册。

## API

客户端邀请功能不要求先修改初始密码：账号启用、会话有效且具备邀请授权即可按配额生成邀请码；查看本人策略、邀请码及记录同样不受初始密码标记限制。管理员接口仍遵循现有管理员认证规则。本规则不修改账号的密码、初始密码标记或邀请授权。

公开接口 `POST /v1/auth/register` 接收 `code`、`username`、`password`，成功返回 201 和账号摘要，不建立会话。请求体上限 8 KiB；未知权限字段被拒绝。

客户端接口均通过 typed IPC 访问主控：

```text
GET       /v1/client/invitations/policy
GET/POST  /v1/client/invitations
POST      /v1/client/invitations/{id}/revoke
GET       /v1/client/invitation-registrations
```

管理员接口：

```text
GET/POST  /v1/admin/invitations
POST      /v1/admin/invitations/{id}
POST      /v1/admin/invitations/{id}/revoke
GET       /v1/admin/invitation-registrations
GET/POST  /v1/admin/users/{username}/invitation-policy
```

管理员删除账号使用 `DELETE /v1/admin/users/{username}`，需在用户列表输入完整用户名确认。
删除会使该账号会话及未使用邀请码失效；该账号发放的无限码即使已有注册也会被撤销。
已注册的受邀账号不受连带删除影响，邀请关系、实际授予权限快照和审计历史保留。
删除后重新注册同名账号会产生新的 `user_id`，不继承旧邀请码、邀请资格、端口转发授权或租约。
历史注册记录按不可变 `user_id` 区分；用户名仍可搜索，但不再作为历史记录的唯一键。

列表服务端分页：`page` 从 1 开始，`page_size` 默认 20、最多 100；支持 `inviter`、`username`（注册记录）、`kind`/`state`（邀请码）、`since`/`until`（RFC3339）。注册参数错误/不可用码、用户名冲突、服务不可用分别给出明确文案，不返回邀请码所属用户。敏感接口响应使用 `Cache-Control: no-store`。

## 反向代理与限流

注册在密码哈希前限流：来源 IP 每分钟 10 次、全局每分钟 120 次；固定 60 秒窗口，429 携带 `Retry-After: 60`。此限制不代替公网反向代理的连接与请求大小限制。

默认只信任实际 TCP 对端。仅当 `PMMAN_TRUSTED_PROXY_IPS` 显式列出对端地址时，才采纳代理覆盖写入的单个 `X-Real-IP`。配置为逗号分隔的精确 IPv4/IPv6 地址，不支持任意转发头、通配符或网段；不要把所有 Docker/公网来源加入白名单。部署者应确认容器看到的实际代理来源地址，并保证代理**覆盖而非追加**客户端该头。未配置时代理后的用户共享该来源 IP 的配额。

Compose 和部署生成器透传此可选配置，默认空值；常规升级保留已有精确代理列表，不覆盖为空。邀请码操作审计保存资源 ID、操作者和事件；请求日志只记录状态及耗时。

## 测试和发布

Linux 测试在本地 WSL，Windows MSVC 构建在本地执行。显式 PostgreSQL 测试仅接受数据库名含 `pmman_invitation_test` 的专属数据库，每次建立随机 schema 并清理自己的 schema，禁止指向生产库：

```sh
cargo test --locked --manifest-path protocol/Cargo.toml
cargo test --locked --manifest-path server/Cargo.toml
PMMAN_INVITATION_TEST_DATABASE_URL=postgresql://postgres@127.0.0.1:55483/pmman_invitation_test \
  cargo test --locked --manifest-path server/Cargo.toml invitations -- --include-ignored
python3 scripts/verify-compose.py --image pmman-network/master:<local-test-tag>
```

Compose 验收使用临时项目、随机密码和独立卷，覆盖空环境、嵌入网页、注册、授权、数据库故障、重启和替换。可选 `--browser-state artifacts/<test-dir>` 暂停供浏览器使用，完成后在该目录放置 `done`，仅用于本地隔离测试。

`scripts/ui/invitations-client-fixture.js` 只验证浏览器界面，未进入应用包，不能替代真实后端或 Windows 安装验收。生产发布顺序为主控先升级、客户端后发布；旧客户端登录和连接协议不变，不需要更新节点。现阶段不包含邮箱验证、找回密码、奖励或多级邀请。
