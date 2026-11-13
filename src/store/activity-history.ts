/** Analytics usage history (docs/store.md section 12): the TypeScript side, which only forwards. */
export { ActivityIngestor, buildActivityReports, rowsReport } from './activity-reports';
export {
  HOURLY_RANGE_MS,
  localDayStarts,
  rangeToSources,
  sessionCounts,
  storeActivityAnswer,
} from './activity-answer';
export { coverageOf, gridFromStore, type StoreActivityCoverage } from './activity-coverage';
