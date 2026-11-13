/**
 * The pinned composer's structured additions to each update (native/aac-status/src/pinned.rs), passed through
 * unchanged: every wait apart, the human update's clock apart from the heartbeat's, and whose board it is.
 */

/** One wait, in its report's words. */
export interface PinnedWaitView {
  /** owner, agent, review, lock, resource, quota (the report's own word), or unknown. */
  on: string;
  text: string;
  short?: string;
  /** The board item that waits. */
  task?: string;
  since?: string;
  until?: string;
  from: 'thread' | 'tasks' | 'status';
}

/** One clock: "Updated 35 hours ago" or "Active 2 min ago", its ISO time and its age. */
export interface PinnedClockView {
  text: string;
  at: string;
  ageSeconds: number;
}

/** The board behind a pin's tasks, percent and loop. */
export interface PinnedBoardView {
  writer: string;
  project: string;
  stream: string;
  projectLabel: string | null;
  streamLabel: string | null;
  /** Another writer's board for the same project and stream: the stream's tasks and rounds, not this thread's. */
  shared: boolean;
}
