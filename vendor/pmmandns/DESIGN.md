# pmmandns 开发设计文档

## 1. 背景与目标

`pmmandns` 是运行在 OpenWrt ARM 设备上的轻量 DNS 代理。网络出口和策略路由由 OpenWrt 负责，本程序只负责 DNS 请求接收、域名分类、上游选择、响应裁剪和缓存。

目标：

- 国内域名只查询国内 DNS。
- 国内域名存在 IPv4 时，只向客户端返回 IPv4。
- 国内域名没有 IPv4 但存在 IPv6 时，只向客户端返回 IPv6。
- 国外域名只查询海外 DNS，并返回完整的 A/AAAA 双栈结果。
- 单一静态 Go 二进制，适合 OpenWrt ARM/ARM64 长期运行。
- 不修改路由表、nftables、ipset，不负责决定后续连接出口。

非目标：

- 不实现 DNS over HTTPS/QUIC 服务端。
- 不做代理转发、透明代理或流量分流。
- 不根据 IP 地理位置判断国内/国外。
- 不向海外 DNS 回退国内域名，也不向国内 DNS 回退国外域名。
- `.dn42` 域名只查询 DN42 专用上游，不进入国内/国外规则分流。

## 2. 术语与规则

### 2.1 域名分类

域名分类由配置中的规则文件决定。生产环境推荐使用
`v2fly/domain-list-community` 的 `cn` 和 `geolocation-!cn` 导出列表；该项目被
V2Ray/Xray 等常见代理软件广泛使用，源数据和更新机制独立于本程序。

- `domestic`：国内域名。
- `foreign`：国外域名，可选；用于显式覆盖默认分类。
- `default`：未命中任何规则时的分类，默认是 `foreign`。

匹配规则使用域名后缀语义。例如规则 `example.com` 应匹配 `example.com` 和 `www.example.com`，但不匹配 `badexample.com`。

规则处理要求：

- 比较前转小写。
- 去除域名末尾的根点 `.`。
- 支持注释行和空行。
- 支持 IDNA/Punycode；内部使用 ASCII 形式匹配。
- 国内规则优先于国外规则，避免分类冲突。

主分类库推荐直接使用 V2Ray/Xray 生态的 `geosite.dat` 二进制文件；程序启动时只读取
其中配置的 `cn` 和 `geolocation-!cn` 两个 GeoSite，并构建内存反向标签树。TXT 文件
仅作为人工补充规则，适合放置 `domestic_extra.txt` 一类本地覆盖项。

规则文件同时兼容普通的一行一个域名格式，以及 geosite 文本导出的以下格式：

```text
domain:example.com
full:host.example.com:@cn
```

`domain:` 和普通域名会进入反向标签树并匹配子域名，`full:` 只匹配完整域名。
`keyword:`、`regexp:` 和 `include:` 不会在查询路径中解释；应在规则生成阶段展开，
或被安全跳过。这样查询只需从右向左遍历域名标签，不会扫描规则文件。

`geoip.dat` 的用途不同：它描述 IP/CIDR 的地区归属，不能在 DNS 上游选择之前替代
`geosite.dat`。如启用 GeoIP 校验，只能用于记录或检查解析结果，不应把“先问海外 DNS
再用 GeoIP 判断”作为国内域名分流策略。

### 2.2 国内域名地址族策略

国内域名只访问国内上游 DNS，并对 A/AAAA 结果进行联合决策：

| 国内上游结果 | 对外可见结果 |
|---|---|
| A 有记录 | 只返回 A，过滤全部 AAAA |
| A 无记录，AAAA 有记录 | 只返回 AAAA |
| A、AAAA 都无记录 | 保留上游的空结果语义 |
| 两种查询都失败 | 返回 `SERVFAIL` |

“A 无记录”指国内 DNS 成功返回 `NOERROR` 且没有 A Answer，不包括超时、网络错误或 `SERVFAIL`。实现必须区分 DNS RCODE 和传输错误。

当客户端只请求 A 或只请求 AAAA 时，代理仍应在内部同时获取 A 和 AAAA，避免无法判断“是否只有 IPv6”。两种查询可以并发发出。

对外响应：

- 有 A：A 请求返回 A；AAAA 请求返回 `NOERROR` 空 Answer。
- 只有 AAAA：A 请求返回 `NOERROR` 空 Answer；AAAA 请求返回 AAAA。
- 两者都没有：遵循国内上游的 `NXDOMAIN`/`NOERROR` 语义。

### 2.3 国外域名地址族策略

国外域名的 A、AAAA 及其他记录类型均查询海外上游。海外响应不做地址族裁剪，客户端请求什么类型就返回什么类型。

未知域名默认按 `foreign` 处理，以优先避免国内 DNS 污染。

## 3. 运行模型

进程监听 UDP 和 TCP DNS：

```text
IPv4: 0.0.0.0:53
IPv6: [::]:53
```

OpenWrt 的 DHCP/DNS 配置负责把路由器地址下发给客户端。若需要保留 dnsmasq 的本地域名，可让 pmmandns 将本地域名转发到本地 dnsmasq 监听地址，例如 `127.0.0.1:1053`；该功能应作为可选配置，不影响公网分流主流程。

每个请求的处理顺序：

1. 校验 DNS 报文，拒绝无 Question 或 Question 数量不支持的请求。
2. 规范化 QNAME 和 QTYPE。
3. 处理本地静态记录/可选 dnsmasq 转发。
4. 根据规则分类。
5. 查询缓存。
6. 缓存未命中时访问对应上游。
7. 对国内域名执行 A/AAAA 联合裁剪。
8. 写入缓存并恢复请求 Transaction ID。
9. 返回 UDP/TCP 响应。

默认只处理每个请求中的第一个 Question；对于多 Question 请求返回 `FORMERR`，避免缓存键和响应裁剪语义不明确。

## 4. 技术选型

- Go：建议使用当前稳定版本，构建时关闭 CGO。
- DNS 协议库：`github.com/miekg/dns`。
- 配置：JSON，避免引入 TOML 解析依赖。
- 日志：标准库 `log`，默认只输出启动、配置错误、上游故障和周期统计。
- 缓存：标准库 `map` + `sync.RWMutex`；第一版不引入外部缓存库。

推荐构建命令：

```sh
CGO_ENABLED=0 GOOS=linux GOARCH=arm GOARM=7 \
  go build -trimpath -ldflags='-s -w' -o pmmandns ./cmd/pmmandns
```

ARM64 使用 `GOARCH=arm64`。程序不得依赖 shell、Python、证书文件或运行时动态库才能提供普通 UDP/TCP DNS 转发。

## 5. 配置设计

示例 `/etc/pmmandns/config.json`：

```json
{
  "listen": ["0.0.0.0:53", "[::]:53"],
  "domestic_upstreams": ["223.5.5.5:53", "119.29.29.29:53"],
  "foreign_upstreams": ["1.1.1.1:53", "8.8.8.8:53"],
  "domestic_rules": ["/etc/pmmandns/domestic.txt"],
  "foreign_rules": ["/etc/pmmandns/foreign.txt"],
  "default_group": "foreign",
  "timeout_ms": 1200,
  "cache_size": 4096,
  "max_ttl": 86400,
  "negative_ttl": 30,
  "local_upstream": "127.0.0.1:1053"
}
```

字段要求：

- `listen` 非空；同一地址不能重复监听。
- 两个公网上游组至少各有一个地址。
- 上游地址必须包含端口，默认只允许 UDP/TCP DNS。
- `timeout_ms` 默认 1200，允许范围 200~5000。
- `cache_size` 是域名联合结果条目数，不是 RR 数量。
- `local_upstream` 可省略；配置后仅对 `local_suffixes` 中的域名后缀转发，例如 `lan`、`local`。
- `local_suffixes` 可省略；为空时不启用本地上游转发。
- `dn42_upstreams` 可选；配置后所有 `*.dn42` 和根域名 `dn42` 查询只发送到该上游。

配置加载失败时进程必须退出并返回非零状态；运行期间规则文件变化不自动生效，第一版通过重启加载。

## 6. 模块设计

建议目录：

```text
cmd/pmmandns/main.go
internal/config/config.go
internal/rules/matcher.go
internal/upstream/resolver.go
internal/cache/cache.go
internal/server/server.go
openwrt/etc/init.d/pmmandns
```

### 6.1 规则匹配器

接口：

```go
type Group int

const (
    GroupDomestic Group = iota
    GroupForeign
)

type Matcher interface {
    Match(name string) Group
}
```

规则使用按 DNS 标签反向组织的 trie 保存。查询 `www.example.com` 时依次访问
`com -> example -> www`，复杂度为域名标签数，且规则文件只在启动时读取。不得用
简单字符串 `HasSuffix`，以避免 `notexample.com` 错误命中 `example.com`。

### 6.2 上游解析器

接口：

```go
type Resolver interface {
    Exchange(ctx context.Context, req *dns.Msg) (*dns.Msg, error)
}
```

行为：

- 按配置顺序尝试上游。
- 每次请求使用独立超时。
- UDP 响应 `TC=1` 时自动使用 TCP 重试同一上游。
- 传输错误、格式错误和超时触发下一个上游。
- 收到合法 DNS 响应但 RCODE 为 `NXDOMAIN`、`NOERROR` 等，不应无条件切换上游。

国内联合查询使用两个独立请求：同一域名、同一 class，QTYPE 分别为 `A` 和 `AAAA`，可用 goroutine 并发执行，最终等待两者完成或达到联合超时。

### 6.3 缓存

国外缓存键：

```text
qname + qtype + qclass
```

国内缓存键应以域名和 class 保存联合结果：

```text
domestic + qname + qclass
```

缓存条目保存：

```go
type DomesticEntry struct {
    A          []dns.RR       // terminal A records used for preference
    AAAA       []dns.RR       // terminal AAAA records used for fallback
    AAnswer    []dns.RR       // CNAME chain plus A records
    AAAAAnswer []dns.RR       // CNAME chain plus AAAA records
    RCode      int
    ExpireAt   time.Time
}
```

缓存响应前必须深拷贝 DNS 消息或 RR，禁止多个请求共享可变对象。返回前始终替换 Transaction ID，并按请求 QTYPE 重新生成 Answer。

国内域名生成响应时必须保留上游选定地址族的 CNAME 链：有 A 时返回 `CNAME + A`，
仅有 AAAA 时返回 `CNAME + AAAA`。不能只提取终端地址记录，否则 Android `netd`/
`getaddrinfo` 可能将响应判定为 `EAI_NODATA`。

TTL 规则：

- 成功结果使用 A/AAAA 中的最小 TTL。
- 结果没有 Answer 时使用 `negative_ttl`。
- TTL 不超过 `max_ttl`。
- 过期条目惰性删除即可；缓存满时删除最旧或最少使用条目。

### 6.4 DNS 服务端

使用 `miekg/dns` 的 `dns.Server` 分别启动 UDP 和 TCP。处理器不得阻塞等待无界网络操作；所有上游请求必须受 context deadline 约束。

响应错误映射：

- 请求格式错误：`FORMERR`。
- 所有上游传输失败：`SERVFAIL`。
- 上游合法返回：尽可能保留其 RCODE、AA、AD 等语义；裁剪 Answer 时不得伪造权威标志。

## 7. 国内联合查询算法

伪代码：

```text
handleDomestic(name, requestedType):
    entry = cache.get(name, class)
    if entry missing:
        aResult, aErr = query(A)
        aaaaResult, aaaaErr = query(AAAA)
        if both transport failed:
            return SERVFAIL
        entry = merge(aResult, aaaaResult)
        cache.put(entry, minTTL)

    if entry.A not empty:
        return response(requestedType, A only, entry.RCode)
    if entry.AAAA not empty:
        return response(requestedType, AAAA only, entry.RCode)
    return response(requestedType, empty answer, entry.RCode)
```

实现时要区分“没有 A 记录”和“A 查询失败”。如果 A 查询失败但 AAAA 成功，应允许返回 AAAA，同时记录一次上游异常；如果 AAAA 查询失败但 A 成功，应返回 A。只有两种地址族查询都无法获得合法 DNS 结果时才返回 `SERVFAIL`。

## 8. 安全与资源限制

- 只允许配置的监听地址和上游地址，不实现开放递归缓存到任意目标。
- 限制单个请求最大 UDP 报文大小，建议使用 EDNS0，响应过大时遵循 DNS 截断规则。
- 限制缓存容量，避免客户端通过随机域名耗尽内存。
- 日志中不打印完整用户域名，或提供 debug 开关；默认避免泄漏家庭查询记录。
- 不在日志中打印上游响应正文。
- 使用 `context` 和服务器关闭超时，支持 SIGTERM 优雅退出。

## 9. OpenWrt 集成

安装文件：

```text
/usr/sbin/pmmandns
/etc/pmmandns/config.json
/etc/pmmandns/domestic.txt
/etc/pmmandns/foreign.txt
/etc/init.d/pmmandns
```

init 脚本要求：

- `START` 顺序晚于网络接口和时间同步服务。
- 支持 `start`、`stop`、`restart`、`reload`、`status`。
- 使用 procd 管理进程、自动重启和 SIGTERM。
- 配置或规则错误时不进入无限重启循环，应通过日志明确报错。

默认部署建议由 pmmandns 直接监听 53；若必须保留 dnsmasq 的 53 监听，则将 dnsmasq 改为本地端口，并让 pmmandns 监听 LAN 对外地址。

## 10. 测试要求

### 10.1 单元测试

- 域名大小写、尾点和 IDNA 规范化。
- 后缀边界匹配，验证 `example.com` 不匹配 `badexample.com`。
- 国内规则优先级。
- A 有、AAAA 有：只返回 A。
- A 无、AAAA 有：只返回 AAAA。
- A/AAAA 都无：保留 `NXDOMAIN`/`NOERROR`。
- A 超时、AAAA 成功：返回 AAAA。
- 两者均失败：返回 `SERVFAIL`。
- TTL 最小值、最大值和负缓存 TTL。
- Transaction ID 在缓存命中后正确替换。

### 10.2 集成测试

使用本地 mock DNS 上游验证：

- 国内/海外上游不会串组。
- UDP 截断时触发 TCP fallback。
- 多个上游按顺序故障转移。
- UDP 和 TCP 客户端都能查询。
- IPv4 和 IPv6 监听地址都能工作。
- 并发请求不会出现缓存数据竞争。

### 10.3 OpenWrt 验收

在目标设备验证：

```sh
nslookup -type=A www.example.cn <router-ip>
nslookup -type=AAAA www.example.cn <router-ip>
logread | grep pmmandns
/etc/init.d/pmmandns status
```

验收标准：

- 国内双栈域名对外仅有 A。
- 国内纯 IPv6 域名对外仅有 AAAA。
- 国外双栈域名同时返回 A 和 AAAA。
- 国内域名查询日志中只出现国内上游地址。
- 国外域名查询日志中只出现海外上游地址。
- 常驻内存和 CPU 占用在目标设备上可接受，且长时间运行无增长趋势。

## 11. 开发顺序

1. 初始化 Go module 和配置解析。
2. 实现域名规范化及规则匹配器。
3. 实现普通国外请求转发和 UDP/TCP server。
4. 实现国内 A/AAAA 联合查询和响应裁剪。
5. 实现 TTL 缓存、故障转移和 TCP fallback。
6. 补充单元/集成测试。
7. 编写 OpenWrt procd init 脚本和交叉编译 Makefile。
8. 在 ARM/ARM64 目标设备进行资源和长时间运行测试。

## 12. 明确的默认行为

最终默认策略如下：

```text
国内域名：只查询国内 DNS；有 A 则只返回 A；无 A 但有 AAAA 则只返回 AAAA。
国外域名：只查询海外 DNS；按客户端请求返回 A、AAAA 或其他记录。
未知域名：按国外域名处理。
DNS 代理：不参与任何 IPv4/IPv6 路由决策。
```
