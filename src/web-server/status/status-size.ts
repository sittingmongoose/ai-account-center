/** The size cap for one `status.json` document. Must equal the crate's STATUS_SIZE_CAP
 * (native/aac-status/src/status_doc.rs); the share reader refuses larger files unread. */
export const STATUS_SIZE_CAP = 64 * 1024;
