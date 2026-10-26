// Public registration never uses the administrator session or retries a POST.
export async function registerAccount(input: {
  code: string;
  username: string;
  password: string;
}): Promise<void> {
  let response: Response;
  try {
    response = await fetch('/v1/auth/register', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(input),
      signal: AbortSignal.timeout(20000),
      credentials: 'omit',
      referrerPolicy: 'no-referrer',
    });
  } catch {
    throw new Error('注册结果未确认，请先在客户端尝试登录；不要重复提交');
  }
  if (!response.ok) {
    const body = (await response.json().catch(() => null)) as { error?: string } | null;
    if (response.status >= 500) {
      throw new Error('注册服务暂不可用；如刚提交过注册，请先在客户端尝试登录');
    }
    throw new Error(body?.error ?? `注册失败（${response.status}）`);
  }
}
