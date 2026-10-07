import assert from 'node:assert/strict';
import test from 'node:test';
import { matchesTaskDueFilter } from './task-filter.ts';
import { formatLocalDate } from './time.ts';

const today = '2027-01-01';
const dates = [null, '2026-12-31', today, '2027-01-02'];

test('Today includes only tasks due on the current local day', () => {
  assert.deepEqual(dates.filter(due => matchesTaskDueFilter(due, 'today', today)), [today]);
});

test('Overdue excludes today, future dates, and undated tasks across a year boundary', () => {
  assert.deepEqual(dates.filter(due => matchesTaskDueFilter(due, 'overdue', today)), ['2026-12-31']);
});

test('Overdue never includes a completed task, while Today and all still do', () => {
  assert.equal(matchesTaskDueFilter('2026-12-31', 'overdue', today, false), true);
  assert.equal(matchesTaskDueFilter('2026-12-31', 'overdue', today, true), false);
  assert.equal(matchesTaskDueFilter(today, 'today', today, true), true);
  assert.equal(matchesTaskDueFilter('2026-12-31', 'all', today, true), true);
});

test('clearing the date filter restores dated and undated tasks', () => {
  assert.equal(matchesTaskDueFilter(null, 'all', today), true);
  assert.equal(matchesTaskDueFilter('2027-01-02', 'all', today), true);
});

test('task date keys use the local calendar rather than a locale-specific display string', () => {
  assert.equal(formatLocalDate(new Date(2026, 11, 31, 23, 59)), '2026-12-31');
  assert.equal(formatLocalDate(new Date(2027, 0, 1, 0, 0)), '2027-01-01');
});
