# pmmandns geosite 分流

生产环境使用主控持有的 V2Fly/Xray `geosite.dat`，不把完整规则文件下发给
Windows 客户端，也不要求节点开放 HTTP 服务。节点只通过主控的节点凭据接口
下载并缓存文件。

主控环境变量 `PMMAN_GEOSITE_DIR` 指向一个目录，目录内容如下：

```text
geosite/
├── geosite.dat
└── manifest.json
```

`manifest.json` 示例：

```json
{
  "version": "2026-10-07",
  "file_name": "geosite.dat",
  "sha256": "<64 位小写 sha256>",
  "size": 2295113,
  "domestic": "cn",
  "foreign": "geolocation-!cn"
}
```

主控每次节点心跳只下发版本、大小、校验和及分类名称。节点发现版本变化后，
使用 `X-PMMAN-NODE-TOKEN` 下载文件，校验成功后原子替换本地缓存，再重新加载
pmmandns。下载或解析失败时保留上一份有效规则，不会切换到不完整文件。

分流优先级为：管理员显式规则、geosite `full`/根域名/关键字/正则规则、默认
分组。生产默认分类为 `cn` 和 `geolocation-!cn`，国内、国外上游分别由节点
网络配置提供。旧的 `pmmandns_domestic_rules` 和 `pmmandns_foreign_rules` 仍
保留用于兼容旧配置，但不再是生产推荐方案。

DNS 回退只针对上游无响应、超时或传输失败。上游返回 `NOERROR + NODATA`
（例如国内域名被过滤的 AAAA 查询）或 `NXDOMAIN` 时，视为有效最终响应，
不得继续查询默认国外 DNS 或客户端本地 DNS，避免国内域名重新得到 IPv6 地址。
