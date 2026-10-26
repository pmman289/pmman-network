// Run using playwright-cli run-code --filename after opening a dedicated session.
async (page) => {
  await page.addInitScript({ path: 'scripts/ui/cleanup-retry-fixture.js' });
  await page.goto('http://127.0.0.1:18338/', { waitUntil: 'domcontentloaded' });
  await page.getByRole('heading', { name: '未连接', exact: true }).waitFor();
  const connect = () => page.getByRole('button', { name: '连接内网', exact: true }).last();
  const pending = () => page.getByText('正在切换地址，等待旧连接清理…', { exact: true }).first();
  await connect().click();
  await pending().waitFor({ timeout: 10000 });
  if (await page.getByRole('heading', { name: '连接失败', exact: true }).count()) throw Error('cleanup shown as failure');
  if (await page.getByRole('button', { name: '切换浅色主题' }).isVisible()) await page.getByRole('button', { name: '切换浅色主题' }).click();
  await page.screenshot({ path: 'output/playwright/cleanup-wait-light.png' });
  await page.getByRole('button', { name: '切换深色主题' }).click();
  await page.screenshot({ path: 'output/playwright/cleanup-wait-dark.png' });
  await page.getByRole('button', { name: '取消连接', exact: true }).focus();
  await page.keyboard.press('Enter');
  await page.getByRole('heading', { name: '未连接', exact: true }).waitFor();
  if (await page.getByRole('heading', { name: '连接失败', exact: true }).count()) throw Error('cancel shown as failure');

  await page.evaluate(() => window.__PMMAN_UI_FIXTURE__.cleanup.setMode('success'));
  await connect().click();
  await pending().waitFor({ timeout: 10000 });
  await page.evaluate(() => window.__PMMAN_UI_FIXTURE__.cleanup.finish());
  await page.getByRole('heading', { name: '内部网络已就绪', exact: true }).waitFor();
  // A progress poll which started before completion must not restore pending state.
  await page.waitForTimeout(5500);
  if (!(await page.getByRole('heading', { name: '内部网络已就绪', exact: true }).isVisible())) throw Error('stale progress replaced connected');
  await page.screenshot({ path: 'output/playwright/cleanup-connected.png' });
  await page.getByRole('button', { name: '断开连接', exact: true }).last().click();
  await page.getByRole('heading', { name: '未连接', exact: true }).waitFor();

  await page.evaluate(() => window.__PMMAN_UI_FIXTURE__.cleanup.setMode('timeout'));
  await connect().click();
  await pending().waitFor({ timeout: 10000 });
  await page.evaluate(() => window.__PMMAN_UI_FIXTURE__.cleanup.finish());
  await page.getByRole('heading', { name: '连接失败', exact: true }).waitFor();
  await page.getByText('旧连接清理尚未完成，请稍后重试或联系管理员', { exact: true }).first().waitFor();
  if (!(await page.getByRole('button', { name: '重新连接', exact: true }).last().isEnabled())) throw Error('timeout left connect disabled');
  await page.screenshot({ path: 'output/playwright/cleanup-timeout.png' });
  return 'PASS: waiting, light/dark, keyboard cancellation, success, stable completion, timeout, retry button';
}
