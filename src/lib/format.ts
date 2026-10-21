import type { Meter, UsageReport } from './types';

/** «en 2 h 13 min», «en 3 d 4 h», «ahora» */
export function formatResetIn(iso: string | null, now: Date = new Date()): string | null {
  if (!iso) return null;
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return null;
  const mins = Math.round((t - now.getTime()) / 60000);
  if (mins <= 0) return 'ahora';
  if (mins < 60) return `en ${mins} min`;
  const h = Math.floor(mins / 60);
  const m = mins % 60;
  if (h < 24) return m ? `en ${h} h ${m} min` : `en ${h} h`;
  const d = Math.floor(h / 24);
  const rh = h % 24;
  return rh ? `en ${d} d ${rh} h` : `en ${d} d`;
}

export function formatMoney(amount: number, currency = 'USD'): string {
  try {
    return new Intl.NumberFormat('es-ES', { style: 'currency', currency, currencyDisplay: 'narrowSymbol', maximumFractionDigits: 2 }).format(amount);
  } catch {
    return `${amount.toFixed(2)} ${currency}`;
  }
}

/** 3_000_000_000 → «3 B», 400_000_000 → «400 M», 1500 → «1,5 K» */
export function formatCompact(n: number): string {
  const abs = Math.abs(n);
  const fmt = (v: number) => new Intl.NumberFormat('es-ES', { maximumFractionDigits: 1 }).format(v);
  if (abs >= 1e9) return `${fmt(n / 1e9)} B`;
  if (abs >= 1e6) return `${fmt(n / 1e6)} M`;
  if (abs >= 1e3) return `${fmt(n / 1e3)} K`;
  return fmt(n);
}

export function formatPercent(p: number): string {
  return `${Math.round(p)}%`;
}

const SHORT_BY_ID = new Map([
  ['weekly', 'Semana'],
  ['weekly_opus', 'Opus'],
  ['weekly_sonnet', 'Sonnet'],
  ['weekly_apps', 'Apps'],
  ['budget_monthly', 'Presupuesto'],
  ['rate_requests', 'Peticiones'],
  ['rate_tokens', 'Tokens'],
  ['extra_usage', 'Extra'],
]);

// Respaldo para medidores sin id (datos antiguos).
const SHORT_BY_LABEL = new Map([
  ['Semana · todos', 'Semana'],
  ['Semana · Opus', 'Opus'],
  ['Semana · Sonnet', 'Sonnet'],
  ['Semana · apps', 'Apps'],
  ['Presupuesto mensual', 'Presupuesto'],
  ['Peticiones (ventana)', 'Peticiones'],
  ['Tokens (ventana)', 'Tokens'],
  ['Uso extra (mes)', 'Extra'],
]);

/** Etiqueta corta para la leyenda de los anillos: por id estable, con la etiqueta como respaldo. */
export function shortLabel(label: string, id = ''): string {
  return SHORT_BY_ID.get(id) ?? SHORT_BY_LABEL.get(label) ?? label.replace(/^Sesión · /, 'Sesión ');
}

export type Level = 'low' | 'mid' | 'high' | 'max';
export function levelFor(p: number | null): Level {
  if (p == null) return 'low';
  if (p >= 90) return 'max';
  if (p >= 70) return 'high';
  if (p >= 40) return 'mid';
  return 'low';
}

/** Medidor con el porcentaje más alto (lo que antes te va a bloquear). */
export function peakMeter(r: UsageReport): Meter | null {
  let best: Meter | null = null;
  for (const m of r.meters) {
    if (m.percent == null) continue;
    if (!best || (best.percent ?? 0) < m.percent) best = m;
  }
  return best;
}

/**
 * Si un ciclo falla pero el anterior tenía datos, conservamos los datos
 * antiguos marcados como `stale` para que el widget no se quede vacío.
 */
export function mergeReports(prev: UsageReport[], next: UsageReport[]): UsageReport[] {
  const byId = new Map(prev.map((r) => [r.id, r]));
  return next.map((r) => {
    const old = byId.get(r.id);
    const failed = r.status === 'error' || r.status === 'warning';
    if (failed && r.meters.length === 0 && !r.spend && old && (old.meters.length > 0 || old.spend)) {
      return { ...old, status: 'warning', message: r.message, stale: true };
    }
    // Sesión caducada: se conservan los últimos datos (atenuados) con la acción
    // de renovar, en vez de dejar el círculo vacío hasta volver a entrar.
    if (r.status === 'needsLogin' && r.meters.length === 0 && old && old.meters.length > 0) {
      return { ...old, status: 'needsLogin', message: r.message, action: r.action, stale: true };
    }
    return r;
  });
}

/** Informes que acaban de perder la sesión (antes tenían datos o estaban bien). */
export function sessionsLost(prev: UsageReport[], next: UsageReport[]): UsageReport[] {
  const before = new Map(prev.map((r) => [r.id, r.status]));
  return next.filter((r) => {
    const was = before.get(r.id);
    return r.status === 'needsLogin' && was != null && was !== 'needsLogin' && was !== 'unconfigured';
  });
}

export function trayTooltip(reports: UsageReport[]): string {
  const parts = reports.map((r) => {
    const peak = peakMeter(r);
    if (peak?.percent != null) return `${r.name}: ${formatPercent(peak.percent)}`;
    if (r.spend) return `${r.name}: ${formatMoney(r.spend.amount, r.spend.currency)}`;
    return `${r.name}: —`;
  });
  // Windows limita el tooltip de la bandeja a ~127 caracteres
  return (parts.join('\n') || 'Quorb').slice(0, 127);
}

/** Resumen en texto plano para copiar al portapapeles. */
export function reportSummary(r: UsageReport, now: Date = new Date()): string {
  const lines = [r.plan ? `${r.name} (${r.plan})` : r.name];
  for (const m of r.meters) {
    const reset = formatResetIn(m.resetsAt, now);
    const value =
      m.percent != null
        ? `${Math.round(m.percent)} %`
        : m.used != null
          ? `${formatCompact(m.used)}${m.unit ? ` ${m.unit}` : ''}`
          : '—';
    lines.push(`· ${m.label}: ${value}${reset ? ` (reinicia ${reset})` : ''}`);
  }
  if (r.spend) lines.push(`· ${r.spend.periodLabel}: ${formatMoney(r.spend.amount, r.spend.currency)}`);
  if (r.message && r.status !== 'ok') lines.push(`· ${r.message}`);
  return lines.join('\n');
}
