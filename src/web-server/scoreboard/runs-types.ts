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
  scannedAt: number | null;
  /** The newest copy of each run, by run id. */
  runs: ShareFile<RunDoc>[];
  writers: number;
  /** Files and writers left out by any cap (the newest days/writers are kept). */
  skipped: { runFiles: number; writerFolders: number };
  problems: ReportProblem[];
}
