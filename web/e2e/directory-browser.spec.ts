import { test, expect } from '@playwright/test';

for (const viewport of [{ width: 1280, height: 800 }, { width: 390, height: 700 }]) {
  test(`selects the current directory without scrolling past a long list at ${viewport.width}px`, async ({ page }) => {
    await page.setViewportSize(viewport);
    const instanceId = 'synthetic-instance';
    const currentPath = '/synthetic/中文 projects';
    let validations = 0;
    let starts = 0;
    await page.route('**/workbench/v1/**', async route => {
      const request = route.request();
      const pathname = new URL(request.url()).pathname;
      if (pathname === '/workbench/v1/application') {
        await route.fulfill({ json: { instanceId, runs: [], sources: [], launchError: null, launchAvailable: true, directoryBrowserAvailable: true, remoteOwner: true } });
      } else if (pathname === '/workbench/v1/application/directories') {
        await route.fulfill({ json: {
          instanceId, currentPath, parentPath: '/synthetic', homePath: '/synthetic',
          roots: [{ name: '文件系统', path: '/' }],
          entries: Array.from({ length: 100 }, (_, index) => ({ name: `文件夹 ${index}`, path: `${currentPath}/${index}`, isSymlink: false })),
          canSelect: true, nextCursor: null, truncated: false,
        } });
      } else if (pathname === '/workbench/v1/launch-targets') {
        validations++;
        await route.fulfill({ json: { targetId: 'synthetic-target', canonicalPath: currentPath, configRevision: 'synthetic-revision', modes: ['new'], nativeHome: '/synthetic/.codex', expiresInSeconds: 30 } });
      } else {
        if (request.method() === 'POST' && pathname.startsWith('/workbench/v1/runs')) starts++;
        await route.fulfill({ status: 404, json: { error: { code: 'unexpected_request' } } });
      }
    });
    // Load Vite's workbench entry with the same route used by the embedded app.
    await page.addInitScript(() => history.replaceState(null, '', '/launch'));
    await page.goto('/workbench.html');
    await page.getByRole('button', { name: '浏览电脑目录', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: '浏览电脑目录', exact: true });
    const select = dialog.getByRole('button', { name: '选择此目录', exact: true });
    const path = dialog.getByRole('textbox', { name: '浏览目录路径', exact: true });
    const folders = dialog.getByRole('list', { name: '子目录' }).getByRole('button');
    await expect(folders).toHaveCount(100);
    await expect(select).toBeEnabled();
    await expect(select).toBeInViewport({ ratio: 1 });
    const inputBox = (await path.boundingBox())!;
    const selectBox = (await select.boundingBox())!;
    const firstFolderBox = (await folders.first().boundingBox())!;
    expect(selectBox.y).toBeGreaterThanOrEqual(inputBox.y + inputBox.height);
    expect(selectBox.y + selectBox.height).toBeLessThanOrEqual(firstFolderBox.y);
    expect(await dialog.evaluate(element => element.scrollTop)).toBe(0);
    await expect(dialog.getByRole('button', { name: '取消', exact: true })).toBeFocused();
    await page.keyboard.press('Tab');
    await page.keyboard.press('Tab');
    await page.keyboard.press('Tab');
    await expect(select).toBeFocused();
    await page.keyboard.press('Enter');
    await expect(dialog).toHaveCount(0);
    await expect(page.getByRole('textbox', { name: '项目目录', exact: true })).toHaveValue(currentPath);
    await expect(page.getByText('已确认目录', { exact: true })).toBeVisible();
    expect(validations).toBe(1);
    expect(starts).toBe(0);
  });
}
