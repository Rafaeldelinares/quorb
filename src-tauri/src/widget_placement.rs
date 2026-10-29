//! Colocación nativa del widget en Linux/X11 (#179).
//!
//! La ventana del widget nace con `resizable: false`; bajo X11 el gestor
//! descarta cualquier resize programático (pistas fijas min=max=320×260) y la
//! tira nunca alcanza el borde del área de trabajo. Este módulo abre el
//! permiso de resize **programático** y fija la ventana con pistas iguales
//! (`min == max == tamaño de la tira`): el gestor no puede ofrecer otro
//! tamaño y GTK recorta cualquier reconfiguración, así que el resize manual es
//! imposible mientras el resize de programa sí se aplica.
//!
//! El ciclo es: pedir tamaño y posición → mapear → leer lo que concedió el
//! gestor → recolocar con la geometría efectiva → leer de vuelta. Cada llamada
//! nativa salta al hilo de UI; la espera está acotada y una colocación
//! posterior cancela la pendiente. Los read-backs salen de GDK: el origen
//! **absoluto del cliente** (`gdk_window_get_origin`, que falla si la ventana
//! no es visible, así que un read-back previo al mapeo nunca se informa como
//! aplicado) y el tamaño (`gdk_window_get_geometry`).

use serde::{Deserialize, Serialize};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::detail_geometry::Bounds;

/// Tolerancia al comparar la geometría efectiva con la pedida: el gestor
/// redondea el tamaño lógico a píxeles de dispositivo enteros.
const TOLERANCE: i32 = 1;
/// Lecturas permitidas por fase mientras el gestor se estabiliza.
pub const READS: usize = 4;
/// Espera de estabilización entre lecturas.
pub const SETTLE: Duration = Duration::from_millis(15);

/// Cómo respondió la superficie nativa a una colocación.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// La geometría efectiva se leyó del compositor y coincide con la pedida.
    Applied,
    /// La solicitud se aplicó pero no se pudo leer (plataformas sin read-back).
    Unverified,
    /// El gestor no concedió la geometría pedida.
    Limited,
    /// La colocación se canceló: otra posterior la sustituyó.
    Superseded,
}

/// Motivo por el que una colocación no pudo verificarse: un **código**, no
/// texto de interfaz (la etiqueta la pone el frontend).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// El gestor no concedió el tamaño pedido.
    SizeRefused,
    /// El gestor no colocó la tira donde se pidió.
    PositionRefused,
    /// La ventana todavía no era visible: no hay read-back del compositor.
    NotViewable,
    /// La lectura nativa falló.
    ReadFailed,
    /// La colocación fue sustituida por otra más reciente.
    Superseded,
}

/// Resultado de una colocación: la geometría que quedó en pantalla.
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub status: Status,
    /// La geometría efectiva leída; `None` si no se pudo leer o se canceló.
    pub bounds: Option<Bounds>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
}

/// Borde del área de trabajo al que la tira está anclada.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Anchor {
    Top,
    Bottom,
    Left,
    Right,
}

/// Lo que pide el frontend: el rectángulo, el borde anclado y el factor con el
/// que se calculó (en DPI mixto, el de la ventana puede ser otro).
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Placement {
    #[serde(flatten)]
    pub bounds: Bounds,
    pub anchor: Anchor,
    /// El factor de escala con el que se calculó el rectángulo.
    pub scale: f64,
}

/// La geometría efectiva que se leyó del compositor (píxeles físicos).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// Origen absoluto del cliente.
    pub origin: (i32, i32),
    /// Tamaño del cliente.
    pub size: (i32, i32),
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Las operaciones nativas que el ciclo necesita: la implementación real salta
/// al hilo de UI y el doble de los tests responde en el acto.
pub trait Surface {
    /// Pide el tamaño fijo: pistas iguales y resize programático.
    fn request_size(&mut self, width: i32, height: i32) -> impl Future<Output = ()> + Send;
    /// Mueve la superficie nativa.
    fn request_position(&mut self, x: i32, y: i32) -> impl Future<Output = ()> + Send;
    /// Mapea la ventana si no lo está: sin mapear, el compositor no responde.
    fn map(&mut self) -> impl Future<Output = ()> + Send;
    /// Lee la geometría del cliente. `None` si todavía no es visible (es
    /// reintentable); `Err` sólo si la lectura falló de verdad.
    fn read_geometry(&mut self) -> impl Future<Output = Result<Option<Geometry>, String>> + Send;
    /// Espera a que el gestor responda.
    fn settle(&mut self) -> impl Future<Output = ()> + Send;
}

/// Transporte de la superficie nativa: X11/XWayland o native Wayland.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// X11 o XWayland: la ventana es un cliente X11 con posición absoluta.
    X11,
    /// Native Wayland: sin posición absoluta ni pistas de tamaño del gestor.
    Other,
}

/// Clasifica el tipo del display de GDK: X11 y XWayland comparten `GdkX11Display`.
#[cfg(any(target_os = "linux", test))]
pub fn transport_from_type_name(name: &str) -> Transport {
    if name.contains("X11") {
        Transport::X11
    } else {
        Transport::Other
    }
}

/// Píxeles lógicos que pide GTK para un tamaño físico (GTK mide en lógicos).
pub fn to_logical(physical: i32, scale: f64) -> i32 {
    (physical as f64 / scale).round() as i32
}

/// Píxeles físicos que representa un tamaño lógico de GTK.
pub fn to_physical(logical: i32, scale: f64) -> i32 {
    (logical as f64 * scale).round() as i32
}

/// True cuando el tamaño concedido coincide con el pedido.
pub fn size_matches(requested: (i32, i32), granted: (i32, i32)) -> bool {
    matches_pair(requested, granted)
}

/// True cuando la posición concedida coincide con la pedida.
pub fn position_matches(requested: (i32, i32), granted: (i32, i32)) -> bool {
    matches_pair(requested, granted)
}

fn matches_pair(requested: (i32, i32), granted: (i32, i32)) -> bool {
    (requested.0 - granted.0).abs() <= TOLERANCE && (requested.1 - granted.1).abs() <= TOLERANCE
}

/// La posición que conserva el borde anclado con el tamaño efectivo: si el
/// gestor concede otra altura, el borde derecho o inferior no se despega.
pub fn anchored_position(placement: &Placement, size: (i32, i32)) -> (i32, i32) {
    match placement.anchor {
        Anchor::Top | Anchor::Left => (placement.bounds.x, placement.bounds.y),
        Anchor::Bottom => (
            placement.bounds.x,
            placement.bounds.y + placement.bounds.h as i32 - size.1,
        ),
        Anchor::Right => (
            placement.bounds.x + placement.bounds.w as i32 - size.0,
            placement.bounds.y,
        ),
    }
}

/// Corre el ciclo: tamaño y posición → mapear → read-back → recolocar → read-back.
/// Las lecturas están acotadas y una colocación posterior detiene el ciclo.
pub async fn drive<S: Surface>(
    surface: &mut S,
    placement: &Placement,
    mut superseded: impl FnMut() -> bool,
) -> Report {
    let wanted = (placement.bounds.w as i32, placement.bounds.h as i32);
    // Tamaño y posición se piden antes de mapear: la ventana aparece ya con su
    // geometría (sin el destello del tamaño por defecto). El cancelador se
    // comprueba **antes** de cada fase nativa: una colocación posterior (o un
    // arrastre) no debe dejar la ventana movida a medias.
    if superseded() {
        return superseded_report();
    }
    surface.request_size(wanted.0, wanted.1).await;
    let wanted_position = anchored_position(placement, wanted);
    if superseded() {
        return superseded_report();
    }
    surface
        .request_position(wanted_position.0, wanted_position.1)
        .await;
    if superseded() {
        return superseded_report();
    }
    surface.map().await;

    // El read-back sólo vale con la ventana visible: es lo que prueba que el
    // compositor concedió la geometría.
    let mut effective = match read_until(surface, wanted).await {
        Ok(effective) => effective,
        Err(reason) => return failed_report(reason),
    };
    if !size_matches(wanted, effective.size) {
        // El compositor todavía no ha concedido el tamaño pedido: se reintenta
        // con la ventana ya visible (una petición real al gestor) y, si tampoco,
        // las pistas vuelven al tamaño real para que la ventana no salte a un
        // tamaño imposible más tarde.
        if superseded() {
            return superseded_report();
        }
        surface.request_size(wanted.0, wanted.1).await;
        effective = match read_until(surface, wanted).await {
            Ok(effective) => effective,
            Err(reason) => return failed_report(reason),
        };
        if !size_matches(wanted, effective.size) {
            if superseded() {
                return superseded_report();
            }
            surface
                .request_size(effective.size.0, effective.size.1)
                .await;
        }
    }
    let target = anchored_position(placement, effective.size);
    if superseded() {
        return superseded_report();
    }
    surface.request_position(target.0, target.1).await;
    let mut origin = effective.origin;
    for _ in 0..READS {
        if position_matches(target, origin) {
            break;
        }
        if superseded() {
            return superseded_report();
        }
        surface.settle().await;
        match surface.read_geometry().await {
            Ok(Some(geometry)) => origin = geometry.origin,
            Ok(None) => {}
            Err(_) => return failed_report(Reason::ReadFailed),
        }
    }
    report_of(wanted, effective, origin, target)
}

/// Lee hasta que el gestor conceda el tamaño pedido (o se agote el presupuesto).
/// `None` (aún no visible) es reintentable; sólo un fallo real es error.
async fn read_until<S: Surface>(surface: &mut S, wanted: (i32, i32)) -> Result<Geometry, Reason> {
    let mut last: Option<Geometry> = None;
    for _ in 0..READS {
        surface.settle().await;
        match surface.read_geometry().await {
            Ok(Some(geometry)) => {
                if size_matches(wanted, geometry.size) {
                    return Ok(geometry);
                }
                last = Some(geometry);
            }
            Ok(None) => {}
            Err(_) => return Err(Reason::ReadFailed),
        }
    }
    last.ok_or(Reason::NotViewable)
}

/// Compone el informe final con la geometría efectiva y la posición leída.
fn report_of(
    wanted: (i32, i32),
    effective: Geometry,
    origin: (i32, i32),
    target: (i32, i32),
) -> Report {
    let size_ok = size_matches(wanted, effective.size);
    let position_ok = position_matches(target, origin);
    Report {
        status: if size_ok && position_ok {
            Status::Applied
        } else {
            Status::Limited
        },
        bounds: Some(Bounds {
            x: origin.0,
            y: origin.1,
            w: effective.size.0.max(0) as u32,
            h: effective.size.1.max(0) as u32,
        }),
        reason: if size_ok && position_ok {
            None
        } else if size_ok {
            Some(Reason::PositionRefused)
        } else {
            Some(Reason::SizeRefused)
        },
    }
}

/// Una colocación cancelada: la geometría quedó a medias y no se informa.
fn superseded_report() -> Report {
    Report {
        status: Status::Superseded,
        bounds: None,
        reason: Some(Reason::Superseded),
    }
}

/// Una colocación sin lectura fiable: no hay geometría que cachear.
fn failed_report(reason: Reason) -> Report {
    Report {
        status: Status::Unverified,
        bounds: None,
        reason: Some(reason),
    }
}

/// Reserva la siguiente generación: una colocación posterior cancela la pendiente.
pub fn next_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::SeqCst) + 1
}

/// Cancela la colocación pendiente (el usuario empezó a arrastrar la tira).
pub fn cancel() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// True cuando una colocación posterior sustituyó a la `generation` dada.
pub fn superseded(generation: u64) -> bool {
    GENERATION.load(Ordering::SeqCst) != generation
}

/// Coloca el widget en la superficie nativa y devuelve la geometría efectiva.
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
pub async fn place(
    window: &tauri::WebviewWindow,
    placement: Placement,
    generation: u64,
) -> Result<Report, String> {
    // Antes de tocar la superficie nativa: sólo la ventana del widget puede
    // colocarse y el rectángulo tiene que ser utilizable (no cero, acotado y
    // sin desbordamiento). Vale para todos los transportes.
    let bounds = placement.bounds.validate_widget(window.label())?;
    #[cfg(target_os = "linux")]
    {
        // GDK sólo se puede tocar en el hilo de UI: la detección del transporte
        // va allí, como todas las llamadas nativas.
        if transport_on_ui(window).await == Transport::X11 {
            let mut surface = UiSurface::new(window.clone(), placement.scale);
            return Ok(drive(&mut surface, &placement, || superseded(generation)).await);
        }
    }
    // Sin read-back en esta plataforma: la solicitud se aplica como antes, y
    // la llamada nativa sigue en el hilo de UI.
    let (tx, mut rx) = tauri::async_runtime::channel(1);
    let ui_window = window.clone();
    window
        .run_on_main_thread(move || {
            let _ = tx.try_send(crate::detail_geometry::apply_widget(&ui_window, bounds));
        })
        .map_err(|e| e.to_string())?;
    rx.recv()
        .await
        .ok_or_else(|| "Widget bounds UI callback was dropped".to_string())?
        .map(|()| Report {
            status: Status::Unverified,
            bounds: None,
            reason: None,
        })
}

/// Fija el tamaño de la ventana nativa (#179).
///
/// Pistas iguales (`min == max == tamaño`): el gestor no puede ofrecer otro
/// tamaño y GTK recorta cualquier reconfiguración, así que el resize manual es
/// imposible (y sin decoraciones no hay bordes que arrastrar). El permiso de
/// resize programático sí queda activo: es lo que permite darle el tamaño.
#[cfg(target_os = "linux")]
pub fn fix_size<W: gtk::prelude::GtkWindowExt>(window: &W, width: i32, height: i32) {
    window.set_resizable(true);
    let geometry = gtk::gdk::Geometry::new(
        width,
        height,
        width,
        height,
        0,
        0,
        0,
        0,
        0.0,
        0.0,
        gtk::gdk::Gravity::Center,
    );
    window.set_geometry_hints(
        None::<&gtk::Widget>,
        Some(&geometry),
        gtk::gdk::WindowHints::MIN_SIZE | gtk::gdk::WindowHints::MAX_SIZE,
    );
    window.resize(width, height);
}

/// Mapea la ventana si no lo está. La visibilidad de usuario sigue siendo del
/// frontend; aquí sólo hace falta que el compositor responda al read-back.
#[cfg(target_os = "linux")]
pub fn map_if_needed<W: gtk::prelude::GtkWindowExt + gtk::prelude::WidgetExt>(window: &W) {
    if !window.is_mapped() {
        window.show_all();
    }
}

/// Corre una llamada nativa en el hilo de UI y espera su resultado.
#[cfg(target_os = "linux")]
async fn on_ui<T: Send + 'static>(
    window: &tauri::WebviewWindow,
    call: impl FnOnce(&gtk::ApplicationWindow) -> T + Send + 'static,
) -> Option<T> {
    let (tx, mut rx) = tauri::async_runtime::channel(1);
    let target = window.clone();
    let inner = window.clone();
    target
        .run_on_main_thread(move || {
            // La ventana GTK sólo se usa aquí: nunca cruza a otro hilo.
            let value = inner.gtk_window().ok().map(|gtk| call(&gtk));
            let _ = tx.try_send(value);
        })
        .ok()?;
    rx.recv().await.flatten()
}

/// La superficie GTK: cada llamada nativa ocurre en el hilo de UI.
#[cfg(target_os = "linux")]
struct UiSurface {
    window: tauri::WebviewWindow,
    /// El factor con el que se calculó la petición (no el de la ventana: en DPI
    /// mixto pueden diferir).
    scale: f64,
}

#[cfg(target_os = "linux")]
impl UiSurface {
    fn new(window: tauri::WebviewWindow, scale: f64) -> Self {
        UiSurface { window, scale }
    }
}

#[cfg(target_os = "linux")]
impl Surface for UiSurface {
    async fn request_size(&mut self, width: i32, height: i32) {
        let (w, h) = (
            to_logical(width, self.scale),
            to_logical(height, self.scale),
        );
        on_ui(&self.window, move |window| fix_size(window, w, h)).await;
    }

    async fn request_position(&mut self, x: i32, y: i32) {
        let (x, y) = (to_logical(x, self.scale), to_logical(y, self.scale));
        on_ui(&self.window, move |window| {
            use gtk::prelude::*;
            window.move_(x, y);
        })
        .await;
    }

    async fn map(&mut self) {
        on_ui(&self.window, map_if_needed).await;
    }

    async fn read_geometry(&mut self) -> Result<Option<Geometry>, String> {
        let scale = self.scale;
        on_ui(&self.window, move |window| {
            use gtk::prelude::*;
            let gdk_window = window.window()?;
            // El origen absoluto del cliente: `gdk_window_get_origin` falla si la
            // ventana no es visible, así que un read-back previo al mapeo no se
            // puede confundir con la respuesta del compositor.
            let (viewable, x, y) = gdk_window.origin();
            if viewable == 0 {
                return None;
            }
            let (_, _, width, height) = gdk_window.geometry();
            Some(Geometry {
                origin: (to_physical(x, scale), to_physical(y, scale)),
                size: (to_physical(width, scale), to_physical(height, scale)),
            })
        })
        .await
        .ok_or_else(|| "the native window could not be read".to_string())
    }

    async fn settle(&mut self) {
        // El hilo de UI queda libre mientras el gestor responde.
        tokio::time::sleep(SETTLE).await;
    }
}

/// El transporte de la superficie nativa, detectado del display real.
/// **Sólo en el hilo de UI**: GDK no se puede tocar desde otro hilo.
#[cfg(target_os = "linux")]
fn transport() -> Transport {
    static TRANSPORT: std::sync::OnceLock<Transport> = std::sync::OnceLock::new();
    *TRANSPORT.get_or_init(detect_transport)
}

#[cfg(target_os = "linux")]
fn detect_transport() -> Transport {
    use gtk::prelude::*;
    match gtk::gdk::Display::default() {
        Some(display) => transport_from_type_name(display.type_().name()),
        None => Transport::Other,
    }
}

/// Detecta el transporte en el hilo de UI (GDK no admite otro hilo).
#[cfg(target_os = "linux")]
async fn transport_on_ui(window: &tauri::WebviewWindow) -> Transport {
    on_ui(window, |_| transport())
        .await
        .unwrap_or(Transport::Other)
}
