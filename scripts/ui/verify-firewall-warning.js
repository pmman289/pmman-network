// Browser-only IPC regression; does not change Windows firewall or create real leases.
async (page) => {
  await page.addInitScript({ path: 'scripts/ui/cleanup-retry-fixture.js' });
  await page.goto('http://127.0.0.1:18338/', { waitUntil: 'domcontentloaded' });
  await page.getByRole('heading', { name: '未连接', exact: true }).waitFor();
  await page.evaluate(() => {
    const controls = window.__PMMAN_UI_FIXTURE__;
    const original = window.__TAURI_INTERNALS__.invoke;
    const warning = 'Windows 防火墙规则同步失败：Windows 错误 5，策略拒绝（HTTP 503 诊断样例）；请手动检查目标端口入站规则。';
    controls.firewallFailing = true;
    controls.set({ portForwardQuota: { enabled: true, limit: 5, used: 0, available: 5 } });
    window.__TAURI_INTERNALS__.invoke = async (command, args) => {
      const response = await original(command, args);
      if (command === 'create_port_forward') {
        const forward = {
          forward_id: 'fixture-firewall-lease', user_id: 'fixture-user', username: 'UI 验收',
          device_id: 'fixture-device', entry_id: 'fixture-entry', node_id: 'fixture-node',
          protocols: args.protocols, public_address: '198.51.100.1', public_port: 20338,
          target_address: '192.0.2.1', target_port: args.targetPort, state: 'active',
          config_version: 1, created_at: '', updated_at: '',
        };
        Object.assign(response, { portForwards: [forward], portForwardMessage: '端口转发已建立',
          portForwardQuota: { enabled: true, limit: 5, used: 1, available: 4 } });
      }
      if (command === 'release_port_forward') {
        Object.assign(response, { portForwards: [], portForwardMessage: '端口转发已释放',
          portForwardQuota: { enabled: true, limit: 5, used: 0, available: 5 } });
      }
      if (['connect', 'create_port_forward', 'release_port_forward', 'list_port_forwards'].includes(command)) {
        response.portForwardFirewallWarning = controls.firewallFailing ? warning : '';
      }
      controls.set(response);
      return response;
    };
  });
  await page.getByRole('button', { name: '连接内网', exact: true }).last().click();
  await page.getByText('正在切换地址，等待旧连接清理…', { exact: true }).first().waitFor();
  await page.evaluate(() => window.__PMMAN_UI_FIXTURE__.cleanup.finish());
  const connected = page.getByRole('heading', { name: '内部网络已就绪', exact: true });
  const warning = page.locator('.inline-notice.warning');
  const assertNoFailure = async () => {
    if (await page.locator('.inline-notice.error').count()) throw Error('firewall warning became operation error');
    if (await page.getByRole('heading', { name: '连接失败', exact: true }).count()) throw Error('firewall warning became connection failure');
  };
  await connected.waitFor();
  await warning.waitFor();
  if (await warning.getAttribute('role') !== 'status') throw Error('warning incorrectly uses alert');
  if ((await warning.locator('p').innerText()).includes('连接失败')) throw Error('diagnostic incorrectly summarized as connection failure');
  await warning.locator('summary').click();
  if (!(await warning.locator('pre').innerText()).includes('Windows 错误 5')) throw Error('diagnostic lost');
  await assertNoFailure();
  if (await page.getByRole('button', { name: '切换浅色主题' }).isVisible()) await page.getByRole('button', { name: '切换浅色主题' }).click();
  await page.screenshot({ path: 'output/playwright/firewall-warning-light.png' });
  await page.getByRole('button', { name: '切换深色主题' }).click();
  await page.screenshot({ path: 'output/playwright/firewall-warning-dark.png' });
  await page.waitForTimeout(5500);
  if (!(await connected.isVisible()) || !(await warning.isVisible())) throw Error('poll lost connection or warning');
  await assertNoFailure();
  await page.getByRole('button', { name: '端口转发', exact: true }).click();
  await page.getByRole('button', { name: '创建转发', exact: true }).click();
  await page.getByRole('spinbutton', { name: '客户端目标端口' }).fill('3389');
  await page.locator('form.forward-form').getByRole('button', { name: '创建转发', exact: true }).click();
  await page.getByText('198.51.100.1:20338', { exact: true }).waitFor();
  if (!(await warning.isVisible())) throw Error('creation lost warning');
  await assertNoFailure();
  await page.screenshot({ path: 'output/playwright/firewall-warning-forward.png' });
  await page.getByRole('button', { name: '释放', exact: true }).click();
  await page.getByText('198.51.100.1:20338', { exact: true }).waitFor({ state: 'detached' });
  await assertNoFailure();
  if (!(await warning.isVisible())) throw Error('release lost firewall warning');
  await page.evaluate(() => { window.__PMMAN_UI_FIXTURE__.firewallFailing = false; });
  // Re-entering the page triggers a real UI list request against the IPC fixture.
  await page.getByRole('button', { name: '网络连接', exact: true }).click();
  await page.getByRole('button', { name: '端口转发', exact: true }).click();
  await warning.waitFor({ state: 'detached' });
  await assertNoFailure();
  return 'PASS: connected with independent warning, details, light/dark, polling, create/release success, recovery';
}
