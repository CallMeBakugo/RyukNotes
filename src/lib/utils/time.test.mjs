import assert from 'node:assert/strict';
import test from 'node:test';
import { formatClockTime } from './time.ts';

const at = (hour, minute) => new Date(2026, 1, 15, hour, minute);

test('12-hour insertion distinguishes midnight, noon, and evening', () => {
  assert.equal(formatClockTime(at(0, 7), '12h'), '12:07 AM');
  assert.equal(formatClockTime(at(12, 0), '12h'), '12:00 PM');
  assert.equal(formatClockTime(at(23, 5), '12h'), '11:05 PM');
});

test('24-hour insertion retains zero-padded clock values', () => {
  assert.equal(formatClockTime(at(0, 7), '24h'), '00:07');
  assert.equal(formatClockTime(at(23, 5), '24h'), '23:05');
});

test('relative display mode inserts an absolute time rather than an aging label', () => {
  assert.equal(formatClockTime(at(17, 28), 'relative'), '17:28');
});
