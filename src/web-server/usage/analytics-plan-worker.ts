/** Find the local log roots away from the dashboard's HTTP event loop (analytics-request-plan.ts). */
import { parentPort, workerData } from 'worker_threads';
import type { AccountAnalyticsActivityRequest } from '../services/account-analytics-activity';
import {
  laterActivityRequests,
  readyActivityRequests,
  type ActivityPlanScope,
  type ReadyActivityPlan,
} from './analytics-request-plan';
import { lowerCollectorThreadPriority } from './collector-concurrency';

export type PlanWorkerRequest =
  | { part: 'ready'; activity: ActivityPlanScope }
  | { part: 'later'; activity: ActivityPlanScope; scanned: string[] };

export type PlanWorkerResponse =
  | { ok: true; ready?: ReadyActivityPlan; later?: AccountAnalyticsActivityRequest[] }
  | { ok: false };

async function plan(request: PlanWorkerRequest): Promise<PlanWorkerResponse> {
  if (request.part === 'ready') return { ok: true, ready: readyActivityRequests(request.activity) };
  return { ok: true, later: await laterActivityRequests(request.activity, request.scanned) };
}

if (parentPort) {
  const port = parentPort;
  lowerCollectorThreadPriority();
  void plan(workerData as PlanWorkerRequest)
    .catch((): PlanWorkerResponse => ({ ok: false }))
    .then((response) => port.postMessage(response))
    .finally(() => port.close());
}
