//! MiniMax — API oficial.
//!   · `GET {base}/v1/token_plan/remains` → cuota del plan (Subscription Key del plan M/Token)
//!   · `GET {base}/account/query_balance` → saldo PAYG (API key de pago por uso, best-effort)
//!
//! Son **dos credenciales distintas** en la consola de MiniMax: la Subscription Key
//! del plan y la API key de pago por uso. Cada medidor usa la suya.
//!
//! MiniMax **responde HTTP 200 incluso cuando la operación falla**: el estado real
//! viaja en el sobre `base_resp`. Ver [[envelope]].

use super::{net_error, Who};
use crate::config::MiniMaxConfig;
use crate::model::{Kind, Meter, Status, UsageReport};
use crate::secrets;
use serde_json::Value;

pub async fn fetch(cfg: &MiniMaxConfig, who: Who<'_>, http: &reqwest::Client) -> UsageReport {
    let report = who.report("MiniMax", Kind::Subscription);
    let Some(key) = who.secret(secrets::MINIMAX_SUBSCRIPTION_KEY) else {
        return report.fail(Status::Unconfigured, "Añade tu Subscription Key de MiniMax en Ajustes.");
    };
    let base = cfg.base_url.trim_end_matches('/');

    let res = match http.get(format!("{base}/v1/token_plan/remains")).bearer_auth(&key).send().await {
        Ok(r) => r,
        Err(e) => return report.fail(Status::Error, net_error(e)),
    };
    match res.status().as_u16() {
        200 => {}
        401 | 403 => return report.fail(Status::Unconfigured, "Subscription Key de MiniMax rechazada."),
        code => return report.fail(Status::Error, format!("HTTP {code} en /v1/token_plan/remains")),
    }
    let plan: Value = match res.json().await {
        Ok(v) => v,
        Err(e) => return report.fail(Status::Error, format!("Respuesta inesperada: {e}")),
    };
    if let Err(msg) = envelope(&plan) {
        return report.fail(Status::Error, msg);
    }

    // El saldo PAYG va con otra clave y es un endpoint que no está en la
    // documentación pública oficial: si falla, el informe del plan sigue válido.
    // Guardamos si había clave para poder distinguir «no configuré el saldo» de
    // «lo configuré y falló»: sin eso el usuario no tiene forma de saberlo.
    let payg_key = who.secret(secrets::MINIMAX_API_KEY);
    let balance = match payg_key.as_deref() {
        Some(api_key) => balance(http, &base, api_key).await,
        None => None,
    };

    let mut report = report;
    let parsed = parse(&plan, balance.as_ref(), payg_key.is_some());
    report.meters = parsed.meters;
    report.message = parsed.message;
    report
}

pub(crate) struct Parsed {
    pub meters: Vec<Meter>,
    pub message: Option<String>,
}

/// Lectura **best-effort** del saldo: cualquier fallo devuelve `None` y deja el
/// informe del plan intacto. No reintenta ni distingue causas, porque ninguna
/// cambia lo que el usuario ve.
async fn balance(http: &reqwest::Client, base: &str, api_key: &str) -> Option<Value> {
    let res = http.get(format!("{base}/account/query_balance")).bearer_auth(api_key).send().await.ok()?;
    if !res.status().is_success() {
        return None;
    }
    let value: Value = res.json().await.ok()?;
    envelope(&value).ok()?;
    Some(value)
}

/// MiniMax devuelve **HTTP 200 con el fallo dentro de `base_resp`**. Sin este
/// chequeo, una clave caducada o un plan inexistente producen un informe vacío
/// que parece correcto, que es peor que un fallo visible.
///
/// Estricto con el sobre ausente: un `token_plan/remains` que no lo trae no es un
/// payload que sabemos leer, así que se reporta como error en vez de devolver
/// cero medidores en silencio.
fn envelope(v: &Value) -> Result<(), String> {
    let Some(base) = v.get("base_resp") else {
        return Err("MiniMax devolvió una respuesta sin base_resp".into());
    };
    if base.is_null() {
        return Err("MiniMax devolvió una respuesta con base_resp vacío".into());
    }
    match num(base.get("status_code")) {
        Some(0.0) => Ok(()),
        Some(code) => Err(base
            .get("status_msg")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
            .map_or_else(|| format!("MiniMax rechazó la consulta (status_code {code})"), |m| format!("MiniMax: {m}"))),
        // Sin código no hay veredicto: no se inventa un fallo ni un éxito.
        None => Ok(()),
    }
}

/// Los importes de MiniMax llegan como cadenas decimales en varios endpoints y
/// como números en otros. Aceptamos ambos sin adivinar.
fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(|s| s.trim().parse().ok())))
}

fn timestamp(v: Option<&Value>) -> Option<i64> {
    num(v).map(|n| n as i64)
}

/// Las dos ventanas del Token Plan. Los nombres de campo son los del endpoint
/// `/v1/token_plan/remains`.
struct Window {
    id: &'static str,
    label: &'static str,
    used: &'static str,
    total: &'static str,
    start: &'static str,
    end: &'static str,
}

const WINDOWS: [Window; 2] = [
    Window {
        id: "token_plan_5h",
        label: "Plan · 5 horas",
        used: "current_interval_usage_count",
        total: "current_interval_total_count",
        start: "current_interval_start_time",
        end: "current_interval_end_time",
    },
    Window {
        id: "token_plan_weekly",
        label: "Plan · semanal",
        used: "current_weekly_usage_count",
        total: "current_weekly_total_count",
        start: "current_weekly_start_time",
        end: "current_weekly_end_time",
    },
];

pub(crate) fn parse(plan: &Value, balance: Option<&Value>, payg_configured: bool) -> Parsed {
    let mut meters = vec![];

    // El Token Plan tiene **dos ventanas**: una de 5 horas (`current_interval_*`)
    // y otra semanal (`current_weekly_*`). Se emiten las dos: con una sola, el
    // anillo miente cuando la de 5 h va sobrada y la semana va al límite.
    //
    // El porcentaje sale de `usage / total`, nunca de `remains`. La lectura de
    // estos campos depende del endpoint: en el viejo `/coding_plan/remains`,
    // `current_interval_usage_count` era lo que **quedaba** (MiniMax-AI/MiniMax-M2#99),
    // y al migrar a `/v1/token_plan/remains` pasó a ser el **consumo**
    // (MiniMax-AI/cli#104, «`current_interval_usage_count` is used count, not
    // remaining»). Aquí se usa la del endpoint actual; equivocarse invierte el anillo.
    for window in WINDOWS {
        let used = num(plan.get(window.used));
        let total = num(plan.get(window.total));
        let Some(limit) = total.filter(|t| *t > 0.0) else { continue };
        let spent = used.unwrap_or(0.0);
        let start = timestamp(plan.get(window.start));
        let end = timestamp(plan.get(window.end));
        meters.push(Meter {
            id: window.id.into(),
            label: window.label.into(),
            // Saturado aquí, y no solo en el anillo: un payload con
            // `usage > total` (ciclo mal cerrado) no debe colarse al popup ni
            // quedar persistido en el historial para siempre.
            percent: Some((spent / limit * 100.0).clamp(0.0, 100.0)),
            used: Some(spent),
            limit: Some(limit),
            unit: None,
            resets_at: end.and_then(|at| chrono::DateTime::from_timestamp(at, 0)).map(|at| at.to_rfc3339()),
            window_seconds: start.zip(end).map(|(s, e)| e - s),
        });
    }

    // El saldo PAYG no es un porcentaje: no hay «total» contra el que medirlo.
    // Va como medidor sin `percent`, que la UI ya sabe mostrar sin añadir un anillo.
    let available = balance.and_then(|b| num(b.get("available_amount")));
    if let Some(amount) = available {
        meters.push(Meter {
            id: "payg_balance".into(),
            label: "Saldo PAYG".into(),
            percent: None,
            used: Some(amount),
            limit: None,
            unit: Some("USD".into()),
            resets_at: None,
            window_seconds: None,
        });
    }

    // El mensaje resume la ventana **peor**: con dos ventanas, decir solo la de
    // 5 h hides que la semana está al límite.
    let worst = meters
        .iter()
        .filter(|m| m.percent.is_some())
        .max_by(|a, b| a.percent.partial_cmp(&b.percent).unwrap_or(std::cmp::Ordering::Equal));
    let plan = match worst {
        Some(m) => Some(format!("{} {:.0}/{:.0}", m.label, m.used.unwrap_or(0.0), m.limit.unwrap_or(0.0))),
        None => None,
    };
    // Con la API key puesta y el saldo sin llegar, el usuario no puede
    // distinguir «no lo configuré» de «lo configuré y falla». Decírselo es la
    // diferencia entre depurar una clave ycreer que MiniMax no publishes saldo.
    let saldo = match available {
        Some(balance) => Some(format!("saldo {balance:.2} $")),
        None if payg_configured => Some("saldo no disponible".to_string()),
        None => None,
    };
    let message = match (plan, saldo) {
        (Some(plan), Some(saldo)) => Some(format!("{plan} · {saldo}")),
        (Some(plan), None) => Some(plan),
        (None, Some(saldo)) => Some(saldo),
        (None, None) => None,
    };
    Parsed { meters, message }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Envuelve el payload **en el mismo nivel** que `base_resp`, como lo hace
    /// MiniMax: los campos del plan son hermanos del sobre, no hijos suyos.
    fn ok(mut body: Value) -> Value {
        body.as_object_mut()
            .expect("el fixture debe ser un objeto")
            .insert("base_resp".into(), json!({"status_code": 0, "status_msg": "success"}));
        body
    }

    #[test]
    fn derives_the_percentage_from_usage_over_total_never_from_remains() {
        let p = parse(&ok(json!({
            "current_interval_usage_count": 2500,
            "current_interval_total_count": 10000,
            "current_interval_remains_count": 7500,
        })), None, false);
        let meter = &p.meters[0];
        assert_eq!(meter.id, "token_plan_5h");
        assert_eq!(meter.percent, Some(25.0));
        assert_eq!(meter.used, Some(2500.0));
        assert_eq!(meter.limit, Some(10000.0));
        // El anillo debe ser el consumo, no lo que queda.
        assert_ne!(meter.percent, Some(75.0));
        assert_eq!(p.meters.len(), 1, "sin ventana semanal ni saldo solo se emite la de 5 h");
    }

    #[test]
    fn emits_both_token_plan_windows_independently() {
        let p = parse(&ok(json!({
            "current_interval_usage_count": 2500, "current_interval_total_count": 10000,
            "current_weekly_usage_count": 9000, "current_weekly_total_count": 10000,
        })), None, false);
        assert_eq!(p.meters.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["token_plan_5h", "token_plan_weekly"]);
        assert_eq!(p.meters[0].percent, Some(25.0));
        // Con la semana al 90 % y la ventana de 5 h al 25 %, el anillo tiene que
        // mirar a la peor: el anillo es único y solo puede mentir si elige mal.
        assert_eq!(p.meters[1].percent, Some(90.0));
    }

    #[test]
    fn distinguishes_a_missing_balance_from_a_configured_one_that_failed() {
        let plan = ok(json!({"current_interval_usage_count": 3, "current_interval_total_count": 6}));
        // Sin API key: el usuario no pidió saldo, no hay nada que señalar.
        assert_eq!(parse(&plan, None, false).message.as_deref(), Some("Plan · 5 horas 3/6"));
        // Con API key y saldo caído: tiene que verse, o el usuario no puede
        // distinguir «no lo configuré» de «lo configuré y falló».
        assert_eq!(parse(&plan, None, true).message.as_deref(), Some("Plan · 5 horas 3/6 · saldo no disponible"));
        // Sin ventanas de plan tampoco se pierde el aviso.
        assert_eq!(parse(&ok(json!({})), None, true).message.as_deref(), Some("saldo no disponible"));
    }

    #[test]
    fn saturates_a_percentage_that_exceeds_the_total() {
        // Ciclo mal cerrado o payload sucio: usage > total no puede pintar un
        // anillo por encima de 100 ni quedar persistido así en el historial.
        let p = parse(&ok(json!({
            "current_interval_usage_count": 1500, "current_interval_total_count": 1000,
        })), None, false);
        assert_eq!(p.meters[0].percent, Some(100.0));
        // El valor crudo se conserva: used/limit siguen diciendo la verdad.
        assert_eq!(p.meters[0].used, Some(1500.0));
        assert_eq!(p.meters[0].limit, Some(1000.0));
    }

    #[test]
    fn rejects_a_200_carrying_a_failed_base_resp() {
        let v = json!({"base_resp": {"status_code": 1004, "status_msg": "invalid api key"}});
        assert_eq!(envelope(&v).unwrap_err(), "MiniMax: invalid api key");
        let sin_mensaje = json!({"base_resp": {"status_code": 1008}});
        assert_eq!(envelope(&sin_mensaje).unwrap_err(), "MiniMax rechazó la consulta (status_code 1008)");
        assert!(envelope(&ok(json!({}))).is_ok());
        // Sin sobre no se afirma un éxito que no se pudo verificar.
        assert!(envelope(&json!({"current_interval_total_count": 10})).is_err());
        assert!(envelope(&json!({"base_resp": null})).is_err());
        // Un sobre sin código no da veredicto en ninguna dirección.
        assert!(envelope(&json!({"base_resp": {}})).is_ok());
    }

    #[test]
    fn payg_balance_is_a_value_meter_and_never_touches_the_ring() {
        let p = parse(
            &ok(json!({"current_interval_usage_count": 1, "current_interval_total_count": 4})),
            Some(&ok(json!({"available_amount": "12.50", "cash_balance": "10.00", "voucher_balance": "2.50"}))),
            true,
        );
        assert_eq!(p.meters.len(), 2);
        assert_eq!(p.meters[1].id, "payg_balance");
        assert_eq!(p.meters[1].percent, None, "un saldo no es un porcentaje consumido");
        assert_eq!(p.meters[1].used, Some(12.50));
        assert_eq!(p.meters[1].unit.as_deref(), Some("USD"));
        assert_eq!(p.message.as_deref(), Some("Plan · 5 horas 1/4 · saldo 12.50 $"));
    }

    #[test]
    fn a_missing_or_unusable_balance_leaves_the_plan_report_intact() {
        let plan = ok(json!({"current_interval_usage_count": 3, "current_interval_total_count": 6}));
        for balance in [None, Some(&json!({})), Some(&ok(json!({"available_amount": null})))] {
            let p = parse(&plan, balance, false);
            assert_eq!(p.meters.len(), 1, "el fallo del saldo no puede degradar el anillo del plan");
            assert_eq!(p.meters[0].percent, Some(50.0));
        }
        assert_eq!(parse(&plan, None, false).message.as_deref(), Some("Plan · 5 horas 3/6"));
    }

    #[test]
    fn reads_amounts_as_strings_or_numbers_and_never_divides_by_zero() {
        let as_string = parse(&ok(json!({"current_interval_usage_count": "40", "current_interval_total_count": "80"})), None, false);
        assert_eq!(as_string.meters[0].percent, Some(50.0));
        let sin_total = parse(&ok(json!({"current_interval_usage_count": 40, "current_interval_total_count": 0})), None, false);
        assert!(sin_total.meters.is_empty(), "un total de 0 no produce un anillo indefinido");
        let sin_datos = parse(&ok(json!({})), None, false);
        assert!(sin_datos.meters.is_empty() && sin_datos.message.is_none());
    }

    #[test]
    fn carries_the_interval_reset_and_window_when_minimax_sends_them() {
        let p = parse(&ok(json!({
            "current_interval_usage_count": 5,
            "current_interval_total_count": 10,
            "current_weekly_usage_count": 1,
            "current_weekly_total_count": 10,
            "current_interval_start_time": 1_735_689_600, // 2025-01-01T00:00:00Z
            "current_interval_end_time": 1_735_707_600,   // +5 h
            "current_weekly_start_time": 1_735_516_800, // 2024-12-30T00:00:00Z
            "current_weekly_end_time": 1_736_121_600,   // 2025-01-06T00:00:00Z
        })), None, false);
        assert_eq!(p.meters[0].window_seconds, Some(18_000));
        assert_eq!(p.meters[0].resets_at.as_deref(), Some("2025-01-01T05:00:00+00:00"));
        assert_eq!(p.meters[1].window_seconds, Some(604_800));
        assert_eq!(p.meters[1].resets_at.as_deref(), Some("2025-01-06T00:00:00+00:00"));
        // Sin marcas de tiempo el medidor sigue siendo válido, solo pierde el reinicio.
        let sin_ventana = parse(&ok(json!({"current_interval_usage_count": 5, "current_interval_total_count": 10})), None, false);
        assert!(sin_ventana.meters[0].resets_at.is_none() && sin_ventana.meters[0].window_seconds.is_none());
    }
}
