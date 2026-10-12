//! Los avisos de Lumina, como toasts de BookOS.
//!
//! WebKitGTK entrega cada `new Notification()` de WhatsApp por la señal
//! `show-notification`, y aquí se reenvía a `org.freedesktop.Notifications`,
//! que en una sesión BookOS sirve el propio compositor. El dato va de WebKit a
//! Rust por GObject: la página remota no necesita ningún puente de IPC.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use futures_util::StreamExt;
use tauri::{AppHandle, Manager};
use webkit2gtk::NotificationExt;

const DESTINO: &str = "org.freedesktop.Notifications";
const RUTA: &str = "/org/freedesktop/Notifications";
const INTERFAZ: &str = "org.freedesktop.Notifications";

static CONEXION: tokio::sync::OnceCell<zbus::Connection> = tokio::sync::OnceCell::const_new();

async fn proxy() -> zbus::Result<zbus::Proxy<'static>> {
    let conexion = CONEXION.get_or_try_init(zbus::Connection::session).await?;
    zbus::Proxy::new(conexion, DESTINO, RUTA, INTERFAZ).await
}

/// Qué hacer cuando se pulsa un aviso.
enum Accion {
    /// Un mensaje: WhatsApp abre su chat cuando WebKit le dice `clicked()`.
    Mensaje(webkit2gtk::Notification),
    /// Una descarga terminada: se abre el fichero.
    Archivo(PathBuf),
}

#[derive(Default)]
struct Estado {
    /// Por id de D-Bus, los avisos que siguen en pantalla o en el historial.
    vivas: HashMap<u32, Accion>,
    /// Mensajes cuyo `Notify` aún no ha contestado, por id de WebKit.
    pendientes: HashMap<u64, webkit2gtk::Notification>,
}

thread_local! {
    // Solo se toca desde el hilo de GTK: los objetos de WebKit no son `Send`,
    // así que todo lo que llega del bus vuelve aquí con `run_on_main_thread`.
    static ESTADO: RefCell<Estado> = RefCell::new(Estado::default());
}

async fn notify(
    resumen: &str,
    cuerpo: &str,
    sustituye: u32,
    categoria: &str,
    responder: bool,
) -> zbus::Result<u32> {
    let mut pistas: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
    pistas.insert("desktop-entry", "bookos-lumina".into());
    pistas.insert("category", categoria.into());
    // «default» no es un botón en BookOS: es pulsar el propio toast.
    // `inline-reply` sí: BookOS lo dibuja como «Responder» y pide el texto en
    // una caja propia; la respuesta vuelve por `NotificationReplied`.
    let mut acciones = vec!["default", "Abrir"];
    if responder {
        acciones.extend(["inline-reply", "Responder"]);
    }
    proxy()
        .await?
        .call(
            "Notify",
            &(
                "Lumina",
                sustituye,
                "bookos-lumina",
                resumen,
                cuerpo,
                &acciones[..],
                pistas,
                -1i32,
            ),
        )
        .await
}

/// Publica un mensaje de WhatsApp. Hilo de GTK.
pub fn mostrar_mensaje(app: &AppHandle, aviso: &webkit2gtk::Notification) {
    // Con la app bloqueada el aviso no dice de quién ni qué, y no se puede
    // responder: si no, el bloqueo se saltaría desde el propio aviso.
    let privado = crate::bloqueo::bloqueada();
    let (resumen, cuerpo) = if privado {
        ("Lumina".to_string(), "Mensaje nuevo".to_string())
    } else {
        (
            aviso.title().map(|t| t.to_string()).unwrap_or_default(),
            aviso.body().map(|t| t.to_string()).unwrap_or_default(),
        )
    };
    // WhatsApp etiqueta los avisos por chat. Reutilizar el id hace que el
    // segundo mensaje de un chat sustituya al primero en vez de apilar otro.
    let etiqueta = aviso.tag();
    let sustituye = ESTADO.with_borrow_mut(|e| {
        e.pendientes.insert(aviso.id(), aviso.clone());
        etiqueta
            .as_ref()
            .and_then(|t| {
                e.vivas.iter().find_map(|(id, a)| match a {
                    Accion::Mensaje(n) if n.tag().as_ref() == Some(t) => Some(*id),
                    _ => None,
                })
            })
            .unwrap_or(0)
    });

    // La página puede retirar el aviso, por ejemplo al leer el chat en el móvil.
    let clave = aviso.id();
    aviso.connect_closed(move |_| {
        let id = ESTADO.with_borrow_mut(|e| {
            e.pendientes.remove(&clave);
            let id = e.vivas.iter().find_map(|(id, a)| match a {
                Accion::Mensaje(n) if n.id() == clave => Some(*id),
                _ => None,
            });
            id.inspect(|id| {
                e.vivas.remove(id);
            })
        });
        if let Some(id) = id {
            tauri::async_runtime::spawn(async move {
                if let Err(e) = async {
                    proxy()
                        .await?
                        .call::<_, _, ()>("CloseNotification", &(id,))
                        .await
                }
                .await
                {
                    eprintln!("lumina: no se pudo retirar el aviso {id}: {e}");
                }
            });
        }
    });

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match notify(&resumen, &cuerpo, sustituye, "im.received", !privado).await {
            Ok(id) => {
                let _ = app.run_on_main_thread(move || {
                    ESTADO.with_borrow_mut(|e| {
                        if let Some(n) = e.pendientes.remove(&clave) {
                            e.vivas.insert(id, Accion::Mensaje(n));
                        }
                    })
                });
            }
            Err(e) => eprintln!("lumina: el servidor de notificaciones no contestó: {e}"),
        }
    });
}

/// Avisa de una descarga terminada. Hilo de GTK.
pub fn mostrar_descarga(app: &AppHandle, ruta: PathBuf) {
    let nombre = ruta
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match notify("Descarga completada", &nombre, 0, "transfer.complete", false).await {
            Ok(id) => {
                let _ = app.run_on_main_thread(move || {
                    ESTADO.with_borrow_mut(|e| e.vivas.insert(id, Accion::Archivo(ruta)));
                });
            }
            Err(e) => eprintln!("lumina: el servidor de notificaciones no contestó: {e}"),
        }
    });
}

/// Lo que el servidor de notificaciones le cuenta a Lumina.
enum Senal {
    /// Se pulsó un botón del aviso, o el propio aviso con `default`.
    Accion(u32, String),
    Cerrada(u32),
    /// Se escribió una respuesta en la caja de «Responder».
    Respuesta(u32, String),
}

/// Envía `texto` al chat del aviso. Hilo de GTK.
///
/// Abre el chat con el mismo `clicked()` que usa pulsar el aviso, pero **sin**
/// enseñar la ventana: quien responde desde el aviso no quiere que Lumina le
/// salte encima. Escribir y enviar lo hace `pagina.js`, que comprueba que el
/// chat abierto es el del aviso antes de teclear nada.
fn responder(app: &AppHandle, aviso: &webkit2gtk::Notification, texto: &str) {
    let Some(whatsapp) = app.get_webview("whatsapp") else {
        return eprintln!("lumina: no hay webview de WhatsApp para responder");
    };
    let titulo = aviso.title().map(|t| t.to_string()).unwrap_or_default();
    aviso.clicked();
    // El dato va de Rust a la página y nunca al revés, como todo en Lumina.
    let (Ok(titulo), Ok(texto)) = (serde_json::to_string(&titulo), serde_json::to_string(texto))
    else {
        return;
    };
    if let Err(e) = whatsapp.eval(format!(
        "window.__luminaResponder && window.__luminaResponder({titulo}, {texto})"
    )) {
        eprintln!("lumina: no se pudo entregar la respuesta a la página: {e}");
    }
}

/// Escucha las pulsaciones, respuestas y cierres de los avisos durante toda la
/// vida de la app.
pub fn escuchar(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let proxy = match proxy().await {
            Ok(p) => p,
            Err(e) => return eprintln!("lumina: sin bus de sesión, no habrá avisos: {e}"),
        };
        // Un solo flujo y no uno por señal: «respondida» y «cerrada» llegan
        // seguidas y con flujos separados nada garantiza el orden, y si el
        // cierre se atiende primero la respuesta ya no encuentra su aviso.
        let Ok(senales) = proxy.receive_all_signals().await else {
            return eprintln!("lumina: no se pudo escuchar al servidor de notificaciones");
        };
        let mut senales = std::pin::pin!(senales.filter_map(|m| async move {
            let cuerpo = m.body();
            match m.header().member()?.as_str() {
                "ActionInvoked" => cuerpo
                    .deserialize::<(u32, String)>()
                    .ok()
                    .map(|(id, a)| Senal::Accion(id, a)),
                "NotificationClosed" => cuerpo
                    .deserialize::<(u32, u32)>()
                    .ok()
                    .map(|(id, _)| Senal::Cerrada(id)),
                "NotificationReplied" => cuerpo
                    .deserialize::<(u32, String)>()
                    .ok()
                    .map(|(id, t)| Senal::Respuesta(id, t)),
                _ => None,
            }
        }));
        while let Some(senal) = senales.next().await {
            let app2 = app.clone();
            let _ = app.run_on_main_thread(move || match senal {
                Senal::Respuesta(id, texto) => {
                    // Un aviso de antes de bloquear aún tiene «Responder».
                    if crate::bloqueo::bloqueada() {
                        return eprintln!("lumina: respuesta descartada, la app está bloqueada");
                    }
                    // Se queda en el mapa: el cierre que llega detrás es el que
                    // lo quita.
                    ESTADO.with_borrow(|e| {
                        if let Some(Accion::Mensaje(n)) = e.vivas.get(&id) {
                            responder(&app2, n, &texto);
                        }
                    });
                }
                Senal::Cerrada(id) | Senal::Accion(id, _) => {
                    let accion = match &senal {
                        Senal::Accion(_, a) => a.as_str(),
                        _ => "",
                    };
                    let Some(hecho) = ESTADO.with_borrow_mut(|e| e.vivas.remove(&id)) else {
                        return;
                    };
                    if accion != "default" {
                        return;
                    }
                    match hecho {
                        Accion::Mensaje(n) => {
                            crate::mostrar_ventana(&app2);
                            n.clicked();
                        }
                        Accion::Archivo(ruta) => crate::abrir_fuera(ruta.as_os_str()),
                    }
                }
            });
        }
    });
}
