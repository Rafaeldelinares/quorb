//! Contrato determinista del ciclo de colocación nativa (#179).
//!
//! El doble de `Surface` modela al gestor de ventanas: responde al pedido de
//! tamaño con retraso, puede recortarlo al mínimo anterior y sólo responde
//! cuando la ventana está mapeada (antes, su geometría no es prueba del
//! compositor). Las aserciones miran el resultado del ciclo, no sólo el orden
//! de las llamadas.

use super::widget_placement::*;
use crate::detail_geometry::Bounds;

/// Gestor de ventanas simulado: responde tarde, y sólo con la ventana mapeada.
struct Fake {
    /// (origen, tamaño) que concede el gestor una vez mapeada la ventana.
    granted: Geometry,
    /// (origen, tamaño) que la ventana conserva mientras el gestor calla.
    pending: Geometry,
    /// Lecturas que el gestor necesita antes de conceder la geometría.
    delay: usize,
    /// Lecturas que aún no ven la ventana (todavía no visible).
    hidden: usize,
    /// Lecturas hechas y estabilizaciones esperadas.
    reads: usize,
    settles: usize,
    /// Si la ventana ya está mapeada.
    mapped: bool,
    /// Si la lectura falla (no hay respuesta de la superficie nativa).
    fail_read: bool,
    /// Llamadas nativas, en orden.
    calls: Vec<String>,
}

impl Fake {
    /// El gestor concede la geometría pedida, tras una lectura.
    fn granting() -> Self {
        Fake {
            granted: Geometry {
                origin: (1859, 899),
                size: (61, 181),
            },
            pending: Geometry {
                origin: (800, 425),
                size: (320, 260),
            },
            delay: 1,
            hidden: 0,
            reads: 0,
            settles: 0,
            mapped: false,
            fail_read: false,
            calls: Vec::new(),
        }
    }

    /// El gestor nunca concede la geometría: conserva la anterior.
    fn refusing() -> Self {
        Fake {
            delay: usize::MAX,
            ..Fake::granting()
        }
    }
}

impl Surface for Fake {
    async fn request_size(&mut self, width: i32, height: i32) {
        self.calls.push(format!("size {width}x{height}"));
    }

    async fn request_position(&mut self, x: i32, y: i32) {
        self.calls.push(format!("position {x},{y}"));
    }

    async fn map(&mut self) {
        self.calls.push("map".into());
        self.mapped = true;
    }

    async fn read_geometry(&mut self) -> Result<Option<Geometry>, String> {
        self.reads += 1;
        if self.fail_read {
            return Err("the native surface did not answer".into());
        }
        // Todavía no visible: no es un fallo, es "todavía no".
        if self.reads <= self.hidden {
            return Ok(None);
        }
        // Sin mapear no hay prueba del compositor: la geometría no se lee.
        if !self.mapped {
            return Ok(None);
        }
        Ok(Some(if self.reads > self.delay {
            self.granted
        } else {
            self.pending
        }))
    }

    async fn settle(&mut self) {
        self.settles += 1;
    }
}

fn placement(anchor: Anchor) -> Placement {
    Placement {
        bounds: Bounds {
            x: 1859,
            y: 899,
            w: 61,
            h: 181,
        },
        anchor,
        scale: 1.0,
    }
}

/// Corre el ciclo con el doble: sin temporizadores, sin hilo de UI.
fn run(fake: &mut Fake, placement: &Placement, superseded: bool) -> Report {
    futures::executor::block_on(drive(fake, placement, || superseded))
}

/// Corre el ciclo con un cancelador que decide en cada fase.
fn run_with(fake: &mut Fake, placement: &Placement, superseded: impl FnMut() -> bool) -> Report {
    futures::executor::block_on(drive(fake, placement, superseded))
}

#[test]
fn the_cycle_reports_the_geometry_the_window_manager_granted() {
    let mut fake = Fake::granting();
    let report = run(&mut fake, &placement(Anchor::Right), false);
    assert_eq!(report.status, Status::Applied);
    assert_eq!(
        report.bounds,
        Some(Bounds {
            x: 1859,
            y: 899,
            w: 61,
            h: 181
        })
    );
    assert!(report.reason.is_none());
    // La ventana se mapea antes de leer: el read-back es prueba del compositor.
    assert!(fake.calls.iter().any(|c| c == "map"), "la ventana se mapea");
}

#[test]
fn a_window_that_is_not_visible_yet_is_retried_and_not_a_failure() {
    let mut fake = Fake::granting();
    // Las dos primeras lecturas no ven la ventana: no es un error, es "todavía
    // no" y el ciclo tiene que sobrevivir dentro de su presupuesto.
    fake.hidden = 2;
    let report = run(&mut fake, &placement(Anchor::Right), false);
    assert_eq!(report.status, Status::Applied);
    assert_eq!(
        report.bounds,
        Some(Bounds {
            x: 1859,
            y: 899,
            w: 61,
            h: 181
        })
    );

    // Si la ventana no llega a verse dentro del presupuesto, tampoco es un
    // fallo de lectura: se informa como tal y no se cachea geometría.
    let mut hidden = Fake::granting();
    hidden.hidden = usize::MAX;
    let report = run(&mut hidden, &placement(Anchor::Right), false);
    assert_eq!(report.status, Status::Unverified);
    assert_eq!(report.reason, Some(Reason::NotViewable));
    assert_eq!(report.bounds, None);
}

#[test]
fn a_size_the_window_manager_clamps_is_a_limitation_and_not_the_request() {
    let mut fake = Fake::refusing();
    let report = run(&mut fake, &placement(Anchor::Right), false);
    assert_eq!(report.status, Status::Limited);
    assert_eq!(report.reason, Some(Reason::SizeRefused));
    // La geometría que se informa es la que quedó en pantalla, no la pedida.
    assert_eq!(
        report.bounds,
        Some(Bounds {
            x: 800,
            y: 425,
            w: 320,
            h: 260
        })
    );
}

#[test]
fn the_cycle_stops_reading_after_its_budget() {
    let mut fake = Fake::refusing();
    let report = run(&mut fake, &placement(Anchor::Right), false);
    assert!(fake.reads <= 4 * READS, "reads: {}", fake.reads);
    assert!(fake.settles <= 4 * READS, "sin recuperación permanente");
    assert_eq!(report.status, Status::Limited);
}

#[test]
fn an_already_sized_surface_is_not_positioned_after_a_cancellation() {
    let mut fake = Fake::granting();
    // El tamaño ya es el pedido: el ciclo no entra en la espera.
    fake.granted.size = (61, 181);
    fake.pending.size = (61, 181);
    fake.delay = 0;
    // La cancelación llega después de la fase de tamaño: la posición tampoco
    // se pide (el cancelador se comprueba antes de cada fase nativa).
    let mut checks = 0;
    let report = run_with(&mut fake, &placement(Anchor::Right), || {
        checks += 1;
        checks > 1
    });
    assert_eq!(report.status, Status::Superseded);
    assert!(
        !fake.calls.iter().any(|c| c.starts_with("position")),
        "no se mueve la ventana después de cancelar: {:?}",
        fake.calls
    );
    // Una colocación cancelada no informa geometría: quedó a medias.
    assert_eq!(report.bounds, None);
}

#[test]
fn the_limited_path_does_not_move_the_window_after_a_cancellation() {
    let mut fake = Fake::refusing();
    // El cancelador pasa a true tras el primer read_until (después de mapear):
    // en el camino limitado tampoco se piden tamaño ni posición.
    let mut checks = 0;
    let report = run_with(&mut fake, &placement(Anchor::Right), || {
        checks += 1;
        checks > 3
    });
    assert_eq!(report.status, Status::Superseded);
    let after = fake
        .calls
        .iter()
        .rposition(|c| c == "map")
        .expect("la ventana se mapea");
    let native_after: Vec<&String> = fake.calls[after + 1..]
        .iter()
        .filter(|c| c.starts_with("size") || c.starts_with("position"))
        .collect();
    assert!(
        native_after.is_empty(),
        "sin fases nativas tras cancelar: {native_after:?}"
    );
}

#[test]
fn a_failed_read_is_not_reported_as_a_zero_sized_geometry() {
    let mut fake = Fake::granting();
    fake.fail_read = true;
    let report = run(&mut fake, &placement(Anchor::Right), false);
    assert_eq!(report.status, Status::Unverified);
    assert_eq!(report.reason, Some(Reason::ReadFailed));
    // Sin read-back no hay geometría que cachear como origen del arrastre.
    assert_eq!(report.bounds, None);
    assert!(!fake.calls.iter().any(|c| c.starts_with("size 0")));
}

#[test]
fn the_anchored_edge_survives_a_different_granted_size() {
    let mut fake = Fake::granting();
    // El gestor concede otra anchura: el borde derecho no debe despegarse.
    fake.granted = Geometry {
        origin: (1680, 899),
        size: (240, 181),
    };
    let report = run(&mut fake, &placement(Anchor::Right), false);
    assert_eq!(report.status, Status::Limited);
    // La posición se recalcula con el tamaño efectivo: 1859 + 61 - 240 = 1680.
    assert!(
        fake.calls.iter().any(|c| c == "position 1680,899"),
        "la posición usa el tamaño efectivo: {:?}",
        fake.calls
    );
}

#[test]
fn the_limited_path_restores_the_hints_to_the_real_size() {
    let mut fake = Fake::refusing();
    let report = run(&mut fake, &placement(Anchor::Right), false);
    assert_eq!(report.status, Status::Limited);
    // Las pistas vuelven al tamaño real: la ventana no puede saltar después.
    assert!(
        fake.calls.iter().any(|c| c == "size 320x260"),
        "las pistas se restauran: {:?}",
        fake.calls
    );
}

#[test]
fn the_transport_comes_from_the_real_gdk_display_type() {
    assert_eq!(transport_from_type_name("GdkX11Display"), Transport::X11);
    assert_eq!(
        transport_from_type_name("GdkWaylandDisplay"),
        Transport::Other
    );
    assert_eq!(
        transport_from_type_name("GdkBroadwayDisplay"),
        Transport::Other
    );
    assert_eq!(transport_from_type_name(""), Transport::Other);
}

#[test]
fn the_size_tolerance_absorbs_the_window_manager_rounding() {
    assert!(size_matches((61, 181), (61, 181)));
    assert!(size_matches((61, 181), (62, 181)));
    assert!(size_matches((61, 181), (60, 180)));
    assert!(!size_matches((61, 181), (63, 181)));
    assert!(!size_matches((61, 181), (61, 183)));
    assert!(!size_matches((61, 181), (240, 181)));
}

#[test]
fn only_the_widget_window_may_place_its_own_bounds() {
    let ok = Bounds {
        x: 10,
        y: 20,
        w: 61,
        h: 181,
    };
    assert!(ok.validate_widget("widget").is_ok());
    // Sólo la ventana del widget puede colocarse.
    for label in ["detail", "settings", "overlay", ""] {
        assert!(ok.validate_widget(label).is_err(), "label: {label}");
    }
    // Y el rectángulo tiene que ser utilizable.
    assert!(Bounds { w: 0, ..ok }.validate_widget("widget").is_err());
    assert!(Bounds { h: 0, ..ok }.validate_widget("widget").is_err());
    assert!(Bounds { w: 1_000_001, ..ok }
        .validate_widget("widget")
        .is_err());
    assert!(Bounds { h: 1_000_001, ..ok }
        .validate_widget("widget")
        .is_err());
    assert!(Bounds { x: i32::MAX, ..ok }
        .validate_widget("widget")
        .is_err());
    assert!(Bounds { y: i32::MAX, ..ok }
        .validate_widget("widget")
        .is_err());
    assert!(Bounds {
        x: -1920,
        y: -100,
        ..ok
    }
    .validate_widget("widget")
    .is_ok());
}

#[test]
fn the_scale_conversion_uses_the_factor_the_request_was_computed_with() {
    // A escala 1 el round-trip es exacto; a 1,5 y 2 el gestor redondea al
    // píxel de dispositivo, así que el error físico es de 1 px como mucho.
    for scale in [1.0, 1.5, 2.0] {
        for physical in [61, 91, 181, 362] {
            let logical = to_logical(physical, scale);
            let back = to_physical(logical, scale);
            assert!(
                (back - physical).abs() <= 1,
                "scale {scale}: {physical} -> {logical} -> {back}"
            );
        }
    }
    // A escala 1,5 el tamaño físico pedido puede no existir: 91 px es 60,67
    // lógicos y el gestor concede 92 px físicos.
    assert_eq!(to_logical(91, 1.5), 61);
    assert_eq!(to_physical(61, 1.5), 92);
    // En DPI mixto manda el factor de la petición, no el de la ventana.
    let placement = Placement {
        bounds: Bounds {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        },
        anchor: Anchor::Top,
        scale: 1.25,
    };
    assert_eq!(to_logical(placement.bounds.w as i32, placement.scale), 80);
    assert_eq!(to_physical(80, placement.scale), 100);
}

#[test]
fn the_placement_payload_deserializes_with_its_bounds_anchor_and_scale() {
    // El payload es el que manda el frontend: rectángulo, borde anclado y el
    // factor con el que se calculó.
    let payload = r#"{"x":1859,"y":899,"w":61,"h":181,"anchor":"right","scale":1.5}"#;
    let placement: Placement = serde_json::from_str(payload).expect("the payload must deserialize");
    assert_eq!(
        placement.bounds,
        Bounds {
            x: 1859,
            y: 899,
            w: 61,
            h: 181
        }
    );
    assert_eq!(placement.anchor, Anchor::Right);
    assert_eq!(placement.scale, 1.5);
    // Y sin factor no hay colocación: la conversión necesita el factor con el
    // que se calculó el rectángulo.
    let without_scale = r#"{"x":1859,"y":899,"w":61,"h":181,"anchor":"right"}"#;
    assert!(serde_json::from_str::<Placement>(without_scale).is_err());
}

#[test]
fn the_report_travels_with_the_effective_geometry_and_its_reason_code() {
    let applied = Report {
        status: Status::Applied,
        bounds: Some(Bounds {
            x: 1859,
            y: 899,
            w: 61,
            h: 181,
        }),
        reason: None,
    };
    let json = serde_json::to_value(applied).unwrap();
    assert_eq!(json["status"], "applied");
    assert_eq!(json["bounds"]["x"], 1859);
    assert_eq!(json["bounds"]["w"], 61);
    assert!(json.get("reason").is_none());

    let limited = Report {
        status: Status::Limited,
        bounds: Some(Bounds {
            x: 0,
            y: 0,
            w: 320,
            h: 260,
        }),
        reason: Some(Reason::SizeRefused),
    };
    let json = serde_json::to_value(limited).unwrap();
    assert_eq!(json["status"], "limited");
    // El motivo es un código, no prosa de interfaz.
    assert_eq!(json["reason"], "size_refused");

    let superseded = Report {
        status: Status::Superseded,
        bounds: None,
        reason: Some(Reason::Superseded),
    };
    let json = serde_json::to_value(superseded).unwrap();
    assert_eq!(json["status"], "superseded");
    assert_eq!(json["reason"], "superseded");
    assert!(json["bounds"].is_null());
}

/// Prueba en vivo de la secuencia nativa con un webview real (#179). No es
/// determinista: `GDK_BACKEND=x11 cargo test --locked widget_placement -- --ignored`.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "needs an X11 session"]
fn the_native_sequence_fixes_the_size_of_a_real_webview_window() {
    use gtk::prelude::*;
    use webkit2gtk::WebViewExt;
    gtk::init().expect("GTK must initialise");
    // El transporte real de esta sesión: sin display X11 no hay transporte X11.
    let display = gtk::gdk::Display::default().expect("a display");
    let name = display.type_().name().to_string();
    assert_eq!(
        crate::widget_placement::transport_from_type_name(&name),
        Transport::X11
    );

    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_decorated(false);
    window.set_resizable(false);
    window.set_default_size(320, 260);
    let view = webkit2gtk::WebView::new();
    view.load_html(
        "<html><body style='margin:0'><div style='width:61px;height:181px'></div></body></html>",
        None,
    );
    window.add(&view);
    settle(700);
    assert_ne!(
        window.size(),
        (61, 181),
        "la ventana arranca con su tamaño por defecto"
    );

    // La secuencia de producción: pistas iguales y resize programático.
    crate::widget_placement::fix_size(&window, 61, 181);
    // Antes de mapear, el tamaño pedido es el de GDK (nada de destello).
    assert_eq!(
        window.size(),
        (61, 181),
        "el tamaño se pide antes de mapear"
    );
    // El mapeo es condicional: si la ventana ya está visible, no se toca.
    crate::widget_placement::map_if_needed(&window);
    settle(700);
    assert_eq!(
        window.size(),
        (61, 181),
        "el tamaño pedido se aplica exacto"
    );

    // El resize manual es imposible: las pistas fijas recortan cualquier
    // reconfiguración, aunque el cliente la pida.
    window.resize(200, 200);
    settle(300);
    assert_eq!(
        window.size(),
        (61, 181),
        "la ventana no se puede redimensionar"
    );

    // Y la posición se aplica con el tamaño fijo puesto: las pistas iguales no
    // bloquean el movimiento.
    window.move_(1859, 899);
    settle(300);
    assert_eq!(
        window.position(),
        (1859, 899),
        "la posición se aplica con el tamaño fijo"
    );
}

/// Deja que GTK procese los eventos (la respuesta del gestor) durante `ms`.
#[cfg(target_os = "linux")]
fn settle(ms: u64) {
    let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < until {
        while gtk::events_pending() {
            let _ = gtk::main_iteration_do(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
