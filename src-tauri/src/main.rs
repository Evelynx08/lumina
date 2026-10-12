#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autenticar;
mod barra;
mod bloqueo;
#[cfg(debug_assertions)]
mod captura;
mod fondo;
mod notificaciones;
mod webkit;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};

use tauri::webview::{DownloadEvent, NewWindowResponse, WebviewBuilder};
use tauri::window::WindowBuilder;
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, Theme, WebviewUrl, Window, WindowEvent,
};

const WHATSAPP: &str = "https://web.whatsapp.com/";
/// Alto de la barra de título (HIG §5).
const ALTO_BARRA: f64 = 36.0;

const TOKENS: &str = include_str!("../../src/tokens.css");
const ESTILO_WHATSAPP: &str = include_str!("whatsapp.css");
const PAGINA: &str = include_str!("pagina.js");

/// Preferencias restauradas desde el fichero local o la migración de la barra web.
static PALETA_ACTIVA: AtomicBool = AtomicBool::new(true);
/// Último estado visto: arrastrar un borde manda decenas de `Resized`, y
/// rehacer las hojas en cada uno recalcula el estilo de todo WhatsApp.
static MAXIMIZADA: AtomicBool = AtomicBool::new(false);
/// El fondo actual y la última imagen elegida, aunque ahora se use otro modo.
static FONDO: Mutex<fondo::Fondo> = Mutex::new(fondo::Fondo::Escritorio);
static IMAGEN_GUARDADA: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn mostrar_ventana(app: &AppHandle) {
    if let Some(ventana) = app.get_window("main") {
        let _ = ventana.show();
        let _ = ventana.unminimize();
        let _ = ventana.set_focus();
    }
}

/// Enlaces y ficheros van a la aplicación que el usuario tenga por defecto.
pub fn abrir_fuera(que: &OsStr) {
    if let Err(e) = std::process::Command::new("xdg-open").arg(que).spawn() {
        eprintln!("lumina: xdg-open falló: {e}");
    }
}

fn paleta_css() -> String {
    dirs::config_dir()
        .and_then(|d| std::fs::read_to_string(d.join("bookos/palette.css")).ok())
        .unwrap_or_default()
}

/// El tema del escritorio, preguntado al portal XDG, que en BookOS sirve el
/// compositor. Copiado de Reloj: `gdbus` viene con GLib y no añade nada.
/// `color-scheme`: 1 oscuro, 2 claro, 0 sin preferencia.
fn tema_del_portal() -> Option<Theme> {
    let salida = std::process::Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.portal.Desktop",
            "--object-path",
            "/org/freedesktop/portal/desktop",
            "--method",
            "org.freedesktop.portal.Settings.ReadOne",
            "org.freedesktop.appearance",
            "color-scheme",
        ])
        .output()
        .ok()?;
    // Contesta «(<uint32 1>,)».
    let texto = String::from_utf8_lossy(&salida.stdout);
    match texto.split("uint32").nth(1)?.trim_start().chars().next()? {
        '1' => Some(Theme::Dark),
        '2' => Some(Theme::Light),
        _ => None,
    }
}

/// El tema de la ventana es lo que WebKitGTK expone como
/// `prefers-color-scheme`: con eso cambian a la vez la barra, la paleta y el
/// propio WhatsApp, que por defecto sigue al sistema.
fn aplicar_tema(ventana: &Window) {
    let _ = ventana.set_theme(tema_del_portal());
}

/// Recompone las hojas de usuario de WhatsApp: los tokens de reserva, encima
/// la paleta de Ajustes si está activa, y las reglas que los usan.
fn aplicar_estilos(app: &AppHandle) {
    static PETICIONES: OnceLock<SyncSender<AppHandle>> = OnceLock::new();
    let peticiones = PETICIONES.get_or_init(|| {
        let (emisor, receptor) = sync_channel::<AppHandle>(1);
        // Un solo trabajador y una petición pendiente: las ráfagas de foco
        // no acumulan hilos ni varias imágenes decodificadas. Cada turno lee
        // el estado actual, por lo que agrupar peticiones no pierde cambios.
        std::thread::spawn(move || {
            while let Ok(app) = receptor.recv() {
                actualizar_estilos(&app);
            }
        });
        emisor
    });
    match peticiones.try_send(app.clone()) {
        Ok(()) | Err(TrySendError::Full(_)) => {}
        Err(TrySendError::Disconnected(_)) => {
            eprintln!("lumina: el trabajador de estilos se ha detenido");
        }
    }
}

fn actualizar_estilos(app: &AppHandle) {
    let Some(whatsapp) = app.get_webview("whatsapp") else {
        return;
    };
    let maximizada = MAXIMIZADA.load(Ordering::Relaxed);
    let oscuro = app
        .get_window("main")
        .and_then(|v| v.theme().ok())
        .is_some_and(|t| t == Theme::Dark);
    let fondo = FONDO.lock().unwrap_or_else(|e| e.into_inner()).clone();
    // Fuera del hilo de GTK: la primera vez hay que abrir y reducir la imagen
    // del fondo, y eso no puede congelar la ventana.
    let mut css = String::from(TOKENS);
    if PALETA_ACTIVA.load(Ordering::Relaxed) {
        css.push_str(&paleta_css());
    }
    css.push_str(ESTILO_WHATSAPP);
    css.push_str(&fondo::css(&fondo, oscuro));
    // La ventana es transparente y la barra redondea las esquinas de arriba;
    // las de abajo las tiene que recortar la página. Maximizada no hay esquinas.
    if !maximizada {
        css.push_str("html{clip-path:inset(0 round 0 0 20px 20px)!important}");
    }
    if let Err(e) = whatsapp.with_webview(move |w| webkit::estilos(&w.inner(), css)) {
        eprintln!("lumina: no se pudieron actualizar los estilos: {e}");
    }
}

/// Ruta libre en Descargas: «foto.jpg», «foto (1).jpg», «foto (2).jpg»…
fn destino_libre(carpeta: &Path, nombre: &OsStr) -> PathBuf {
    let base = Path::new(nombre);
    let tronco = base.file_stem().unwrap_or(nombre).to_string_lossy();
    let extension = base
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (0..)
        .map(|n| match n {
            0 => carpeta.join(nombre),
            n => carpeta.join(format!("{tronco} ({n}){extension}")),
        })
        .find(|ruta| !ruta.exists())
        .expect("la serie de nombres no se acaba")
}

/// `modo`: «ninguno», «escritorio» o «imagen» (con `ruta`).
fn fondo_chats(app: AppHandle, modo: &str, ruta: Option<PathBuf>) -> Result<(), String> {
    let nuevo = match (modo, ruta) {
        ("ninguno", _) => fondo::Fondo::Ninguno,
        ("escritorio", _) => fondo::Fondo::Escritorio,
        ("imagen", Some(ruta)) => {
            *IMAGEN_GUARDADA.lock().unwrap_or_else(|e| e.into_inner()) = Some(ruta.clone());
            fondo::Fondo::Imagen(ruta)
        }
        _ => return Err(format!("fondo desconocido: {modo}")),
    };
    *FONDO.lock().unwrap_or_else(|e| e.into_inner()) = nuevo;
    aplicar_estilos(&app);
    Ok(())
}

fn paleta_whatsapp(app: AppHandle, activa: bool) {
    PALETA_ACTIVA.store(activa, Ordering::Relaxed);
    aplicar_estilos(&app);
}

fn recargar_whatsapp(app: AppHandle) -> Result<(), String> {
    let whatsapp = app
        .get_webview("whatsapp")
        .ok_or("no hay webview de WhatsApp")?;
    whatsapp.reload().map_err(|e| e.to_string())
}

/// Quita el zoom de página que WebKitGTK hace con el pellizco del touchpad.
/// Igual que en Reloj: el gesto no llega a la página como evento, así que no
/// hay `preventDefault` que lo frene; se apaga el `GtkGestureZoom` de la vista.
fn desactivar_zoom_por_pellizco<R: tauri::Runtime>(
    webview: &tauri::Webview<R>,
    _: &tauri::webview::PageLoadPayload<'_>,
) {
    let _ = webview.with_webview(|webview| {
        use gtk::glib::translate::from_glib_none;
        use gtk::prelude::*;

        // Clave interna de WebKitGTK (WebKitWebViewBase.cpp, 2.52.5). Si cambia,
        // no se encuentra y el zoom vuelve; no rompe nada más.
        let Some(gesto) = (unsafe {
            webview
                .inner()
                .data::<gtk::ffi::GtkGesture>("wk-view-zoom-gesture")
        }) else {
            return;
        };
        let gesto: gtk::Gesture = unsafe { from_glib_none(gesto.as_ptr()) };
        gesto.set_propagation_phase(gtk::PropagationPhase::None);
    });
}

fn main() {
    // El selector de «Adjuntar» de WebKitGTK es un GtkFileChooserNative; con
    // esto va por el portal, que en BookOS abre el explorador del sistema en
    // vez del diálogo de GTK. Tiene que estar antes de que arranque GTK.
    std::env::set_var("GTK_USE_PORTAL", "1");

    tauri::Builder::default()
        // Una segunda instancia (el dock, el lanzador) solo trae la ventana:
        // la primera sigue viva en segundo plano para recibir mensajes.
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            mostrar_ventana(app)
        }))
        .on_page_load(desactivar_zoom_por_pellizco)
        .invoke_handler(tauri::generate_handler![barra::migrar_preferencias])
        .setup(|app| {
            let preferencias_migradas = barra::cargar(app.handle())?;
            let ventana = WindowBuilder::new(app, "main")
                .title("Lumina")
                .inner_size(1100.0, 760.0)
                .min_inner_size(720.0, 520.0)
                .decorations(false)
                .transparent(true)
                .build()?;
            aplicar_tema(&ventana);
            let tam = ventana
                .inner_size()?
                .to_logical::<f64>(ventana.scale_factor()?);

            let app_descargas = app.handle().clone();
            let whatsapp = ventana.add_child(
                WebviewBuilder::new(
                    "whatsapp",
                    WebviewUrl::External(WHATSAPP.parse().expect("URL fija bien formada")),
                )
                .transparent(true)
                .enable_clipboard_access()
                .initialization_script(PAGINA)
                .on_new_window(|url, _| {
                    abrir_fuera(OsStr::new(url.as_str()));
                    NewWindowResponse::Deny
                })
                .on_download(move |_, evento| {
                    match evento {
                        DownloadEvent::Requested { url, destination } => {
                            let carpeta = dirs::download_dir().unwrap_or_else(std::env::temp_dir);
                            let nombre = destination
                                .file_name()
                                .map(OsStr::to_os_string)
                                .or_else(|| url.path_segments()?.next_back().map(Into::into))
                                .unwrap_or_else(|| "descarga".into());
                            *destination = destino_libre(&carpeta, &nombre);
                        }
                        DownloadEvent::Finished {
                            path: Some(ruta),
                            success: true,
                            ..
                        } => {
                            let app = app_descargas.clone();
                            let _ = app_descargas.run_on_main_thread(move || {
                                notificaciones::mostrar_descarga(&app, ruta)
                            });
                        }
                        DownloadEvent::Finished { url, .. } => {
                            eprintln!("lumina: falló la descarga de {url}")
                        }
                        _ => {}
                    }
                    true
                }),
                LogicalPosition::new(0.0, ALTO_BARRA),
                LogicalSize::new(tam.width, tam.height - ALTO_BARRA),
            )?;
            barra::crear(&ventana)?;
            bloqueo::crear(&ventana)?;
            if !preferencias_migradas {
                let lector = ventana.add_child(
                    WebviewBuilder::new("barra", WebviewUrl::App("migrar.html".into())),
                    LogicalPosition::new(0.0, 0.0),
                    LogicalSize::new(1.0, 1.0),
                )?;
                lector.hide()?;
            }

            let app_handle = app.handle().clone();
            let wa = whatsapp.clone();
            whatsapp.with_webview(move |w| {
                webkit::reducir_cache(&w.inner());
                webkit::integrar(app_handle, wa, &w.inner());
            })?;
            aplicar_estilos(app.handle());
            notificaciones::escuchar(app.handle().clone());

            let ventana_eventos = ventana.clone();
            ventana.on_window_event(move |evento| match evento {
                // Cerrar no sale: como cualquier mensajería, sigue recibiendo
                // avisos en segundo plano. Salir está en el menú de la barra.
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = ventana_eventos.hide();
                }
                // El plazo del bloqueo cuenta desde que se deja la ventana.
                WindowEvent::Focused(false) => bloqueo::al_desenfocar(ventana_eventos.app_handle()),
                // Maximizar quita las esquinas redondeadas de abajo.
                WindowEvent::Resized(_) => {
                    let ahora = ventana_eventos.is_maximized().unwrap_or(false);
                    if MAXIMIZADA.swap(ahora, Ordering::Relaxed) != ahora {
                        aplicar_estilos(ventana_eventos.app_handle());
                        barra::actualizar(ventana_eventos.app_handle());
                    }
                }
                // Ajustes puede haber cambiado el fondo, la paleta o el tema
                // mientras la ventana estaba detrás. Releer al volver cuesta un
                // `gdbus` y un fichero, y ahorra vigilar nada en segundo plano.
                WindowEvent::Focused(true) => {
                    aplicar_tema(&ventana_eventos);
                    aplicar_estilos(ventana_eventos.app_handle());
                    barra::actualizar(ventana_eventos.app_handle());
                    bloqueo::al_enfocar(ventana_eventos.app_handle());
                }
                _ => {}
            });

            #[cfg(debug_assertions)]
            captura::programar(&ventana);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error al arrancar Lumina");
}
