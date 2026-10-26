import type { ClientUpdate } from "../types/client";

// Backend diagnostics remain available verbatim in details. Never use them as a headline.
export function messageSummary(
  text: string,
  fallback = "操作失败，请重试。",
): string {
  if (!text.trim()) return fallback;
  if (/密码.*不一致|两次输入/.test(text)) return "两次输入的密码不一致。";
  if (/密码.*(错误|不正确)|用户名.*密码|凭据.*无效/.test(text))
    return "用户名或密码不正确。";
  if (/401|会话.*失效|会话.*到期/.test(text)) return "登录已失效，请重新登录。";
  if (/403|未授权|没有权限|权限不足/.test(text))
    return "没有使用此功能的权限。";
  if (/额度|达到.*上限|配额/.test(text)) return "可用额度不足。";
  if (/没有.*(健康|可用)节点|暂无可用节点/.test(text))
    return "暂无可用节点，请刷新列表。";
  if (/握手|WireGuard|隧道.*失败/.test(text))
    return "无法连接节点，请重试或更换节点。";
  if (/路由.*失败|Windows 错误/.test(text)) return "网络配置失败，请重试。";
  if (/503|502|504|HTTP 5\d\d/.test(text)) return "服务暂不可用，请稍后重试。";
  if (/后台服务|管道|pipe|os error/i.test(text))
    return "无法访问后台服务，请重新启动客户端。";
  if (/timeout|超时/i.test(text)) return "请求超时，请重试。";
  if (/端口转发.*失败|节点应用/.test(text)) return "端口转发失败，请稍后重试。";
  if (
    text.length <= 48 &&
    !/HTTP|endpoint=|json:|<redacted>|peer|\/\/|[{}]/i.test(text)
  )
    return text;
  return fallback;
}

export function p2pSummary(
  message: string,
  enabled: boolean,
  connected: boolean,
): string {
  if (!enabled) return "已关闭直连，使用节点中转";
  if (!connected) return "连接后可查看在线设备";
  if (/失败|不可用|HTTP|json:/i.test(message))
    return "设备列表暂不可用，正在重试";
  return "直连已开启";
}

export const forwardStateLabel = (state: string): string =>
  ({
    pending: "待处理",
    applying: "启用中",
    active: "已启用",
    error: "异常",
    releasing: "释放中",
    released: "已释放",
    expired: "已过期",
  })[state] ?? "未知状态";

export function updateStateLabel(update: ClientUpdate): string {
  return {
    idle: "尚未检查更新",
    checking: "检查更新中",
    current: "已是最新版本",
    available: "有可用更新",
    downloading: "下载中",
    ready: "可以安装更新",
    error: "检查或下载失败",
  }[update.state];
}
