//! Capturas de la barra nativa y de WhatsApp, solo en depuración.
//!
//! Existe para mirar lo que se pinta sin depender de una herramienta de
//! captura del escritorio (Spectacle no funciona en la sesión BookOS). Uso:
//!
//!     LUMINA_CAPTURA=/ruta/ventana.png LUMINA_CAPTURA_ESPERA=15 \
//!     LUMINA_CAPTURA_MENU=1 \
//!     cargo run
//!
//! Deja `ventana.barra.png`, `ventana.menu.png` si está abierto, y
//! `ventana.whatsapp.png`. `LUMINA_CAPTURA_JS_WHATSAPP` se ejecuta un segundo
//! antes de fotografiar WhatsApp. `LUMINA_CAPTURA_CLIC=x,y` pulsa después en ese punto de WhatsApp
//! con un clic de GDK, que WebKit cuenta como gesto del usuario: un `eval` no
//! lo es, y sin gesto el portapapeles ni siquiera llega a pedir permiso.
//! `LUMINA_CAPTURA_PAUSA=s` espera esos segundos más antes de fotografiar.
//! `LUMINA_CAPTURA_BLOQUEO=pin|sistema|nuevo` enseña esa pantalla de bloqueo
//! (sin guardar nada) y deja `ventana.bloqueo.png`.
//! `LUMINA_CAPTURA_FOCO=1` le da antes el foco a la ventana: el compositor solo
//! deja tocar el portapapeles al cliente con el foco del teclado, y una ventana
//! lanzada desde un terminal lo pierde al segundo (medido con `WAYLAND_DEBUG`).

use std::time::Duration;

use tauri::{Manager, Window};

pub fn programar(ventana: &Window) {
    let Some(ruta) = std::env::var_os("LUMINA_CAPTURA") else {
        return;
    };
    let espera = std::env::var("LUMINA_CAPTURA_ESPERA")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12);
    let abrir_menu = std::env::var_os("LUMINA_CAPTURA_MENU").is_some();
    let guion_whatsapp = std::env::var("LUMINA_CAPTURA_JS_WHATSAPP").ok();
    let ventana = ventana.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(espera));
        // El compositor solo deja cambiar el portapapeles al cliente con el
        // foco del teclado: sin esto, cualquier prueba de copiar falla siempre.
        if std::env::var_os("LUMINA_CAPTURA_FOCO").is_some() {
            let _ = ventana.set_focus();
            std::thread::sleep(Duration::from_secs(1));
        }
        if let (Some(js), Some(vista)) =
            (guion_whatsapp, ventana.app_handle().get_webview("whatsapp"))
        {
            let _ = vista.eval(js);
            std::thread::sleep(Duration::from_secs(1));
        }
        if let Ok(como) = std::env::var("LUMINA_CAPTURA_BLOQUEO") {
            let app = ventana.app_handle().clone();
            let _ = ventana
                .app_handle()
                .run_on_main_thread(move || crate::bloqueo::probar(&app, &como));
            std::thread::sleep(Duration::from_secs(1));
        }
        if abrir_menu {
            let _ = ventana
                .app_handle()
                .run_on_main_thread(crate::barra::abrir_menu);
            std::thread::sleep(Duration::from_secs(1));
        }
        let ruta_nativa = std::path::PathBuf::from(&ruta);
        let _ = ventana
            .app_handle()
            .run_on_main_thread(move || {
                crate::barra::capturar(&ruta_nativa);
                crate::bloqueo::capturar(&ruta_nativa);
            });
        if let (Some((x, y)), Some(vista)) = (
            std::env::var("LUMINA_CAPTURA_CLIC").ok().and_then(|c| {
                let (x, y) = c.split_once(',')?;
                Some((x.parse::<f64>().ok()?, y.parse::<f64>().ok()?))
            }),
            ventana.app_handle().get_webview("whatsapp"),
        ) {
            let _ = vista.with_webview(move |w| clic(&w.inner(), x, y));
            std::thread::sleep(Duration::from_secs(1));
        }
        if let Some(pausa) = std::env::var("LUMINA_CAPTURA_PAUSA")
            .ok()
            .and_then(|p| p.parse().ok())
        {
            std::thread::sleep(Duration::from_secs(pausa));
        }
        // GDK no ve lo que WebKit pinta por GL (sale una ventana en blanco),
        // así que cada webview se fotografía con su propia instantánea.
        for etiqueta in ["whatsapp"] {
            let Some(vista) = ventana.app_handle().get_webview(etiqueta) else {
                continue;
            };
            let destino = std::path::PathBuf::from(&ruta).with_extension(format!("{etiqueta}.png"));
            let _ = vista.with_webview(move |w| {
                use webkit2gtk::{SnapshotOptions, SnapshotRegion, WebViewExt};
                w.inner().snapshot(
                    SnapshotRegion::Visible,
                    SnapshotOptions::TRANSPARENT_BACKGROUND,
                    None::<&webkit2gtk::gio::Cancellable>,
                    move |resultado| {
                        let imagen = resultado.map_err(|e| e.to_string()).and_then(|superficie| {
                            let superficie = gtk::cairo::ImageSurface::try_from(superficie)
                                .map_err(|_| "la instantánea no es una imagen".to_string())?;
                            gtk::gdk::pixbuf_get_from_surface(
                                &superficie,
                                0,
                                0,
                                superficie.width(),
                                superficie.height(),
                            )
                            .ok_or_else(|| "superficie vacía".to_string())
                        });
                        match imagen
                            .and_then(|i| i.savev(&destino, "png", &[]).map_err(|e| e.to_string()))
                        {
                            Ok(()) => eprintln!("lumina: captura en {}", destino.display()),
                            Err(e) => eprintln!("lumina: captura de {etiqueta} fallida: {e}"),
                        }
                    },
                );
            });
        }
    });
}

/// Pulsación y suelta del botón izquierdo en `(x, y)` de la vista, entregadas
/// al bucle de GTK como si vinieran del ratón.
fn clic(vista: &webkit2gtk::WebView, x: f64, y: f64) {
    use gtk::glib::translate::{ToGlibPtr, ToGlibPtrMut};
    use gtk::prelude::*;
    let ventana_gdk = vista.window().expect("vista ya realizada");
    let raton = vista
        .display()
        .default_seat()
        .and_then(|asiento| asiento.pointer())
        .expect("la sesión tiene ratón");
    for tipo in [
        gtk::gdk::EventType::ButtonPress,
        gtk::gdk::EventType::ButtonRelease,
    ] {
        let mut evento = gtk::gdk::Event::new(tipo);
        unsafe {
            let crudo: *mut gtk::gdk::ffi::GdkEvent = evento.to_glib_none_mut().0;
            let boton = &mut (*crudo).button;
            boton.window = ventana_gdk.to_glib_full();
            boton.x = x;
            boton.y = y;
            boton.button = 1;
            boton.time = gtk::current_event_time();
        }
        evento.set_device(Some(&raton));
        gtk::main_do_event(&mut evento);
    }
}
