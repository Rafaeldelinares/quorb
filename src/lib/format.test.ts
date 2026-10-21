import { describe, expect, it } from 'vitest';
import { formatCompact, formatResetIn, levelFor, mergeReports, peakMeter, sessionsLost, shortLabel, trayTooltip } from './format';
import type { UsageReport } from './types';

const base: UsageReport = {
  id: 'claude',
  name: 'Claude',
  kind: 'subscription',
  status: 'ok',
  plan: 'Max',
  meters: [
    { id: '', label: 'Sesión · 5 h', percent: 37, used: null, limit: null, unit: null, resetsAt: null, windowSeconds: null },
    { id: '', label: 'Semana · todos', percent: 81, used: null, limit: null, unit: null, resetsAt: null, windowSeconds: null },
  ],
  spend: null,
  message: null,
  action: null,
  fetchedAt: '2026-10-06T10:00:00Z',
};

describe('format', () => {
  it('formats reset distance', () => {
    const now = new Date('2026-10-06T10:00:00Z');
    expect(formatResetIn('2026-10-06T10:45:00Z', now)).toBe('en 45 min');
    expect(formatResetIn('2026-10-06T12:13:00Z', now)).toBe('en 2 h 13 min');
    expect(formatResetIn('2026-10-09T14:00:00Z', now)).toBe('en 3 d 4 h');
    expect(formatResetIn('2026-10-06T09:00:00Z', now)).toBe('ahora');
    expect(formatResetIn(null, now)).toBeNull();
  });

  it('formats big numbers', () => {
    expect(formatCompact(3_000_000_000)).toBe('3 B');
    expect(formatCompact(400_000_000)).toBe('400 M');
  });

  it('classifies levels', () => {
    expect(levelFor(10)).toBe('low');
    expect(levelFor(75)).toBe('high');
    expect(levelFor(95)).toBe('max');
  });

  it('picks the peak meter', () => {
    expect(peakMeter(base)?.percent).toBe(81);
  });

  it('keeps stale data when a cycle fails', () => {
    const failed: UsageReport = { ...base, status: 'warning', meters: [], message: '429' };
    const [merged] = mergeReports([base], [failed]);
    expect(merged.stale).toBe(true);
    expect(merged.meters).toHaveLength(2);
    expect(merged.message).toBe('429');
  });

  it('keeps the last data when the session expires', () => {
    const lost: UsageReport = { ...base, status: 'needsLogin', meters: [], action: 'login', message: 'caducada' };
    const [merged] = mergeReports([base], [lost]);
    expect(merged).toMatchObject({ status: 'needsLogin', action: 'login', message: 'caducada', stale: true });
    expect(merged.meters).toHaveLength(2);
    expect(mergeReports([], [lost])[0].meters).toHaveLength(0);
  });

  it('detects sessions that were just lost', () => {
    const lost: UsageReport = { ...base, status: 'needsLogin', meters: [] };
    expect(sessionsLost([base], [lost]).map((r) => r.id)).toEqual(['claude']);
    expect(sessionsLost([lost], [lost])).toEqual([]);
    expect(sessionsLost([], [lost])).toEqual([]);
  });

  it('shortens ring labels', () => {
    expect(shortLabel('Sesión · 5 h')).toBe('Sesión 5 h');
    expect(shortLabel('Semana · todos')).toBe('Semana');
    expect(shortLabel('Algo nuevo')).toBe('Algo nuevo');
  });

  it('shortens ring labels by stable id, whatever the label text', () => {
    const cases: [string, string][] = [
      ['weekly', 'Semana'],
      ['weekly_opus', 'Opus'],
      ['weekly_sonnet', 'Sonnet'],
      ['weekly_apps', 'Apps'],
      ['budget_monthly', 'Presupuesto'],
      ['rate_requests', 'Peticiones'],
      ['rate_tokens', 'Tokens'],
      ['extra_usage', 'Extra'],
    ];
    for (const [id, short] of cases) expect(shortLabel('Translated label', id)).toBe(short);
    expect(shortLabel('Sesión · 5 h', 'session')).toBe('Sesión 5 h');
    expect(shortLabel('Sesión · 3 h', 'session')).toBe('Sesión 3 h');
    // The codex weekly meter is labelled just «Semana».
    expect(shortLabel('Semana', 'weekly')).toBe('Semana');
  });

  it('falls back to the label when the id is empty or unknown', () => {
    expect(shortLabel('Semana · Opus', '')).toBe('Opus');
    expect(shortLabel('Semana · Opus')).toBe('Opus');
    expect(shortLabel('Semana · Opus', 'something_new')).toBe('Opus');
    expect(shortLabel('Gemini 2.5 Pro', 'model:gemini-2.5-pro')).toBe('Gemini 2.5 Pro');
    expect(shortLabel('Algo nuevo', 'custom')).toBe('Algo nuevo');
  });

  it('builds tray tooltip', () => {
    expect(trayTooltip([base])).toBe('Claude: 81%');
  });
});
