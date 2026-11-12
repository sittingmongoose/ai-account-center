import computers from './computers.json' with { type: 'json' };
export const COMPUTERS = Object.freeze(Object.fromEntries(Object.entries(computers).map(([id, row]) =>
  [id, Object.freeze({ ...row, names: Object.freeze([...row.names]) })])));
export const HOST_IDS = Object.freeze(Object.keys(COMPUTERS));
export const HOST_LABELS = Object.freeze(Object.fromEntries(HOST_IDS.map(id => [id, COMPUTERS[id].label])));
export const HOST_PAIRS = Object.freeze(HOST_IDS.map(id => Object.freeze([id, COMPUTERS[id].label])));
export const hostIcon = id => COMPUTERS[id].os === 'mac' ? 'apple' : COMPUTERS[id].os === 'windows' ? 'windows' : 'ubuntu';
