# pmman_network BIRD 路由导出

`pmman_network.conf` 用于把 Pmman Network 的接口直连前缀和 Linux FIB 表中的动态路由交给现有 BIRD OSPF 实例发布。

主控节点配置页面的“路由广播”标签会根据节点实际上报的 WireGuard 接口名和返回路由表生成同一份模板，并提供过滤器片段、安装步骤和验证命令。主控只生成配置，不会通过 SSH 修改节点上的 BIRD。

部署到不同节点时只需要修改文件顶部的变量：

```bird
define PMMAN_NETWORK_KERNEL_TABLE = 52001;
define PMMAN_NETWORK_INTERFACE = "wgnetport";
define PMMAN_NETWORK_SCAN_TIME = 10;
```

`PMMAN_NETWORK_KERNEL_TABLE` 是 Linux FIB 表编号；`PMMAN_NETWORK_INTERFACE` 是客户端地址池所在接口；`PMMAN_NETWORK_SCAN_TIME` 是 BIRD 同步该表的扫描周期。BIRD 的专用表名和协议名统一保持 `pmman_network_*`，不建议按节点改名。

接口直连的地址池路由由 `protocol direct pmman_network_direct` 学习，直接进入现有 `master4/master6`，不需要 direct 专用 pipe。节点写入表 52001 的 return route 则由 `protocol kernel` 学习到专用 BIRD table，再通过 pipe 进入 `master4/master6`；这是因为 BIRD 只允许一个 kernel syncer 绑定同一个 master 表。return route 需要使用 `scope global`；Linux 默认生成的 `scope link` 路由会被 BIRD 忽略，无法进入 OSPF；节点程序已统一按 global scope 安装这类路由。

数据流向：

```text
Linux main / interface wgnetport -> pmman_network_direct -> master4/master6
Linux table 52001 -> pmman_network_kernel_v4/v6 -> private BIRD table -> pipe -> master4/master6
                                                                          -> internal_ospf/internal_ospf_v6
```

节点本机转发回包时还会为每个客户端地址安装优先级 `10000` 的目的地址规则，
例如 `to 162.141.136.139/32 lookup 52001`。这条规则由节点服务随租约创建和删除，
不是 BIRD 配置的一部分；它保证来自公网或其他节点的回包进入 `wgnetport`，而不是落到
`main` 表的默认出口。BIRD/OSPF 仍负责其他节点的跨节点回程传播。

部署到其他节点：

1. 将 `config/bird/pmman_network.conf` 安装为 `/etc/bird/pmman_network.conf`。
2. 在 `/etc/bird/bird.conf` 中增加：

   ```bird
   include "/etc/bird/pmman_network.conf";
   ```

3. 现有 IPv4/IPv6 OSPF export filter 增加 `proto = "pmman_network_direct"`、`proto = "pmman_network_kernel_v4/v6"` 的允许规则。
4. 现有两个 kernel export filter 增加同一组 `proto = "pmman_network_*"` 的拒绝规则，避免把 Pmman Network 路由写回 Linux 主表。不能只判断 `source = RTS_PIPE`，因为 BIRD pipe 会保留原始路由协议名和 source 类型。
5. 使用 `bird -p -c /etc/bird/bird.conf` 检查后，再执行 `birdc configure`。

如果节点返回路由表填写的是名称而不是数字表号，例如 `ospf_return`，需要先在节点执行：

```bash
ip route show table ospf_return
grep ospf_return /etc/iproute2/rt_tables
```

然后将主控生成模板中的 `PMMAN_NETWORK_KERNEL_TABLE` 替换为对应数字表号。未确认数字表号前不要直接应用模板。

不要使用 `export all`。它可能把默认路由、BGP 路由、其他 VPN 路由或 OSPF 学到的路由再次发布出去；应只放行 `pmman_network_direct` 和 `pmman_network_kernel_v4/v6`。

Linux 表 52001 是内核路由表编号，不是 BIRD table 名称。接口的 connected route 由 direct protocol 立即进入 master 表；表 52001 中新增的动态前缀仍进入专用 BIRD table，再通过 pipe 进入 master 表，并经现有 OSPF 邻接发布。
