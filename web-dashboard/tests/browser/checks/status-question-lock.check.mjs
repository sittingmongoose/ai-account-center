// The real report reader and Status composer feed an isolated browser fixture. Each rollback
// leaves the composed card identical while moving its reason into or out of Report problems.
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { cardDoc, writeCard } from '../../../../tests/unit/web-server/status/status-test-helpers.ts';

const root = fileURLToPath(new URL('../../../..', import.meta.url));
const require = createRequire(import.meta.url);
const card = 'article.q-card';
const notice = `${card} .q-problem`;
const drawer = '.c2-report-problems';
const rollbackSentence = 'Its seq is lower than one already read, so this copy is ignored.';

async function statusFixture(t, name) {
  const dist = path.resolve(t.args.repo ?? root, 'dist/web-server/status');
  const { AgentReportsReader } = require(path.join(dist, 'agent-reports-reader.js'));
  const { buildStatusView } = require(path.join(dist, 'status-view.js'));
  const { MACHINE_IDS } = require(path.resolve(dist, '../services/dashboard-hosts.js'));
  const share = path.join(t.state.home, name);
  const now = Date.now();
  const reader = new AgentReportsReader({ root: () => share, now: () => now });
  const version = cardDoc('vm.review-seq', 'Q1', {
    seq: 2,
    askedAt: new Date(now - 60000).toISOString(),
    writtenAt: new Date(now - 30000).toISOString(),
    sections: [{ type: 'text', title: 'Fixture details', body: 'Some details.', fold: true }],
  });
  const missing = { data: null, asOf: null, error: null };
  const machines = Object.fromEntries(MACHINE_IDS.map((id) => [id, missing]));
  const done = { state: 'done', at: new Date(now).toISOString(), detail: null, attempts: 1 };
  const recorded = {
    id: 'recorded-answer',
    sendId: null,
    card: { ...version, seq: 3 },
    answer: {
      cardId: version.id, cardSeq: 3, cardSha256: '3'.repeat(64),
      kind: 'answer', choice: ['A'], note: '', via: 'deck', answeredAt: done.at, device: 'Fixture',
    },
    share: done,
    doorbell: done,
  };
  async function scan(seq) {
    writeCard(share, { ...version, seq });
    await reader.scan();
  }
  function view({ answered = true, answerHistoryProblem = null } = {}) {
    return buildStatusView({
      now,
      timeZone: 'America/New_York',
      reports: reader.snapshot(),
      registry: missing,
      machines,
      browsers: missing,
      simlock: missing,
      usage: null,
      answers: answered ? [recorded] : [],
      answerHistoryProblem,
    });
  }
  await scan(2);
  return { scan, view };
}

async function openStatus(t, read) {
  await t.viewport(1440, 900);
  await t.app.signIn();
  await t.page.route((url) => new URL(url).pathname === '/api/status', (route) =>
    route.fulfill({ json: read() })
  );
  await t.goto('/app/status', { settleMs: 300 });
  await t.page.waitForSelector(card);
}

async function reloadStatus(t, applied, expected) {
  await t.page.evaluate(() => window.dispatchEvent(new Event('online')));
  await t.page.waitForFunction(applied, expected);
}

async function checkPlacement(t, insideDrawer) {
  const text = await t.page.locator(notice).innerText();
  await t.equal('the card contains the reason only while it is out of the drawer', text.includes(rollbackSentence), !insideDrawer);
  await t.check('the question remains locked', await t.page.locator(`${card} .opt-pick`).first().isDisabled());
  if (insideDrawer) {
    const disclosure = t.page.locator(drawer);
    if (!(await disclosure.evaluate((el) => el.open))) await disclosure.locator('summary').click();
    await t.count(`${drawer} li`, 1);
    await t.text(`${drawer} li`, /seq is lower than one already read/);
  }
}

async function checkRollback(t, startsInDrawer) {
  const fixture = await statusFixture(t, startsInDrawer ? 'rollback-restore' : 'rollback-apply');
  if (startsInDrawer) await fixture.scan(1);
  let current = fixture.view();
  const before = current.cards.open[0];
  await openStatus(t, () => current);
  await checkPlacement(t, startsInDrawer);
  await t.page.locator(`${card} details summary`).click();
  const technical = t.page.getByRole('button', { name: 'Technical detail for agents' });
  await technical.click();
  await t.page.evaluate(() => {
    window.__questionLockNodes = ['article.q-card', 'article.q-card .q-problem', 'article.q-card details']
      .map((selector) => document.querySelector(selector));
  });

  await fixture.scan(startsInDrawer ? 2 : 1);
  current = fixture.view();
  await t.equal('the production composer returns the identical locked card', current.cards.open[0], before);
  await t.equal('the reader toggled the structured file problem', current.reportProblems?.length ?? 0, startsInDrawer ? 0 : 1);
  await reloadStatus(t, (shown) => document.querySelector('.c2-report-problems')?.hidden === !shown, !startsInDrawer);
  await checkPlacement(t, !startsInDrawer);
  await t.check('the card, warning paragraph and disclosure are the same DOM nodes', await t.page.evaluate(() =>
    ['article.q-card', 'article.q-card .q-problem', 'article.q-card details']
      .every((selector, index) => document.querySelector(selector) === window.__questionLockNodes[index])
  ));
  await t.check('the open section stays open', await t.page.locator(`${card} details`).evaluate((el) => el.open));
  await t.equal('technical details stay open', await technical.getAttribute('aria-expanded'), 'true');
}

export const tests = {
  'status question lock moves reason back from the drawer when seq 2 returns': (t) => checkRollback(t, true),
  'status question lock moves reason into the drawer when seq 1 arrives': (t) => checkRollback(t, false),
  async 'status question lock explains recovery from an answer-history outage'(t) {
    const fixture = await statusFixture(t, 'history-outage');
    const answerHistoryProblem = "The answer history can't be read right now.";
    let current = fixture.view({ answered: false, answerHistoryProblem });
    await openStatus(t, () => current);
    const historyNote = t.page.locator('.sec-questions .plate-note.problem').filter({ hasText: answerHistoryProblem });
    await t.equal('Questions shows the answer-history error', await historyNote.innerText(), answerHistoryProblem);
    await t.text(notice, /It can be answered once the answer history can be read again\./);
    await t.check('the card stays locked during the outage', await t.page.locator(`${card} .opt-pick`).first().isDisabled());

    current = fixture.view({ answered: false });
    await reloadStatus(t, () => !!document.querySelector('article.q-card .opt-pick:not([disabled])'));
    await t.count(notice, 0);
    await historyNote.waitFor({ state: 'detached' });
    await t.equal('the answer-history error clears', await historyNote.count(), 0);
  },
};
