// Drives the sign-in page in Chromium with a virtual authenticator that
// supports PRF (run by tests/auth_page.rs).
// Usage: node auth_page.js BASE USER PASSWORD APP_URL
const { chromium } = require('playwright');
const [base, user, password, appUrl] = process.argv.slice(2);

(async () => {
  const browser = await chromium.launch();
  const context = await browser.newContext({ locale: 'en-US' });
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  // The refused password (step 4) logs its 401; anything else is an error.
  page.on('console', (m) => { if (m.type() === 'error' && !m.text().includes('status of 401')) errors.push(m.text()); });
  const cdp = await context.newCDPSession(page);
  await cdp.send('WebAuthn.enable');
  const { authenticatorId } = await cdp.send('WebAuthn.addVirtualAuthenticator', {
    options: { protocol: 'ctap2', transport: 'internal', hasResidentKey: true, hasUserVerification: true, isUserVerified: true, hasPrf: true, automaticPresenceSimulation: true },
  });
  const status = () => page.textContent('#status');
  const step = (name) => console.log('step: ' + name);

  // 1. Manage: password sign-in (as a password manager would fill it).
  await page.goto(base + '/auth/');
  await page.waitForSelector('#signin:not([hidden])');
  await page.fill('#user', user);
  await page.fill('#pw', password);
  await page.click('#pwform button[type=submit]');
  await page.waitForSelector('#manage:not([hidden])', { timeout: 30000 });
  step('password sign-in');
  // 2. Add a passkey.
  await page.fill('#keyname', 'Virtual key');
  await page.click('#addform button[type=submit]');
  await page.waitForFunction(() => document.querySelectorAll('#keys li').length === 1, null, { timeout: 30000 });
  step('passkey added: ' + await status());
  // 3. Turn the password off.
  page.once('dialog', (d) => d.accept());
  await page.click('#pwoff');
  await page.waitForSelector('#pwon:not([hidden])', { timeout: 15000 });
  step('password off: ' + await page.textContent('#pwstate'));
  await page.click('#done');
  await page.waitForSelector('#finished:not([hidden])');
  // 4. The password no longer works. (The virtual authenticator would
  // answer the passkey autofill request on its own: pause it.)
  await cdp.send('WebAuthn.setAutomaticPresenceSimulation', { authenticatorId, enabled: false });
  await page.goto(base + '/auth/');
  await page.waitForSelector('#signin:not([hidden])');
  await page.fill('#user', user);
  await page.fill('#pw', password);
  await page.click('#pwform button[type=submit]');
  await page.waitForFunction(() => document.querySelector('#status').className === 'error', null, { timeout: 30000 });
  step('password refused: ' + await status());
  // 5. App sign-in with the passkey: pairing code, then the redirect.
  await page.goto(appUrl);
  await page.waitForSelector('#pairing:not([hidden])');
  step('pairing code ' + await page.textContent('#code'));
  await cdp.send('WebAuthn.setAutomaticPresenceSimulation', { authenticatorId, enabled: true });
  const nav = page.waitForURL(/^http:\/\/127\.0\.0\.1:\d+\/callback/, { timeout: 30000 });
  await page.click('#passkey');
  await nav.catch(async (e) => { throw new Error(e.message + ' / status: ' + await status()); });
  step('redirected');
  const { credentials } = await cdp.send('WebAuthn.getCredentials', { authenticatorId });
  step('credentials on the authenticator: ' + credentials.length);
  if (errors.length) console.log('errors: ' + JSON.stringify(errors));
  await browser.close();
})().catch((e) => { console.error('FAILED: ' + e); process.exit(1); });
