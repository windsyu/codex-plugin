// Shared user navigation after retiring the standalone model-request page.
async function openCalls(page) {
  await page.getByRole('button', { name: '查看用量概览', exact: true }).click();
  await page.getByRole('button', { name: '查看调用记录', exact: true }).click();
}
async function closeCalls(page) {
  await page.getByRole('button', { name: '关闭调用面板', exact: true }).click();
}
module.exports = { openCalls, closeCalls };
