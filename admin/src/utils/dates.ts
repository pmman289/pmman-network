export function formatDate(value?: string | null, missing = '—') {
  if (!value) return missing;
  const date = new Date(value);
  if (!Number.isFinite(date.getTime()) || date.getTime() <= 0) return missing;
  return date.toLocaleString('zh-CN', { hour12: false });
}

export function formatLastActivity(value?: string | null) {
  return formatDate(value, '未记录');
}
