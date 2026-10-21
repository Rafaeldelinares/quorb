//! Historial local de lecturas para las mini gráficas y el ritmo de consumo.
//! Se guarda en `history.json` en la carpeta de datos de la app y se poda a 30 días.

use crate::model::UsageReport;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const KEEP_SECS: i64 = 30 * 86400;
/// Guardamos un punto por medidor como mucho cada 10 min, salvo que cambie ≥ 1 punto.
const MIN_GAP_SECS: i64 = 10 * 60;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Sample {
    /// unix (s)
    pub t: i64,
    /// id del proveedor
    pub p: String,
    /// id estable del medidor (etiqueta en muestras antiguas o medidores sin id; «spend» para el gasto)
    pub l: String,
    /// % del medidor o importe gastado
    pub v: f64,
}

pub struct History {
    path: PathBuf,
    samples: Mutex<Vec<Sample>>,
}

impl History {
    pub fn load(path: PathBuf) -> Self {
        let samples = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<Sample>>(&s).ok())
            .unwrap_or_default();
        Self { path, samples: Mutex::new(samples) }
    }

    /// Añade las lecturas correctas de un ciclo y guarda en disco si ha cambiado algo.
    pub fn record(&self, reports: &[UsageReport], now: i64) {
        let mut s = self.samples.lock().unwrap();
        let changed = append(&mut s, reports, now);
        if changed {
            let _ = save(&self.path, &s);
        }
    }

    pub fn query(&self, provider: &str, since: i64) -> Vec<Sample> {
        self.samples
            .lock()
            .unwrap()
            .iter()
            .filter(|x| x.p == provider && x.t >= since)
            .cloned()
            .collect()
    }
}

pub(crate) fn append(samples: &mut Vec<Sample>, reports: &[UsageReport], now: i64) -> bool {
    let mut changed = false;
    for r in reports.iter().filter(|r| matches!(r.status, crate::model::Status::Ok | crate::model::Status::Warning)) {
        let points = r
            .meters
            .iter()
            // El id estable identifica la serie; la etiqueta solo es el respaldo de medidores sin id.
            .filter_map(|m| m.percent.map(|p| (if m.id.is_empty() { m.label.clone() } else { m.id.clone() }, p)))
            .chain(r.spend.as_ref().map(|s| ("spend".to_string(), s.amount)));
        for (label, v) in points {
            let last = samples.iter().rev().find(|x| x.p == r.id && x.l == label);
            let keep = match last {
                None => true,
                Some(l) => now - l.t >= MIN_GAP_SECS || (v - l.v).abs() >= 1.0,
            };
            if keep {
                samples.push(Sample { t: now, p: r.id.clone(), l: label, v });
                changed = true;
            }
        }
    }
    let before = samples.len();
    samples.retain(|x| now - x.t <= KEEP_SECS);
    changed || samples.len() != before
}

fn save(path: &Path, samples: &[Sample]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    // Escritura atómica: primero a .tmp y luego renombrar.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string(samples).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, Meter, Spend, Status};

    fn report(pct: f64) -> UsageReport {
        let mut r = UsageReport::new("claude", "Claude", Kind::Subscription);
        r.meters.push(Meter { id: "session".into(), label: "Sesión · 5 h".into(), percent: Some(pct), ..Default::default() });
        r
    }

    #[test]
    fn throttles_and_prunes() {
        let mut s = vec![];
        assert!(append(&mut s, &[report(10.0)], 1_000));
        // mismo valor 1 min después: no se guarda
        assert!(!append(&mut s, &[report(10.2)], 1_060));
        // cambia 2 puntos: se guarda
        assert!(append(&mut s, &[report(12.0)], 1_120));
        // 10 min después: se guarda aunque no cambie
        assert!(append(&mut s, &[report(12.0)], 1_120 + 600));
        assert_eq!(s.len(), 3);
        // 31 días después se poda lo viejo
        append(&mut s, &[report(5.0)], 1_000 + 31 * 86400);
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn new_samples_store_the_meter_id_and_fall_back_to_the_label() {
        let mut r = UsageReport::new("claude", "Claude", Kind::Subscription);
        r.meters.push(Meter { id: "session".into(), label: "Sesión · 5 h".into(), percent: Some(10.0), ..Default::default() });
        r.meters.push(Meter { label: "Sin id".into(), percent: Some(20.0), ..Default::default() });
        let mut s = vec![];
        assert!(append(&mut s, &[r.clone()], 5));
        assert_eq!(s.iter().map(|x| x.l.as_str()).collect::<Vec<_>>(), ["session", "Sin id"]);
        // El throttle también va por id: una etiqueta traducida no duplica el punto.
        r.meters[0].label = "Session · 5 h".into();
        assert!(!append(&mut s, &[r], 65));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn records_spend_and_skips_errors() {
        let mut api = UsageReport::new("openai_api", "OpenAI API", Kind::Api);
        api.spend = Some(Spend { amount: 3.5, currency: "USD".into(), period_label: "Este mes".into(), budget: None });
        let bad = report(50.0).fail(Status::Error, "x");
        let mut s = vec![];
        append(&mut s, &[api, bad], 0);
        assert_eq!(s, vec![Sample { t: 0, p: "openai_api".into(), l: "spend".into(), v: 3.5 }]);
    }
}
