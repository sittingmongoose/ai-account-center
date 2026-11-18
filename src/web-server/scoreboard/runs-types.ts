import type { ReportProblem } from '../status/agent-reports-reader';
import type { RunDoc } from '../status/run-schema';
import type { ShareFile } from './share-files';

export interface RunsState {
  /** Corrections already resolved over the complete history, before filtering. */
  correctionsResolved?: boolean;
  /** Valid files for reader receipts, including superseded runs and voids; never scored. */
  accepted?: ShareFile<RunDoc>[];
  /** missing: no `v1/writers` folder; error: the folder could not be listed. */
  state: 'ok' | 'missing' | 'error';
  /** True when the snapshot was taken while a scan was still running: the runs read so far, not the whole share. */
  partial?: boolean;
  scannedAt: number | null;
  /** The newest copy of each run, by run id. */
  runs: ShareFile<RunDoc>[];
  writers: number;
  /**
   * Run files and writer folders left out by any cap (the newest days, and the writers with the
   * newest run dates, are kept). `runFiles` counts files, not runs: a run copied into several date
   * folders counts once per file. Below the writer cap it is exact; past it, and under a folder
   * that could not be listed, it is a lower bound (the runs Source says so).
   */
  skipped: { runFiles: number; writerFolders: number };
  problems: ReportProblem[];
}
