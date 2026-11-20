// The kit's pinned thread: T3's state database as the Status reader reads it, in the run's temporary home
// (<home>/.t3/userdata/statev2.sqlite, the native reader's default under HOME), plus the one report that names
// the pin. A pin with no report is "silent" and the page draws no row, so both are needed for the rail test's
// sheet step. Synthetic and made-up, never this machine's T3 data (AAC_T3_STATE_DB stays unset unless realPins).
import fs from 'node:fs';
import path from 'node:path';

/** The pin the kit seeds, and the fixture report whose agent.t3Thread names it. */
export const KIT_PIN = {
	id: 'kit-pinned-thread-0001',
	title: 'Kit pinned thread (synthetic)',
	writer: 'vm.pm-fo-lead',
};

/**
 * Write the pin into the run's temporary home and point one fixture report at it. Returns whether the report
 * was found (a home without the fixture reports still gets the database, but the pin then reads as silent).
 */
export async function seedKitPin({ home, reports }) {
	const dir = path.join(home, '.t3', 'userdata');
	fs.mkdirSync(dir, { recursive: true });
	const { DatabaseSync } = await import('node:sqlite');
	const db = new DatabaseSync(path.join(dir, 'statev2.sqlite'));
	try {
		// The columns and the payload keys crates/aac-status/src/pins.rs requires of a top-level pin.
		db.exec(`CREATE TABLE IF NOT EXISTS orchestration_v2_projection_threads (
			thread_id TEXT PRIMARY KEY, title TEXT, archived_at TEXT, deleted_at TEXT, payload_json TEXT NOT NULL)`);
		db.prepare(
			'INSERT OR REPLACE INTO orchestration_v2_projection_threads (thread_id, title, archived_at, deleted_at, payload_json) VALUES (?, ?, NULL, NULL, ?)'
		).run(
			KIT_PIN.id,
			KIT_PIN.title,
			JSON.stringify({
				pinnedAt: '2026-11-01T12:00:00.000Z',
				pinOrderKey: 'kit-0001',
				createdAt: '2026-11-01T11:00:00.000Z',
			})
		);
	} finally {
		db.close();
	}
	const report = path.join(reports, 'v1', 'writers', KIT_PIN.writer, 'thread.json');
	if (!fs.existsSync(report)) return false;
	const doc = JSON.parse(fs.readFileSync(report, 'utf8'));
	doc.agent = { ...doc.agent, t3Thread: KIT_PIN.id };
	doc.writtenAt = new Date().toISOString();
	fs.writeFileSync(report, `${JSON.stringify(doc, null, 2)}\n`);
	return true;
}
