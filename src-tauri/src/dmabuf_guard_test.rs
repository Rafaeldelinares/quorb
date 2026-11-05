//! Test determinista del gate de DMABUF (issue #168).
//!
//! La regla es pura y no toca GDK ni el entorno: el test distingue Wayland
//! de X11/XWayland, que es lo que pide el mantenedor para el criterio de
//! cierre del issue.

use super::dmabuf_guard::should_disable_dmabuf;
use crate::widget_placement::DisplayKind;

#[test]
fn wayland_desactiva_dmabuf() {
    assert!(should_disable_dmabuf(DisplayKind::Wayland));
}

#[test]
fn x11_no_desactiva_dmabuf() {
    // Bajo X11/XWayland la ruta DMABUF puede seguir: el problema es específico
    // de Wayland, y desactivarla cambiaría el comportamiento de quien no lo
    // necesita.
    assert!(!should_disable_dmabuf(DisplayKind::X11));
}

#[test]
fn backend_desconocido_no_desactiva_dmabuf() {
    // Sin display GDK no se sabe el backend efectivo: no se toca el entorno.
    assert!(!should_disable_dmabuf(DisplayKind::Unknown));
}
