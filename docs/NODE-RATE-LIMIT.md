# 节点客户端限速

## 语义和范围

主控为每个节点配置每客户端的上传/下载 Mbps（十进制 megabit/s），0 为不限速，整数 1–100000 为有限额度。上传指客户端到节点，下载指节点到客户端。不是节点所有用户共享的总带宽。P2P 直连不受限；经节点的客户端互联 relay 仍受实际中转节点规则约束。

主控保存配置，节点重新注册不得覆盖；客户端从 HTTPS 主控节点列表、连接配置及在线目录获得规则。旧配置缺失字段按不限速处理。管理员提交需验证整数、范围和权限，并确认对在线用户影响。

## 数据路径

WireGuard 网关 peer 指向应用管理的本地 UDP 代理，只有网关密文经限速器送往节点。P2P 已有独立 loopback peer；直连候选探测/数据不进节点限速器，relay 封装入同一调度器。相同节点的网关/relay 合并计费，不同节点独立额度。上下行独立，计费为 UDP payload 加公网 IP/UDP 头，不含 HTTPS 控制面和节点列表 UDP RTT 探测。

令牌桶持续补充、100ms 有限突发；有界 FIFO 延迟队列，超时/队满丢包而非无限内存或阻塞数据线程。在线降额不会重置成满桶，重复目录不重充。断开销毁会话队列。IPv4/IPv6、网关握手与 DNS 都必须通过真实代理路径。

客户端控制接收/发送节奏可使 TCP 反馈跟随规则，但下载限速无法阻止已抵达网卡的 UDP 洪泛，恶意修改客户端也可绕过规则。生产环境如需要不可绕过的总带宽/防滥用必须增加服务端兜底，本功能不宣称具备该能力。

## 准出证据

- 主控权限、参数校验、旧存储兼容、保存失败不变更、重注册/重启保留。
- 管理页面标准组件、上下行保存/回显、0 提示、错误恢复；客户端每节点及当前连接显示。
- 确定时间的令牌桶、共享预算、上下行隔离、动态变更、有界队列及直连绕过测试。
- 本地 MSVC/WSL musl 构建，前后端 lint/type/build，安装包包含新 sidecar。
- txgz 主控部署，真实 Windows 节点网关与双栈业务限速通过：配置 2/3 Mbps 时实测上行 1.879 Mbps、下行 2.875 Mbps；在线降为 1/1 Mbps 后实测上行 0.936 Mbps、下行 0.952 Mbps。验收同时断言 WireGuard peer endpoint 为本地 `127.0.0.1:<port>`，确认没有绕过客户端限速代理；断开后服务、探针、节点临时 peer 和限速配置均完成清理/恢复。
- P2P 直连路径使用独立 UDP socket 和 direct 分支，不调用节点 RateLimiter；节点 relay 才进入对应节点预算。service P2P 回归覆盖直连/relay 切换，真实双机公网验收覆盖 directTxBytes 与 relayTxBytes；节点限速验收使用网关路径，不把 P2P 直连流量计入节点网关速率。
- 用户已明确跳过 Hyper-V P2P，不启动此类测试，不把 fixture/回环单元测试替代真实公网吞吐证据。

### 2026-10-06 实际准出记录

- WSL service 全量测试 16/16 通过；新增限速代理真实 loopback/underlay 双向转发测试；protocol 5/5 通过。
- Windows MSVC release sidecar 构建通过；txgz 公网限速验收使用真实 WireGuard 双栈路径、2 MiB 流量样本、在线心跳更新和配置恢复，结果 `txgz_rate_limit_verification=passed`。
- 修复的关键问题：WireGuard 握手等待逻辑不得把 peer endpoint 从本地代理改回公网 endpoint；NodeProxy 使用 loopback socket 接入 WireGuard、wildcard socket 发送/接收公网 UDP，避免以 `127.0.0.1` 作为公网源地址。
