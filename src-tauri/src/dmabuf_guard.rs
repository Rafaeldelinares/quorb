//! Mitigación del rectángulo negro por DMABUF en Wayland nativo (issue #168).
//!
//! **Qué pasa.** Sobre Wayland, WebKitGTK compone el buffer del widget con
//! DMABUF. Cuando una superficie WebKitGTK transparente ya visible cambia de
//! tamaño, el compositor puede componer memoria sin inicializar y pintar un
//! rectángulo negro opaco sobre el escritorio.
//!
//! **La mitigación.** `WEBKIT_DISABLE_DMABUF_RENDERER=1` hace que WebKitGTK
//! use SHM en lugar de DMABUF. Es una variable de entorno que el toolkit lee
//! al inicializarse, así que tiene que estar seteada **antes** de que exista
//! la primera webview; por eso esta llamada vive al principio de `run()` y no
//! en el hook `setup()` (donde Tauri ya creó la webview del widget).
//!
//! **Por qué el backend efectivo y no `XDG_SESSION_TYPE`.** La misma sesión
//! puede correr la app bajo XWayland (`GDK_BACKEND=x11`), y ahí la ruta DMABUF
//! no es el problema. El backend efectivo se lee del display GDK, el mismo
//! detector que usa `widget_placement` para colocar la tira: así la
//! mitigación no puede discrepar del resto de las decisiones de plataforma.
//!
//! **Por qué no se cachea `Unknown`.** Si el detector se consultara antes de
//! que exista el display GDK, su caché quedaría fijada en `Unknown` para
//! siempre. Por eso `display_kind()` solo cachea un backend ya resuelto, y
//! esta mitigación inicializa GDK antes de consultarlo.

use crate::widget_placement::DisplayKind;

/// ¿Corresponde desactivar DMABUF para este backend?
///
/// Pura y sin GDK: es la regla que el test determinista debe cubrir
/// (Wayland → sí; X11/XWayland → no; desconocido → no).
pub fn should_disable_dmabuf(kind: DisplayKind) -> bool {
    matches!(kind, DisplayKind::Wayland)
}

/// Aplica la mitigación antes de que exista la primera webview.
///
/// Es idempotente: si la variable ya está seteada, no hace nada.
#[cfg(target_os = "linux")]
pub fn apply_before_first_webview() {
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_some() {
        return;
    }
    // GDK inicializado y en el hilo principal: sin display no hay backend
    // efectivo que mirar. `gtk::init()` usa `gtk_init_check`, así que si no
    // hay display devuelve Err en vez de abortar la app.
    if !gtk::is_initialized() {
        if let Err(e) = gtk::init() {
            eprintln!("Quorb: no hay display GDK ({e}); no se desactiva DMABUF.");
            return;
        }
    }
    let kind = crate::widget_placement::display_kind();
    if !should_disable_dmabuf(kind) {
        return;
    }
    // WebKitGTK lee esta variable al inicializar el toolkit: tiene que estar
    // seteada antes de que se cree la primera webview.
    std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
}

/// En macOS y Windows la ruta DMABUF no aplica: el compositor no compone
/// buffers WebKitGTK sin inicializar de esta forma.
#[cfg(not(target_os = "linux"))]
pub fn apply_before_first_webview() {}
