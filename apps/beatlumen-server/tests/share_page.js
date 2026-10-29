// Opens public links in Chromium and checks the decrypted file
// (run by tests/share_page.rs).
// Usage: node share_page.js URL_PLAIN URL_PASSWORD PASSWORD SHA256_HEX
const { chromium } = require('playwright');
const [plainUrl, pwUrl, password, expected] = process.argv.slice(2);

(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage({ locale: 'en-US', acceptDownloads: true });
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  // The wrong password (403) is expected to log.
  page.on('console', (m) => { if (m.type() === 'error' && !m.text().includes('403')) errors.push(m.text()); });
  // Clicks "Download" and hashes the saved file.
  const digest = async () => {
    const [download] = await Promise.all([page.waitForEvent('download'), page.click('#save')]);
    const data = require('fs').readFileSync(await download.path());
    result.savedAs = download.suggestedFilename();
    return require('crypto').createHash('sha256').update(data).digest('hex');
  };
  const result = {};

  await page.goto(plainUrl);
  await page.waitForSelector('#ready:not([hidden])', { timeout: 30000 });
  result.plainName = await page.textContent('#name');
  result.plainOk = (await digest()) === expected;
  result.audio = await page.isVisible('#audio');

  await page.goto(pwUrl);
  await page.waitForSelector('#pwform:not([hidden])', { timeout: 30000 });
  result.noFileBeforePassword = await page.isHidden('#file');
  await page.fill('#pw', 'wrong one');
  await page.click('#pwform button[type=submit]');
  await page.waitForFunction(() => document.getElementById('status').className === 'error', null, { timeout: 60000 });
  result.wrongRefused = (await page.textContent('#status')).includes('Wrong password');
  await page.fill('#pw', password);
  await page.click('#pwform button[type=submit]');
  await page.waitForSelector('#ready:not([hidden])', { timeout: 60000 });
  result.pwOk = (await digest()) === expected;

  // A broken link (secret missing) explains itself.
  await page.goto(plainUrl.split('#')[0]);
  await page.waitForFunction(() => document.getElementById('status').textContent.length > 0, null, { timeout: 10000 });
  result.incomplete = (await page.textContent('#status')).includes('incomplete');
  result.errors = errors;
  console.log(JSON.stringify(result));
  await browser.close();
})().catch((e) => { console.log(JSON.stringify({ fatal: String(e) })); process.exit(1); });
