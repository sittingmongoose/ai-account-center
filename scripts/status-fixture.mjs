#!/usr/bin/env node
/**
 * Writes a realistic agent reports folder for the Status tab's page and phone tests.
 *
 *   node scripts/status-fixture.mjs <reports-dir> [--ccs-dir <dir>] [--with-problems] [--with-kinds]
 *     [--no-video]
 *
 * <reports-dir> gets v1/writers/** (2 projects, 4 streams with tasks.json and 1 board-only stream,
 * 8 threads in mixed states, 5 open cards including a batch of 3 with recommendations, and 2 cards
 * already answered), stream boards under boards/, and a pm-mail registry copy under
 * pm-mail-registry/ (2 threads that do not report yet).
 *
 * The 2 answers and 1 note (a note alone on an open card) live in the server's private store, so
 * they need the server's ccs folder:
 * `--ccs-dir <dir>` (else CCS_DIR, else CCS_HOME/.ccs). The script writes
 * <dir>/status/answer-key.pem (created when absent), appends the records to
 * <dir>/status/answers.jsonl and writes their signed copies to <reports-dir>/v1/answers/.
 * One card (F-Q3) has option images under the writer's images/ folder.
 *
 * Start the test server with:
 *   AAC_AGENT_REPORTS_DIR=<reports-dir> AAC_PM_MAIL_REGISTRY_DIR=<reports-dir>/pm-mail-registry \
 *   AAC_STATUS_DOORBELL=off CCS_DIR=<dir>
 * so it reads only the fixture and never sends pm-mail.
 *
 * F-Q3 also gets a short H.264 MP4 with a poster when ffmpeg is installed (`--no-video` skips it),
 * for the Range-request playback check on phones. `--with-kinds` adds an objection, an action, a
 * judgement and a sample card (9 open cards then).
 * `--with-problems` adds a malformed tasks.json and a card with NUL bytes (the unreadable state).
 * Times are relative to now, so run it just before the test. It refuses a folder that already
 * holds v1/writers.
 */
import {
  createHash,
  createPrivateKey,
  createPublicKey,
  generateKeyPairSync,
  randomUUID,
  sign,
} from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const MIN = 60_000;
const HOUR = 60 * MIN;

function canonicalJson(value) {
  if (value === null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  const keys = Object.keys(value)
    .filter((key) => value[key] !== undefined)
    .sort();
  return `{${keys.map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(',')}}`;
}

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const body = `${JSON.stringify(value, null, 2)}\n`;
  fs.writeFileSync(file, body);
  return Buffer.from(body);
}

function answerKey(storeDir) {
  fs.mkdirSync(storeDir, { recursive: true, mode: 0o700 });
  const file = path.join(storeDir, 'answer-key.pem');
  if (!fs.existsSync(file)) {
    const { privateKey } = generateKeyPairSync('ed25519');
    fs.writeFileSync(file, privateKey.export({ type: 'pkcs8', format: 'pem' }), { mode: 0o600 });
  }
  const privateKey = createPrivateKey(fs.readFileSync(file, 'utf8'));
  const publicKey = createPublicKey(privateKey);
  const keyId = createHash('sha256')
    .update(publicKey.export({ type: 'spki', format: 'der' }))
    .digest('hex')
    .slice(0, 16);
  return {
    privateKey,
    publicKeyPem: publicKey.export({ type: 'spki', format: 'pem' }).toString(),
    keyId,
  };
}

const option = (key, label, gets = null, costs = null, media = [], sections = []) => ({
  key,
  label,
  gets,
  costs,
  media,
  sections,
});

/**
 * A 3-second H.264 MP4 (and its poster) from ffmpeg, when ffmpeg is installed: the phone test
 * needs a real video to check Range playback. Without ffmpeg the card shows only images.
 */
function writeVideo(dir) {
  const video = path.join(dir, 'pip-resize.mp4');
  const poster = path.join(dir, 'pip-resize.png');
  const run = (args) =>
    spawnSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y', ...args], {
      stdio: 'ignore',
      timeout: 60_000,
    }).status === 0;
  const made =
    run([
      '-f',
      'lavfi',
      '-i',
      'testsrc=duration=3:size=320x240:rate=15',
      '-c:v',
      'libx264',
      '-pix_fmt',
      'yuv420p',
      '-movflags',
      '+faststart',
      video,
    ]) && run(['-i', video, '-frames:v', '1', poster]);
  return made;
}

/** A 2x2 PNG, so the page has a real option image to show. */
const TINY_PNG = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFklEQVR4nGP4z8DwHwyBNAMDA8MZBgBVlgf5j5xnFAAAAABJRU5ErkJggg==',
  'base64'
);

/** Writes the fixture; returns what it wrote (for tests). */
export function writeStatusFixture(
  root,
  {
    ccsDir = null,
    withProblems = false,
    withKinds = false,
    withVideo = false,
    now = Date.now(),
  } = {}
) {
  const v1 = path.join(root, 'v1');
  if (fs.existsSync(path.join(v1, 'writers')))
    throw new Error(`${root} already holds v1/writers; use an empty folder.`);
  const at = (ago) => new Date(now - ago).toISOString();
  const writers = path.join(v1, 'writers');
  const seqs = new Map();
  const head = (schema, writer, ago) => {
    const seq = (seqs.get(writer) ?? 10) + 1;
    seqs.set(writer, seq);
    return { schema, writer, seq, writtenAt: at(ago) };
  };
  const boards = path.join(root, 'boards');
  const board = (name, ago) => {
    const file = path.join(boards, `${name}.md`);
    fs.mkdirSync(boards, { recursive: true });
    fs.writeFileSync(file, `# ${name} board\n\nThe human record of this stream.\n`);
    const time = new Date(now - ago);
    fs.utimesSync(file, time, time);
    return file;
  };

  // Threads (8): mixed states, one stale, one blocked on the owner.
  const thread = (writer, ago, fields) => {
    const name = writer.slice(writer.indexOf('.') + 1);
    writeJson(path.join(writers, writer, 'thread.json'), {
      ...head('agent-reports/thread@1', writer, ago),
      // A thread that ended its turn cannot heartbeat: done, idle, waiting and paused may say null.
      ttlSeconds: fields.ttlSeconds === undefined ? 1800 : fields.ttlSeconds,
      agent: {
        name,
        harness: fields.harness ?? 'claude-code',
        model: fields.model ?? 'claude-opus-5-5',
        effort: fields.effort ?? 'high',
        account: fields.account ?? 'claude_main',
        machine: writer.slice(0, writer.indexOf('.')),
        t3Thread: null,
        parent: fields.parent ?? null,
      },
      project: fields.project ?? null,
      stream: fields.stream ?? null,
      state: fields.state,
      doing: fields.doing ?? null,
      blockedOn: fields.blockedOn ?? null,
      progress: fields.progress ?? null,
      links: fields.links ?? [],
    });
  };
  const closureBoard = board('C-closure', 3 * HOUR);
  thread('vm.pm-fo-lead', 4 * MIN, {
    project: 'puppet-master',
    stream: 'F',
    state: 'working',
    doing: 'Landing group 4 of the PM7 follow-ups',
    progress: { done: 7, total: 12, unit: 'items' },
  });
  thread('vm.tools-lead', 9 * MIN, {
    project: 'puppet-master',
    stream: 'TOOLS',
    state: 'waiting',
    doing: 'Blind pre-check of browser-check 2.1',
    effort: 'xhigh',
  });
  thread('vm.aac-status-lead', 2 * MIN, {
    project: 'ai-account-center',
    stream: 'STATUS',
    state: 'working',
    doing: 'Status tab page and phone layout',
    progress: { done: 3, total: 8, unit: 'items' },
  });
  thread('mac.aac-claude-relogin', 50 * MIN, {
    project: 'ai-account-center',
    stream: 'SIGNIN',
    state: 'done',
    ttlSeconds: null,
    doing: 'Sign-in renewal landed; runner reinstalled',
  });
  thread('vm.pm-closure-lead', 12 * MIN, {
    project: 'puppet-master',
    stream: 'C',
    state: 'blocked',
    doing: 'Re-audit batch B09',
    blockedOn: { kind: 'owner', what: 'Which reviewer pair to trust for B09', since: at(40 * MIN) },
    links: [{ label: 'Board', path: closureBoard }],
  });
  thread('windows.pm-win-build', 3 * HOUR, {
    project: 'puppet-master',
    stream: 'F',
    state: 'paused',
    doing: 'Windows release build (waiting for a quiet PC)',
    model: 'gpt-6.1-sol',
    harness: 'codex',
    account: 'codex_main',
    parent: 'pm-fo-lead',
  });
  thread('nas1.nas-backup', 20 * MIN, {
    state: 'idle',
    doing: 'Nightly share backup check',
    model: 'glm-5.3',
    harness: 'zcode',
    account: 'zcode',
  });
  thread('vm.fo-welcome', 6 * MIN, {
    project: 'puppet-master',
    stream: 'F',
    state: 'working',
    doing: 'Welcome PiP look glitch (FO-41)',
    parent: 'pm-fo-lead',
  });

  // Streams with tasks.json (4).
  const item = (id, title, state, ago, extra = {}) => ({
    id,
    title,
    state,
    owner: extra.owner ?? null,
    branch: extra.branch ?? null,
    head: extra.head ?? null,
    reviews: extra.reviews ?? [],
    landed: state === 'landed' ? { sha: extra.sha ?? 'c76d1cf756', at: at(ago) } : null,
    blockedOn: extra.blockedOn ?? null,
    why: extra.why ?? null,
    updatedAt: at(ago),
  });
  const tasks = (writer, ago, fields, items) =>
    writeJson(path.join(writers, writer, 'tasks.json'), {
      ...head('agent-reports/tasks@1', writer, ago),
      ...fields,
      items,
    });
  tasks(
    'vm.pm-fo-lead',
    5 * MIN,
    {
      project: 'puppet-master',
      projectLabel: 'Puppet Master',
      stream: 'F',
      streamLabel: 'PM7 follow-ups',
      owner: 'pm-fo-lead',
      goal: 'Fix every PM7 follow-up before the owner’s QA round',
      board: board('F-follow-ups', 30 * MIN),
    },
    [
      item('FO-38', 'Welcome screen focus ring', 'landed', 26 * HOUR, {
        reviews: [{ by: 'gpt-6.1-sol/max', verdict: 'pass', at: at(27 * HOUR) }],
      }),
      item('FO-39', 'Settings search highlights', 'landed', 20 * HOUR),
      item('FO-40', 'Welcome PiP look glitch', 'landed', 5 * HOUR, {
        owner: 'fo-welcome',
        branch: 't3/fix/welcome-pip-look-glitches',
        head: '369a1bcdfa',
      }),
      item('FO-41', 'PiP shadow on resize', 'working', 6 * MIN, { owner: 'fo-welcome' }),
      item('FO-33', 'Lines chart label placement', 'blocked', 2 * HOUR, {
        blockedOn: { kind: 'owner', what: 'Card vm.pm-fo-lead--Q3', since: at(2 * HOUR) },
      }),
      item('FO-34', 'Empty state copy', 'review', 40 * MIN),
      item('FO-35', 'Toast stacking order', 'fixing', 25 * MIN),
      item('FO-36', 'Export button spacing', 'ready', 15 * MIN),
      item('FO-37', 'Keyboard shortcut sheet', 'todo', 3 * HOUR),
      item('FO-42', 'Old splash animation', 'dropped', 8 * HOUR, {
        why: 'Replaced by the new welcome',
      }),
      item('FO-43', 'Docs link in About', 'done', 9 * HOUR),
      item('FO-44', 'Window title casing', 'todo', 4 * HOUR),
    ]
  );
  tasks(
    'vm.tools-lead',
    10 * MIN,
    {
      project: 'puppet-master',
      projectLabel: 'Puppet Master',
      stream: 'TOOLS',
      streamLabel: 'Agent tools',
      owner: 'tools-lead',
      goal: 'Shared tools every agent can rely on',
      board: board('TOOLS', 2 * HOUR),
    },
    [
      item('T-16', 'browser-check 2.1 slots', 'review', 30 * MIN),
      item('T-17', 'simlock iOS warm pool', 'working', 50 * MIN),
      item('T-18', 'agent-report command', 'working', 20 * MIN),
      item('T-14', 'quota-watch per-stream attribution', 'landed', 2 * HOUR),
      item('T-15', 'pm-mail rings through T3', 'landed', 30 * HOUR),
      item('T-19', 'Recording codec fallback', 'blocked', 3 * HOUR, {
        blockedOn: { kind: 'resource', what: 'Windows GPU busy', since: at(3 * HOUR) },
      }),
    ]
  );
  tasks(
    'vm.aac-status-lead',
    3 * MIN,
    {
      project: 'ai-account-center',
      projectLabel: 'AI Account Center',
      stream: 'STATUS',
      streamLabel: 'Status tab',
      owner: 'aac-status-lead',
      goal: 'One page that answers "how are we looking?"',
      board: board('STATUS', 20 * MIN),
    },
    [
      item('ST-1', 'Contract', 'landed', 6 * HOUR),
      item('ST-2', 'Server view and answers', 'review', 10 * MIN),
      item('ST-3', 'Leptos page', 'working', 15 * MIN),
      item('ST-4', 'Shell tab and iframe', 'working', 25 * MIN),
      item('ST-5', 'Phone layout', 'todo', HOUR),
      item('ST-6', 'Scoreboard tab', 'todo', HOUR),
    ]
  );
  tasks(
    'mac.aac-claude-relogin',
    55 * MIN,
    {
      project: 'ai-account-center',
      projectLabel: 'AI Account Center',
      stream: 'SIGNIN',
      streamLabel: 'Sign-in renewal',
      owner: 'aac-claude-relogin',
      goal: 'Claude sign-ins renew by themselves',
      board: null,
    },
    [
      item('SR-1', 'Runner on the Mac', 'landed', 30 * HOUR),
      item('SR-2', 'CAPTCHA wait clean-up', 'landed', 2 * HOUR),
      item('SR-3', 'Windows runner', 'done', 3 * HOUR),
    ]
  );

  // Cards: 5 open (a batch of 3 with recommendations) and 2 to be answered.
  const cards = {};
  const card = (writer, local, ago, fields) => {
    const id = `${writer}--${local}`;
    const doc = {
      ...head('agent-reports/card@1', writer, ago),
      id,
      label: fields.label,
      project: fields.project ?? null,
      stream: fields.stream ?? null,
      askedBy: writer.slice(writer.indexOf('.') + 1),
      replyTo: { pmMail: writer.slice(writer.indexOf('.') + 1), t3Thread: null },
      state: 'open',
      title: fields.title,
      now: fields.now ?? null,
      nowAsOf: fields.nowAsOf ?? null,
      oldAnswer: fields.oldAnswer ?? null,
      whatChanged: fields.whatChanged ?? null,
      options: fields.options,
      recommendation: fields.recommendation ?? null,
      multiple: fields.multiple ?? false,
      urgency: fields.urgency ?? 'today',
      blocks: fields.blocks ?? null,
      sections: fields.sections ?? [],
      batch: fields.batch ?? null,
      evidence: fields.evidence ?? [],
      replies: fields.replies ?? [],
      kind: fields.kind ?? 'question',
      neededBy: fields.neededBy ?? null,
      exception: fields.exception ?? null,
      reversible: fields.reversible ?? null,
      verdict: fields.verdict ?? null,
      group: fields.group ?? null,
      agreement: fields.agreement ?? null,
      doubt: fields.doubt ?? null,
      appliesTo: fields.appliesTo ?? null,
      changesCanon: fields.changesCanon ?? [],
      related: fields.related ?? [],
      askedAt: at(ago),
    };
    const bytes = writeJson(path.join(writers, writer, 'cards', `${id}.json`), doc);
    cards[id] = { doc, sha256: createHash('sha256').update(bytes).digest('hex') };
    return id;
  };
  const images = path.join(writers, 'vm.pm-fo-lead', 'images');
  fs.mkdirSync(images, { recursive: true });
  fs.writeFileSync(path.join(images, 'fo33-inside.png'), TINY_PNG);
  fs.writeFileSync(path.join(images, 'fo33-under-axis.png'), TINY_PNG);
  const hasVideo = withVideo && writeVideo(images);
  const batch = 'F-questions-2026-11-05';
  card('vm.pm-fo-lead', 'Q3', 2 * HOUR, {
    label: 'F-Q3',
    project: 'puppet-master',
    stream: 'F',
    title: 'Where should the lines chart put its label?',
    now: 'On small windows the chart label covers the newest point, so the latest value is hidden.',
    nowAsOf: at(2 * HOUR),
    options: [
      option(
        'A',
        'Keep the label inside the plot',
        'The label moves left when it would cover a point.',
        'Can still feel crowded on a phone.',
        [
          {
            kind: 'image',
            path: 'images/fo33-inside.png',
            caption: 'The label inside the plot at 320 px',
            label: 'mockup',
            poster: null,
          },
        ],
        [
          {
            type: 'text',
            title: 'How it moves',
            fold: true,
            body: 'It slides left of the newest point.',
          },
        ]
      ),
      option(
        'B',
        'Move it under the axis',
        'The label never covers data.',
        'It sits further from the line it names.',
        [
          {
            kind: 'image',
            path: 'images/fo33-under-axis.png',
            caption: 'The label under the axis',
            label: 'mockup',
            poster: null,
          },
        ]
      ),
    ],
    recommendation: {
      key: 'A',
      why: 'Matches the other charts.',
      confidence: 'medium',
      flip: 'If the chart gets narrower than 280 px, B reads better.',
    },
    blocks: 'FO-33 landing (group 4)',
    batch,
    sections: [
      {
        type: 'facts',
        title: 'Where it shows',
        fold: false,
        rows: [
          { label: 'Windows narrower than', value: '420 px' },
          { label: 'Charts affected', value: '3 of 11' },
        ],
      },
      {
        type: 'callout',
        title: null,
        fold: false,
        tone: 'warn',
        body: 'The newest value is the one people read first.',
      },
      ...(hasVideo
        ? [
            {
              type: 'media',
              title: 'The glitch today',
              fold: false,
              items: [
                {
                  kind: 'video',
                  path: 'images/pip-resize.mp4',
                  caption: 'Resizing the window (test pattern)',
                  label: 'built',
                  poster: 'images/pip-resize.png',
                },
              ],
            },
          ]
        : []),
      { type: 'timeline', title: 'A section type this page does not know', fold: true },
    ],
  });
  card('vm.pm-fo-lead', 'Q4', 100 * MIN, {
    label: 'F-Q4',
    project: 'puppet-master',
    stream: 'F',
    title: 'Should the newest toast appear on top or at the bottom?',
    now: 'Toasts stack in arrival order, so a new one can appear under older ones.',
    options: [
      option('A', 'Newest on top', 'The latest message is where the eye lands first.', null),
      option(
        'B',
        'Newest at the bottom',
        'Reads like a chat log.',
        'New messages push older ones up.'
      ),
    ],
    recommendation: { key: 'A', why: 'The eye lands on the top first.' },
    batch,
    verdict: 'adapt',
    group: 'toasts',
    agreement: { same: false, note: 'Two reviewers split on this one.' },
    related: ['vm.pm-fo-lead--Q3'],
  });
  card('vm.pm-fo-lead', 'Q5', 90 * MIN, {
    label: 'F-Q5',
    project: 'puppet-master',
    stream: 'F',
    title: 'Drop the old splash animation for good?',
    now: 'The new welcome screen replaced it; the old one is still in the build behind a flag.',
    oldAnswer: { text: 'Keep it for one more release.', at: at(9 * 24 * HOUR) },
    whatChanged: 'That release shipped and nobody turned the flag on.',
    options: [
      option('A', 'Drop it', 'A smaller build and one less setting.', null),
      option('B', 'Keep it behind a setting', null, 'Another setting to test every release.'),
    ],
    recommendation: { key: 'A', why: 'Nobody has turned it on since PM6.' },
    urgency: 'this-week',
    batch,
    appliesTo: { count: 4, description: 'Every feature still behind a PM6 flag' },
    changesCanon: [{ ref: 'PM7 plan > Flags', what: 'Old flags are removed, not kept' }],
    doubt: 'One tester may still use the old splash on a slow machine.',
  });
  card('vm.tools-lead', 'Q1', 25 * MIN, {
    label: 'T-Q1',
    project: 'puppet-master',
    stream: 'TOOLS',
    title: 'Which machines may record during the owner’s streaming hours?',
    now: 'Recording on the VM drops frames while Moonlight streams.',
    options: [
      option('vm', 'Ubuntu VM', 'Smoothest video.', 'Drops frames while streaming.'),
      option('mac', 'Mac'),
      option('windows', 'Windows PC', 'Fastest encoder.', 'It is the owner’s own PC.'),
      option('nas1', 'Nas1', null, 'No GPU encoder.'),
    ],
    multiple: true,
    urgency: 'now',
    blocks: 'T-19',
    evidence: [{ label: 'Frame drop log', path: path.join(root, 'boards', 'TOOLS.md') }],
    sections: [
      {
        type: 'compare',
        title: 'What other setups do',
        fold: false,
        items: [
          {
            name: 'OBS on the gaming PC',
            does: 'Records with the GPU encoder while streaming.',
            problem: 'Competes with the game for the encoder.',
            link: { label: 'OBS', url: 'https://obsproject.com/' },
            version: '31',
          },
        ],
      },
      {
        type: 'notCopying',
        title: null,
        fold: true,
        items: [{ what: 'Recording at 60 fps', why: 'Doubles the encoder load.' }],
      },
      {
        type: 'terms',
        title: null,
        fold: true,
        items: [
          { term: 'Moonlight', meaning: 'The app that streams the VM desktop to the owner.' },
        ],
      },
      {
        type: 'unknowns',
        title: null,
        fold: false,
        items: ['Whether the Mac encoder holds up at 1080p.'],
      },
      {
        type: 'list',
        title: 'Steps if VM is chosen',
        fold: true,
        ordered: true,
        items: ['Pause Moonlight', 'Record', 'Resume'],
      },
      {
        type: 'table',
        title: 'Encoders',
        fold: true,
        columns: ['Machine', 'Encoder'],
        rows: [
          ['VM', 'NVENC'],
          ['Nas1', ''],
        ],
      },
      {
        type: 'links',
        title: null,
        fold: true,
        items: [{ label: 'Board', url: '/mnt/Cursor/share/puppet-master/TOOLS.md' }],
      },
    ],
  });
  card('vm.aac-status-lead', 'Q2', 40 * MIN, {
    label: 'ST-Q2',
    project: 'ai-account-center',
    stream: 'STATUS',
    title: 'Show stale threads collapsed or greyed?',
    now: 'Threads that stopped reporting still take a full row.',
    options: [
      option('A', 'Collapsed under "quiet"', 'A shorter list.', 'One tap to see them.'),
      option('B', 'Greyed in place', 'Nothing moves while you read.', 'A longer list.'),
    ],
    recommendation: { key: 'B', why: 'Nothing moves while you read.' },
    urgency: 'whenever',
    sections: [
      {
        type: 'quote',
        title: 'What the Status contract says',
        fold: false,
        items: [
          {
            text: 'Nothing is ever shown as live when its source is stale.',
            where: 'GET /api/status > Source',
            context: ['Agreed 2026-11-06'],
            source: 'docs/status-tab.md',
            link: null,
            version: 'v1',
          },
        ],
      },
      {
        type: 'planChange',
        title: null,
        fold: true,
        doc: 'docs/status-tab.md',
        unit: 'Thread',
        now: 'Stale threads take a full row.',
        proposed: 'Stale threads are greyed in place.',
      },
    ],
    replies: [
      { at: at(10 * MIN), text: 'Both are in the screenshot set now; greyed is on the left.' },
    ],
  });
  if (withKinds) {
    card('vm.pm-fo-lead', 'O1', 70 * MIN, {
      kind: 'objection',
      label: 'F-O1',
      project: 'puppet-master',
      stream: 'F',
      title: 'I am moving the export button into the toolbar',
      now: 'Two people missed it at the bottom of the panel during QA.',
      options: [],
      reversible: { yes: true, how: 'One commit to revert.' },
      neededBy: { at: at(-4 * HOUR), ifNoAnswer: 'It lands with group 4 tonight.' },
    });
    card('vm.aac-status-lead', 'D1', 15 * MIN, {
      kind: 'action',
      label: 'ST-D1',
      project: 'ai-account-center',
      stream: 'STATUS',
      title: 'Sign in to the second Claude account on the Mac',
      now: 'Its sign-in ended this morning; only you can complete the CAPTCHA.',
      options: [],
      urgency: 'now',
      exception: { rule: 'Agents never sign in for you', text: 'This one needs your hands.' },
    });
  }
  if (withKinds) {
    card('vm.pm-closure-lead', 'J1', 50 * MIN, {
      kind: 'judgement',
      label: 'C-J1',
      project: 'puppet-master',
      stream: 'C',
      title: 'Do these two summaries say the same thing?',
      now: 'A measurement card: your answer becomes part of the answer key.',
      options: [
        option('same', 'Same meaning'),
        option('different', 'Different meaning'),
        option('cant-tell', 'Can’t tell'),
      ],
      group: 'retention-key',
      batch: 'C-judgements',
    });
    card('vm.pm-closure-lead', 'S1', 45 * MIN, {
      kind: 'sample',
      label: 'C-S1',
      project: 'puppet-master',
      stream: 'C',
      title: 'Format sample: a card with a quote section',
      options: [option('A', 'First'), option('B', 'Second')],
    });
  }
  const answeredIds = [
    card('vm.pm-closure-lead', 'Q1', 5 * HOUR, {
      label: 'C-Q1',
      project: 'puppet-master',
      stream: 'C',
      title: 'Re-audit B09 with a second reviewer pair?',
      now: 'B09 passed one review but carries a shortcut flag.',
      options: [
        option(
          'A',
          'Yes, Sol and Astra',
          'A second opinion on the flagged batch.',
          'About an hour of quota.'
        ),
        option('B', 'No, one pass is enough', 'Saves the quota.', 'The flag stays unexplained.'),
      ],
      recommendation: { key: 'A', why: 'B09 had a shortcut flag.' },
    }),
    card('mac.aac-claude-relogin', 'Q7', 26 * HOUR, {
      label: 'SR-Q7',
      project: 'ai-account-center',
      stream: 'SIGNIN',
      title: 'Renew Windows sign-ins from the Mac runner?',
      options: [option('A', 'Yes'), option('B', 'No, give Windows its own runner')],
      recommendation: { key: 'B' },
    }),
  ];

  if (withProblems) {
    const broken = path.join(writers, 'vm.fo-welcome', 'tasks.json');
    fs.writeFileSync(broken, '{"schema": "agent-reports/tasks@1", "writer": ');
    const nul = path.join(writers, 'vm.fo-welcome', 'cards', 'vm.fo-welcome--Q1.json');
    fs.mkdirSync(path.dirname(nul), { recursive: true });
    fs.writeFileSync(nul, Buffer.concat([Buffer.from('{"schema":'), Buffer.alloc(32)]));
  }

  // pm-mail registry copy: 2 threads not reporting yet, 1 quiet for days (left out).
  const registry = path.join(root, 'pm-mail-registry');
  fs.mkdirSync(registry, { recursive: true });
  const reg = (name, ago, about, machine) =>
    writeJson(path.join(registry, `${name}.json`), {
      name,
      kind: 'claude',
      cwd: machine === 'mac' ? '/Users/agent/work' : '/home/agent/work',
      about,
      registered: at(ago + HOUR),
      last_seen: at(ago),
      machine: machine === 'mac' ? 'agent-mac.local' : 'ubuntuserver',
    });
  reg('pm-docs-sweep', 15 * MIN, 'Docs sweep for PM7 release notes', 'vm');
  reg('aac-mac-bar', 45 * MIN, 'Mac bar quota colours', 'mac');
  reg('old-scout', 4 * 24 * HOUR, 'Scouting run from last week', 'vm');

  // Answers: the private store and the signed share copies.
  let storeDir = null;
  if (ccsDir) {
    storeDir = path.join(ccsDir, 'status');
    const key = answerKey(storeDir);
    fs.mkdirSync(path.join(v1, 'keys'), { recursive: true });
    fs.writeFileSync(path.join(v1, 'keys', 'aac-answers.pub'), key.publicKeyPem);
    fs.writeFileSync(path.join(v1, 'keys', `${key.keyId}.pub`), key.publicKeyPem);
    const lines = [];
    const answers = [
      {
        id: answeredIds[1],
        choice: ['B'],
        note: 'Windows gets its own runner; keep the Mac one simple.',
        via: 'deck',
        ago: 20 * HOUR,
      },
      { id: answeredIds[0], choice: ['A'], note: '', via: 'recommended', ago: 3 * HOUR },
      // A note alone: the card stays open and shows it.
      {
        id: 'vm.aac-status-lead--Q2',
        choice: [],
        note: 'Could you show both side by side first?',
        via: 'deck',
        ago: 30 * MIN,
      },
    ];
    for (const entry of answers) {
      const { doc, sha256 } = cards[entry.id];
      const unsigned = {
        schema: 'agent-reports/answer@1',
        cardId: entry.id,
        cardSeq: doc.seq,
        cardSha256: sha256,
        kind: entry.choice.length > 0 ? 'answer' : 'note',
        choice: entry.choice,
        note: entry.note,
        via: entry.via,
        answeredAt: at(entry.ago),
        answeredBy: 'owner',
        device: 'iPhone',
      };
      const signature = {
        alg: 'ed25519',
        keyId: key.keyId,
        value: sign(null, Buffer.from(canonicalJson(unsigned), 'utf8'), key.privateKey).toString(
          'base64'
        ),
      };
      const answer = { ...unsigned, signature };
      const id = randomUUID();
      const file =
        answer.kind === 'note'
          ? `${entry.id}.note-${id.replace(/-/g, '').slice(0, 12)}.json`
          : `${entry.id}.json`;
      writeJson(path.join(v1, 'answers', file), answer);
      const done = (step, detail) => ({
        type: 'delivery',
        id,
        step,
        state: 'done',
        at: at(entry.ago - MIN),
        detail,
      });
      lines.push(
        { type: 'answer', id, at: unsigned.answeredAt, answer, card: doc },
        done('share', 'Signed copy written to the share.'),
        done('doorbell', `Rang ${doc.replyTo.pmMail} through pm-mail.`)
      );
    }
    fs.appendFileSync(
      path.join(storeDir, 'answers.jsonl'),
      lines.map((l) => `${JSON.stringify(l)}\n`).join(''),
      {
        mode: 0o600,
      }
    );
  }
  return {
    root,
    storeDir,
    cards: Object.keys(cards),
    answered: ccsDir ? answeredIds : [],
    noted: ccsDir ? ['vm.aac-status-lead--Q2'] : [],
  };
}

function main(argv) {
  const args = argv.slice(2);
  const take = (flag) => {
    const at = args.indexOf(flag);
    if (at < 0) return null;
    const [, value] = args.splice(at, 2);
    return value ?? null;
  };
  const withProblems = args.includes('--with-problems');
  if (withProblems) args.splice(args.indexOf('--with-problems'), 1);
  const withKinds = args.includes('--with-kinds');
  const withVideo = !args.includes('--no-video');
  if (!withVideo) args.splice(args.indexOf('--no-video'), 1);
  if (withKinds) args.splice(args.indexOf('--with-kinds'), 1);
  const ccsDir =
    take('--ccs-dir') ??
    process.env.CCS_DIR ??
    (process.env.CCS_HOME ? path.join(process.env.CCS_HOME, '.ccs') : null);
  const root = args[0];
  if (!root || args.length !== 1) {
    process.stderr.write(
      'Usage: node scripts/status-fixture.mjs <reports-dir> [--ccs-dir <dir>] [--with-problems] [--with-kinds] [--no-video]\n'
    );
    return 2;
  }
  const result = writeStatusFixture(path.resolve(root), {
    ccsDir: ccsDir ? path.resolve(ccsDir) : null,
    withProblems,
    withKinds,
    withVideo,
  });
  process.stdout.write(
    [
      `Wrote ${result.cards.length} cards to ${result.root}.`,
      result.storeDir
        ? `Answered ${result.answered.length} of them in ${result.storeDir}.`
        : 'No ccs folder given: the 2 answered cards show as open (pass --ccs-dir).',
      'Server environment:',
      `  AAC_AGENT_REPORTS_DIR=${result.root}`,
      `  AAC_PM_MAIL_REGISTRY_DIR=${path.join(result.root, 'pm-mail-registry')}`,
      '  AAC_STATUS_DOORBELL=off',
      result.storeDir ? `  CCS_DIR=${path.dirname(result.storeDir)}` : '',
      '',
    ].join('\n')
  );
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href)
  process.exitCode = main(process.argv);
