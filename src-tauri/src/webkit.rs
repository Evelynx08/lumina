//! Lo que se engancha a la webview de WhatsApp por debajo de Tauri.
//!
//! Todo va por señales nativas de WebKitGTK y no por IPC: la página es remota y
//! no tiene ningún comando concedido, así que lo que necesita la app (avisos,
//! permisos, menú contextual, colores) se resuelve aquí, del lado de Rust.

use tauri::AppHandle;
use webkit2gtk::glib::prelude::ObjectExt;
use webkit2gtk::{
    HitTestResultExt, NotificationPermissionRequest, PermissionRequestExt, SecurityOrigin,
    UserContentInjectedFrames, UserContentManagerExt, UserMediaPermissionRequest, UserStyleLevel,
    UserStyleSheet, WebContextExt, WebViewExt,
};

pub fn integrar(app: AppHandle, whatsapp: tauri::Webview, vista: &webkit2gtk::WebView) {
    // Avisos para los mensajes; micrófono y cámara para las notas de voz y
    // las fotos; portapapeles para pegar imágenes. Lo demás (ubicación, puntero bloqueado…) WhatsApp Web no lo
    // necesita y se niega sin preguntar.
    vista.connect_permission_request(|_, peticion| {
        // El de portapapeles (WebKitGTK ≥ 2.42) no existe en el crate, que se
        // queda en la API 2.40: se reconoce por el nombre del tipo. Hace falta
        // para pegar capturas (ver `pagina.js`).
        if peticion.is::<NotificationPermissionRequest>()
            || peticion.is::<UserMediaPermissionRequest>()
            || peticion.type_().name() == "WebKitClipboardPermissionRequest"
        {
            peticion.allow();
        } else {
            eprintln!("lumina: permiso denegado: {}", peticion.type_().name());
            peticion.deny();
        }
        true
    });

    // Sin esto WhatsApp arranca con `Notification.permission = "default"` y
    // enseña su banner de «notificaciones desactivadas» hasta que se pulsa
    // «Activar». La señal llega cuando WebKit lanza un proceso web nuevo; la
    // llamada directa cubre el que ya pudiera estar en marcha.
    if let Some(contexto) = vista.context() {
        let conceder = |c: &webkit2gtk::WebContext| {
            c.initialize_notification_permissions(
                &[&SecurityOrigin::for_uri("https://web.whatsapp.com/")],
                &[],
            );
        };
        conceder(&contexto);
        contexto.connect_initialize_notification_permissions(conceder);
    }

    vista.connect_show_notification(move |_, aviso| {
        crate::notificaciones::mostrar_mensaje(&app, aviso);
        true
    });

    // Devolver `true` deja sin abrir el menú de GTK. En su lugar se dibuja el
    // de BookOS dentro de la página (`pagina.js`). WhatsApp ya hace
    // `preventDefault` en sus propios menús, que ni siquiera llegan aquí.
    vista.connect_context_menu(move |_, _, evento, objetivo| {
        let (x, y) = evento.coords().unwrap_or_default();
        let enlace = objetivo.link_uri().map(|u| u.to_string());
        let datos = serde_json::json!({
            "x": x,
            "y": y,
            "enlace": enlace,
            "editable": objetivo.context_is_editable(),
            "seleccion": objetivo.context_is_selection(),
        });
        let _ = whatsapp.eval(format!(
            "window.__luminaMenu && window.__luminaMenu({datos})"
        ));
        true
    });
}

/// Sustituye las hojas de usuario de la página. WebKit las reaplica en vivo,
/// sin recargar.
///
/// Nivel `User` porque así un `!important` nuestro gana al `!important` de
/// WhatsApp en la cascada; con nivel autor dependería del orden de carga.
pub fn estilos(vista: &webkit2gtk::WebView, css: &str) {
    let Some(gestor) = vista.user_content_manager() else {
        return;
    };
    gestor.remove_all_style_sheets();
    gestor.add_style_sheet(&UserStyleSheet::new(
        css,
        UserContentInjectedFrames::TopFrame,
        UserStyleLevel::User,
        &[],
        &[],
    ));
}

/// Monta la barra encima de WhatsApp.
///
/// En Linux, wry mete las webviews hijas en un `GtkBox` vertical y no hace
/// caso de `set_position` ni `set_size` (medido: con la ventana a 760 px el
/// reparto era 570 para WhatsApp arriba y 190 para la barra abajo, antes y
/// después de mostrar la ventana). Se cambia el `GtkBox` por un `GtkOverlay`:
/// WhatsApp de fondo con margen arriba, la barra flotando encima, y así la
/// barra puede crecer sobre WhatsApp para enseñar su menú. GTK solo hace de
/// contenedor; no pinta nada.
pub fn superponer(barra: &webkit2gtk::WebView, alto: i32) {
    use gtk::prelude::*;
    let caja: gtk::Box = barra
        .parent()
        .and_then(|p| p.downcast().ok())
        .expect("wry pone las webviews hijas en un GtkBox");
    let barra: gtk::Widget = barra.clone().upcast();
    let whatsapp = caja
        .children()
        .into_iter()
        .find(|hijo| *hijo != barra)
        .expect("la webview de WhatsApp se añade antes que la barra");
    caja.remove(&whatsapp);
    caja.remove(&barra);

    let capas = gtk::Overlay::new();
    whatsapp.set_margin_top(alto);
    capas.add(&whatsapp);
    barra.set_valign(gtk::Align::Start);
    barra.set_size_request(-1, alto);
    capas.add_overlay(&barra);
    caja.pack_start(&capas, true, true, 0);
    capas.show_all();
}

/// Con el menú abierto la barra cubre la ventana entera, transparente: su
/// velo atrapa el clic de fuera que lo cierra.
///
/// No basta con `valign = Fill`: medido, la asignación seguía en 36 px. Lo que
/// sí respeta el overlay es el `size_request`, así que se pide el alto entero.
pub fn desplegar(barra: &webkit2gtk::WebView, abierta: bool, alto_cerrada: i32) {
    use gtk::prelude::*;
    let alto = match (abierta, barra.parent()) {
        (true, Some(capas)) => capas.allocated_height(),
        _ => alto_cerrada,
    };
    barra.set_size_request(-1, alto);
}
