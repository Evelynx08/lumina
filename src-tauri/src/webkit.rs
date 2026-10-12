//! Lo que se engancha a la webview de WhatsApp por debajo de Tauri.
//!
//! Todo va por señales nativas de WebKitGTK y no por IPC: la página es remota y
//! no tiene ningún comando concedido, así que lo que necesita la app (avisos,
//! permisos, menú contextual, colores) se resuelve aquí, del lado de Rust.

use std::cell::RefCell;

use tauri::AppHandle;
use webkit2gtk::glib::prelude::ObjectExt;
use webkit2gtk::{
    CacheModel, HitTestResultExt, NotificationPermissionRequest,
    PermissionRequestExt, SecurityOrigin, SettingsExt, UserContentInjectedFrames,
    UserContentManagerExt, UserMediaPermissionRequest, UserStyleLevel, UserStyleSheet,
    WebContextExt, WebViewExt,
};

pub fn reducir_cache(vista: &webkit2gtk::WebView) {
    if let Some(contexto) = vista.context() {
        // Solo hay una aplicación web: retener páginas como un navegador de
        // muchas pestañas desperdicia RAM. Conservamos una caché de recursos moderada.
        contexto.set_cache_model(CacheModel::DocumentBrowser);
    }
    if let Some(ajustes) = vista.settings() {
        // Wry la activa por defecto; Lumina no tiene navegación atrás/adelante.
        ajustes.set_enable_page_cache(false);
    }
}

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
pub fn estilos(vista: &webkit2gtk::WebView, css: String) {
    thread_local! {
        // La hoja de usuario sobrevive a las recargas. Volver al foco con el
        // mismo CSS no debe invalidar estilos ni volver a decodificar el fondo.
        static ULTIMO_CSS: RefCell<String> = const { RefCell::new(String::new()) };
    }
    let Some(gestor) = vista.user_content_manager() else {
        return;
    };
    ULTIMO_CSS.with_borrow_mut(|anterior| {
        if *anterior == css {
            return;
        }
        gestor.remove_all_style_sheets();
        gestor.add_style_sheet(&UserStyleSheet::new(
            &css,
            UserContentInjectedFrames::TopFrame,
            UserStyleLevel::User,
            &[],
            &[],
        ));
        *anterior = css;
    });
}
