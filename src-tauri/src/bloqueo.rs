//! Bloqueo de la app, como el de las apps de Android: una pantalla que tapa
//! WhatsApp hasta que se pone el PIN propio, la contraseña del sistema o la
//! huella.
//!
//! No cifra nada: los datos de WebKit siguen en claro en el disco. Protege
//! de quien use el ordenador con la sesión abierta, no de quien lea el disco.
//!
//! La pantalla es nativa (GTK con el CSS de la paleta, como la barra): en
//! HTML sería otro proceso web entero solo para un teclado numérico.
//!
//! El plazo no usa temporizadores: se apunta cuándo se perdió el foco y se
//! mira al volver. Con la ventana en segundo plano no se despierta nada.

use std::cell::{Cell, RefCell};
use std::io::Read;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use gtk::prelude::*;
use tauri::{AppHandle, Manager, Window};

use crate::autenticar::{self, Veredicto};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Metodo {
    Ninguno,
    Sistema,
    Pin,
}

pub struct Ajustes {
    pub metodo: Metodo,
    /// Cadena PHC de Argon2 (lleva la sal y los parámetros).
    pin: Option<String>,
    pub huella: bool,
    /// Segundos fuera de la ventana antes de bloquear; 0 bloquea al salir.
    pub plazo: u64,
}

static AJUSTES: Mutex<Ajustes> = Mutex::new(Ajustes {
    metodo: Metodo::Ninguno,
    pin: None,
    huella: true,
    plazo: 300,
});

pub const PLAZOS: [(u64, &str); 4] = [
    (0, "Al salir de Lumina"),
    (60, "Tras 1 minuto"),
    (300, "Tras 5 minutos"),
    (1800, "Tras 30 minutos"),
];

/// Fallos seguidos antes de hacer esperar, y cuánto, como en Android.
const FALLOS_ANTES_DE_ESPERAR: u32 = 5;
const ESPERA_S: u32 = 30;
const PIN_CIFRAS: std::ops::RangeInclusive<usize> = 4..=12;

pub fn ajustes() -> MutexGuard<'static, Ajustes> {
    AJUSTES.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn a_json() -> serde_json::Value {
    let a = ajustes();
    serde_json::json!({
        "metodo": match a.metodo {
            Metodo::Ninguno => "ninguno",
            Metodo::Sistema => "sistema",
            Metodo::Pin => "pin",
        },
        "pin": a.pin,
        "huella": a.huella,
        "plazo": a.plazo,
    })
}

/// Sin la clave (preferencias de antes del bloqueo) queda desactivado.
pub fn restaurar(datos: &serde_json::Value) {
    let mut a = ajustes();
    a.pin = datos["pin"].as_str().map(str::to_string);
    a.metodo = match (datos["metodo"].as_str(), &a.pin) {
        (None | Some("ninguno"), _) => Metodo::Ninguno,
        (Some("pin"), Some(_)) => Metodo::Pin,
        // Un PIN sin su hash, o un método desconocido: mejor pedir la
        // contraseña del sistema que dejar la app abierta.
        _ => Metodo::Sistema,
    };
    a.huella = datos["huella"].as_bool().unwrap_or(true);
    a.plazo = datos["plazo"]
        .as_u64()
        .filter(|p| PLAZOS.iter().any(|(s, _)| s == p))
        .unwrap_or(300);
}

fn pin_hash(pin: &[u8]) -> Option<String> {
    use argon2::password_hash::{PasswordHasher, SaltString};
    let mut sal = [0_u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut sal))
        .ok()?;
    let sal = SaltString::encode_b64(&sal).ok()?;
    argon2::Argon2::default()
        .hash_password(pin, &sal)
        .ok()
        .map(|h| h.to_string())
}

fn pin_vale(hash: &str, pin: &[u8]) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    PasswordHash::new(hash)
        .is_ok_and(|h| argon2::Argon2::default().verify_password(pin, &h).is_ok())
}

/// Lo que se quiere cambiar en el menú. Con el bloqueo puesto, antes hay que
/// demostrar que eres tú: si no, quitarlo estaría a un clic de cualquiera.
#[derive(Clone, Copy, PartialEq)]
pub enum Cambio {
    Metodo(Metodo),
    Huella(bool),
    Plazo(u64),
    CambiarPin,
}

#[derive(Clone, PartialEq)]
enum Pantalla {
    Oculta,
    Bloqueada,
    Confirmar(Cambio),
    PinNuevo,
    PinRepetir(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Huella {
    Parada,
    Escuchando,
    /// Sin lector o sin dedo registrado; no se vuelve a intentar en la sesión.
    SinLector,
}

struct Vista {
    raiz: gtk::EventBox,
    candado: gtk::Image,
    huella: gtk::Button,
    huella_icono: gtk::Image,
    titulo: gtk::Label,
    subtitulo: gtk::Label,
    entrada: gtk::Entry,
    error: gtk::Label,
    teclado: gtk::Grid,
    borrar_icono: gtk::Image,
    aceptar_icono: gtk::Image,
    aceptar: gtk::Button,
    cancelar: gtk::Button,
}

/// Colores de los iconos, que son SVG y no leen el CSS de GTK.
#[derive(Default)]
struct Colores {
    tx: String,
    tx2: String,
    blue: String,
    sobre_acento: String,
}

thread_local! {
    // Todo en el hilo de GTK: los widgets no son `Send`.
    static VISTA: RefCell<Option<Vista>> = const { RefCell::new(None) };
    static PANTALLA: RefCell<Pantalla> = const { RefCell::new(Pantalla::Oculta) };
    static HUELLA: Cell<Huella> = const { Cell::new(Huella::Parada) };
    static OCUPADO: Cell<bool> = const { Cell::new(false) };
    static FALLOS: Cell<u32> = const { Cell::new(0) };
    static ESPERANDO: Cell<bool> = const { Cell::new(false) };
    static DESDE: Cell<Option<Instant>> = const { Cell::new(None) };
    static COLORES: RefCell<Colores> = RefCell::new(Colores::default());
}

// Heroicons 2.1.5 outline, de `BookOS-HIG/heroicons/24/outline` (HIG §3.2).
const HUELLA_SVG: &str = "M7.864 4.243A7.5 7.5 0 0 1 19.5 10.5c0 2.92-.556 5.709-1.568 8.268M5.742 6.364A7.465 7.465 0 0 0 4.5 10.5a7.464 7.464 0 0 1-1.15 3.993m1.989 3.559A11.209 11.209 0 0 0 8.25 10.5a3.75 3.75 0 1 1 7.5 0c0 .527-.021 1.049-.064 1.565M12 10.5a14.94 14.94 0 0 1-3.6 9.75m6.633-4.596a18.666 18.666 0 0 1-2.485 5.33";
const CANDADO_SVG: &str = "M16.5 10.5V6.75a4.5 4.5 0 1 0-9 0v3.75m-.75 11.25h10.5a2.25 2.25 0 0 0 2.25-2.25v-6.75a2.25 2.25 0 0 0-2.25-2.25H6.75a2.25 2.25 0 0 0-2.25 2.25v6.75a2.25 2.25 0 0 0 2.25 2.25Z";
const BORRAR_SVG: &str = "M12 9.75 14.25 12m0 0 2.25 2.25M14.25 12l2.25-2.25M14.25 12 12 14.25m-2.58 4.92-6.374-6.375a1.125 1.125 0 0 1 0-1.59L9.42 4.83c.21-.211.497-.33.795-.33H19.5a2.25 2.25 0 0 1 2.25 2.25v10.5a2.25 2.25 0 0 1-2.25 2.25h-9.284c-.298 0-.585-.119-.795-.33Z";
const ACEPTAR_SVG: &str = "m4.5 12.75 6 6 9-13.5";

/// Un icono del sistema B a su tamaño en píxeles reales: un pixbuf a escala
/// lógica se vería borroso en una pantalla HiDPI.
fn pintar(imagen: &gtk::Image, trazo: &str, color: &str, tam: i32) {
    let escala = imagen.scale_factor();
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="{l}" height="{l}" fill="none" stroke="{color}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="{trazo}"/></svg>"#,
        l = tam * escala
    );
    let pixbuf = gtk::gdk_pixbuf::PixbufLoader::with_mime_type("image/svg+xml").and_then(|c| {
        c.write(svg.as_bytes())?;
        c.close()?;
        Ok(c.pixbuf())
    });
    match pixbuf {
        Ok(Some(p)) => {
            let superficie = p.create_surface(escala, imagen.window().as_ref());
            imagen.set_from_surface(superficie.as_ref());
        }
        Ok(None) => eprintln!("lumina: el icono SVG salió vacío"),
        Err(e) => eprintln!("lumina: no se pudo dibujar un icono: {e}"),
    }
}

fn pintar_iconos(v: &Vista) {
    COLORES.with_borrow(|c| {
        let huella = if HUELLA.get() == Huella::Escuchando {
            &c.blue
        } else {
            &c.tx2
        };
        pintar(&v.huella_icono, HUELLA_SVG, huella, 32);
        pintar(&v.candado, CANDADO_SVG, &c.tx2, 32);
        pintar(&v.borrar_icono, BORRAR_SVG, &c.tx, 24);
        // El color que la paleta pone encima del acento: con un acento
        // claro (melocotón) el blanco de la HIG apenas se leía.
        pintar(&v.aceptar_icono, ACEPTAR_SVG, &c.sobre_acento, 24);
    });
}

/// La barra llama aquí al cambiar de paleta, de tema o de maximizado.
pub fn recolorear(tx: &str, tx2: &str, blue: &str, sobre_acento: &str, maximizada: bool) {
    COLORES.with_borrow_mut(|c| {
        *c = Colores {
            tx: tx.to_string(),
            tx2: tx2.to_string(),
            blue: blue.to_string(),
            sobre_acento: sobre_acento.to_string(),
        }
    });
    VISTA.with_borrow(|v| {
        let Some(v) = v else { return };
        let contexto = v.raiz.style_context();
        if maximizada {
            contexto.add_class("maximizada");
        } else {
            contexto.remove_class("maximizada");
        }
        pintar_iconos(v);
    });
}

fn boton_tecla(contenido: &impl IsA<gtk::Widget>) -> gtk::Button {
    let boton = gtk::Button::new();
    boton.add(contenido);
    boton.set_size_request(56, 56);
    boton.set_can_focus(false);
    boton.style_context().add_class("tecla");
    boton
}

pub fn crear(ventana: &Window) -> tauri::Result<()> {
    let caja = ventana.default_vbox()?;
    let raiz = gtk::EventBox::new();
    raiz.set_widget_name("lumina-bloqueo");
    let desplazable = gtk::ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    desplazable.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    let columna = gtk::Box::new(gtk::Orientation::Vertical, 12);
    columna.set_halign(gtk::Align::Center);
    columna.set_valign(gtk::Align::Center);
    // Ancho de diálogo de la HIG (§2.4).
    columna.set_size_request(310, -1);
    columna.set_margin_top(24);
    columna.set_margin_bottom(24);

    let candado = gtk::Image::new();
    candado.set_size_request(64, 64);
    let huella_icono = gtk::Image::new();
    let huella = gtk::Button::new();
    huella.add(&huella_icono);
    huella.set_widget_name("lumina-huella");
    huella.set_size_request(64, 64);
    huella.set_halign(gtk::Align::Center);
    huella.set_tooltip_text(Some("Usar la huella"));
    let titulo = gtk::Label::new(None);
    titulo.set_widget_name("lumina-bloqueo-titulo");
    titulo.set_justify(gtk::Justification::Center);
    titulo.set_line_wrap(true);
    let subtitulo = gtk::Label::new(None);
    subtitulo.set_widget_name("lumina-bloqueo-subtitulo");
    subtitulo.set_justify(gtk::Justification::Center);
    subtitulo.set_line_wrap(true);
    let entrada = gtk::Entry::new();
    entrada.set_visibility(false);
    entrada.set_invisible_char(Some('●'));
    entrada.set_alignment(0.5);
    entrada.set_max_length(128);
    let error = gtk::Label::new(None);
    error.set_widget_name("lumina-bloqueo-error");
    error.set_justify(gtk::Justification::Center);
    error.set_line_wrap(true);

    let teclado = gtk::Grid::new();
    teclado.set_row_spacing(12);
    teclado.set_column_spacing(24);
    teclado.set_halign(gtk::Align::Center);
    teclado.set_margin_top(4);
    for (i, cifra) in ["1", "2", "3", "4", "5", "6", "7", "8", "9"]
        .into_iter()
        .enumerate()
    {
        teclado.attach(&tecla_cifra(&entrada, cifra), (i % 3) as i32, (i / 3) as i32, 1, 1);
    }
    let borrar_icono = gtk::Image::new();
    let borrar = boton_tecla(&borrar_icono);
    borrar.set_tooltip_text(Some("Borrar"));
    let campo = entrada.clone();
    borrar.connect_clicked(move |_| {
        let fin = campo.text_length() as i32;
        if fin > 0 {
            campo.delete_text(fin - 1, fin);
        }
    });
    let aceptar_icono = gtk::Image::new();
    let aceptar_tecla = boton_tecla(&aceptar_icono);
    aceptar_tecla.style_context().add_class("aceptar");
    aceptar_tecla.set_tooltip_text(Some("Aceptar"));
    let campo = entrada.clone();
    aceptar_tecla.connect_clicked(move |_| campo.emit_activate());
    teclado.attach(&borrar, 0, 3, 1, 1);
    teclado.attach(&tecla_cifra(&entrada, "0"), 1, 3, 1, 1);
    teclado.attach(&aceptar_tecla, 2, 3, 1, 1);

    let aceptar = gtk::Button::with_label("Desbloquear");
    aceptar.style_context().add_class("primario");
    let cancelar = gtk::Button::with_label("Cancelar");
    cancelar.style_context().add_class("secundario");
    let botones = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    botones.set_homogeneous(true);
    botones.pack_start(&cancelar, true, true, 0);
    botones.pack_start(&aceptar, true, true, 0);

    for widget in [
        candado.upcast_ref::<gtk::Widget>(),
        huella.upcast_ref(),
        titulo.upcast_ref(),
        subtitulo.upcast_ref(),
        entrada.upcast_ref(),
        error.upcast_ref(),
        teclado.upcast_ref(),
        botones.upcast_ref(),
    ] {
        columna.add(widget);
    }
    candado.set_halign(gtk::Align::Center);
    desplazable.add(&columna);
    raiz.add(&desplazable);
    caja.pack_start(&raiz, true, true, 0);
    raiz.show_all();
    // tao llama a `show_all()` al mostrar la ventana: sin esto, cerrar y
    // volver a abrir Lumina enseñaría la pantalla de bloqueo con la app
    // abierta. La webview de WhatsApp lleva la misma marca (`tapar`).
    raiz.set_no_show_all(true);
    raiz.hide();

    let app = ventana.app_handle().clone();
    entrada.connect_insert_text(|campo, texto, _| {
        // Con teclado numérico solo entran cifras, también desde el teclado
        // físico o pegando.
        if campo.input_purpose() == gtk::InputPurpose::Pin
            && !texto.chars().all(|c| c.is_ascii_digit())
        {
            campo.stop_signal_emission_by_name("insert-text");
        }
    });
    let a = app.clone();
    entrada.connect_activate(move |_| enviar(&a));
    let a = app.clone();
    aceptar.connect_clicked(move |_| enviar(&a));
    let a = app.clone();
    cancelar.connect_clicked(move |_| cancelar_cambio(&a));
    let a = app.clone();
    huella.connect_clicked(move |_| escuchar_huella(&a));
    let a = app.clone();
    raiz.connect_key_press_event(move |_, tecla| {
        if tecla.keyval() == gtk::gdk::keys::constants::Escape {
            cancelar_cambio(&a);
        }
        gtk::glib::Propagation::Proceed
    });
    raiz.connect_scale_factor_notify(|_| VISTA.with_borrow(|v| v.iter().for_each(pintar_iconos)));

    VISTA.with_borrow_mut(|v| {
        *v = Some(Vista {
            raiz,
            candado,
            huella,
            huella_icono,
            titulo,
            subtitulo,
            entrada,
            error,
            teclado,
            borrar_icono,
            aceptar_icono,
            aceptar,
            cancelar,
        })
    });

    // Con WhatsApp aún sin pintar: si se tapa después, se ve un instante.
    tapar(&app, false);
    if ajustes().metodo != Metodo::Ninguno {
        mostrar(&app, Pantalla::Bloqueada);
    } else {
        tapar(&app, true);
    }
    Ok(())
}

fn tecla_cifra(entrada: &gtk::Entry, cifra: &'static str) -> gtk::Button {
    let etiqueta = gtk::Label::new(Some(cifra));
    let boton = boton_tecla(&etiqueta);
    let campo = entrada.clone();
    boton.connect_clicked(move |_| {
        let mut fin = campo.text_length() as i32;
        campo.insert_text(cifra, &mut fin);
        campo.set_position(fin);
    });
    boton
}

/// Enseña u oculta WhatsApp. `no_show_all` para que el `show_all()` de tao
/// no la vuelva a enseñar por su cuenta.
fn tapar(app: &AppHandle, visible: bool) {
    let Some(whatsapp) = app.get_webview("whatsapp") else {
        return;
    };
    if let Err(e) = whatsapp.with_webview(move |w| {
        let vista = w.inner();
        vista.set_no_show_all(true);
        vista.set_visible(visible);
    }) {
        eprintln!("lumina: no se pudo tapar WhatsApp: {e}");
    }
}

pub fn bloqueada() -> bool {
    PANTALLA.with_borrow(|p| *p != Pantalla::Oculta)
}

pub fn bloquear(app: &AppHandle) {
    if ajustes().metodo == Metodo::Ninguno {
        return;
    }
    if PANTALLA.with_borrow(|p| *p != Pantalla::Bloqueada) {
        mostrar(app, Pantalla::Bloqueada);
    }
}

pub fn al_desenfocar(app: &AppHandle) {
    DESDE.set(Some(Instant::now()));
    if ajustes().plazo == 0 {
        bloquear(app);
    }
}

pub fn al_enfocar(app: &AppHandle) {
    let plazo = ajustes().plazo;
    if DESDE
        .take()
        .is_some_and(|desde| desde.elapsed().as_secs() >= plazo)
    {
        bloquear(app);
    }
    escuchar_huella(app);
}

fn mostrar(app: &AppHandle, pantalla: Pantalla) {
    PANTALLA.replace(pantalla);
    tapar(app, false);
    VISTA.with_borrow(|v| {
        let Some(v) = v else { return };
        v.raiz.show();
        v.entrada.set_text("");
    });
    avisar("");
    refrescar();
    VISTA.with_borrow(|v| v.iter().for_each(|v| v.entrada.grab_focus()));
    crate::barra::actualizar(app);
    escuchar_huella(app);
}

fn ocultar(app: &AppHandle) {
    PANTALLA.replace(Pantalla::Oculta);
    FALLOS.set(0);
    VISTA.with_borrow(|v| {
        if let Some(v) = v {
            v.entrada.set_text("");
            v.raiz.hide();
        }
    });
    tapar(app, true);
    crate::barra::actualizar(app);
}

/// Pone textos y visibilidad según la pantalla, el método y la huella.
fn refrescar() {
    let pantalla = PANTALLA.with_borrow(Clone::clone);
    let (metodo, con_huella) = {
        let a = ajustes();
        (a.metodo, a.huella)
    };
    let pin = match pantalla {
        Pantalla::PinNuevo | Pantalla::PinRepetir(_) => true,
        _ => metodo == Metodo::Pin,
    };
    let pide_identidad = matches!(pantalla, Pantalla::Bloqueada | Pantalla::Confirmar(_));
    let huella = pide_identidad
        && con_huella
        && autenticar::huella_instalada()
        && HUELLA.get() != Huella::SinLector;
    let o_huella = if huella { " o usa tu huella" } else { "" };
    let (titulo, subtitulo) = match &pantalla {
        Pantalla::Oculta => return,
        Pantalla::Bloqueada | Pantalla::Confirmar(_) => (
            if pantalla == Pantalla::Bloqueada {
                "Lumina está bloqueada"
            } else {
                "Confirma que eres tú"
            },
            if pin {
                format!("Introduce tu PIN{o_huella}")
            } else {
                format!(
                    "Introduce la contraseña de {}{o_huella}",
                    autenticar::usuario()
                )
            },
        ),
        Pantalla::PinNuevo => ("Elige un PIN", "De 4 a 12 cifras".to_string()),
        Pantalla::PinRepetir(_) => ("Repite el PIN", "Para comprobar que es el mismo".to_string()),
    };
    let libre = !OCUPADO.get() && !ESPERANDO.get();
    VISTA.with_borrow(|v| {
        let Some(v) = v else { return };
        v.titulo.set_text(titulo);
        v.subtitulo.set_text(&subtitulo);
        v.huella.set_visible(huella);
        v.candado.set_visible(!huella);
        v.entrada.set_input_purpose(if pin {
            gtk::InputPurpose::Pin
        } else {
            gtk::InputPurpose::Password
        });
        v.entrada.set_placeholder_text(Some(if pin { "PIN" } else { "Contraseña" }));
        v.entrada.set_max_length(if pin { *PIN_CIFRAS.end() as i32 } else { 128 });
        v.entrada.set_sensitive(libre);
        v.teclado.set_visible(pin);
        v.teclado.set_sensitive(libre);
        // Sin teclado numérico hace falta un botón; con él, la tecla ✓.
        v.aceptar.set_visible(!pin);
        v.aceptar.set_sensitive(libre);
        v.aceptar.set_label(if pantalla == Pantalla::Bloqueada {
            "Desbloquear"
        } else {
            "Aceptar"
        });
        v.cancelar.set_visible(pantalla != Pantalla::Bloqueada);
        v.cancelar
            .parent()
            .iter()
            .for_each(|b| b.set_visible(v.aceptar.is_visible() || v.cancelar.is_visible()));
        pintar_iconos(v);
    });
}

fn avisar(texto: &str) {
    VISTA.with_borrow(|v| {
        v.iter().for_each(|v| {
            v.error.set_text(texto);
            // Vacío ocupaba una línea entre el campo y los botones.
            v.error.set_visible(!texto.is_empty());
        })
    });
}

fn enviar(app: &AppHandle) {
    if OCUPADO.get() || ESPERANDO.get() {
        return;
    }
    let Some(texto) = VISTA.with_borrow(|v| {
        v.as_ref().map(|v| {
            let texto = v.entrada.text().to_string();
            v.entrada.set_text("");
            texto
        })
    }) else {
        return;
    };
    let pantalla = PANTALLA.with_borrow(Clone::clone);
    match pantalla {
        Pantalla::Oculta => {}
        Pantalla::PinNuevo => {
            if PIN_CIFRAS.contains(&texto.len()) {
                PANTALLA.replace(Pantalla::PinRepetir(texto));
                avisar("");
                refrescar();
            } else {
                avisar("El PIN tiene que tener de 4 a 12 cifras");
            }
        }
        Pantalla::PinRepetir(primero) => {
            let mut primero = primero.into_bytes();
            let igual = primero == texto.as_bytes();
            autenticar::borrar(&mut primero);
            if igual {
                guardar_pin(app, texto.into_bytes());
            } else {
                PANTALLA.replace(Pantalla::PinNuevo);
                refrescar();
                avisar("Los PIN no coinciden. Vuelve a empezar");
            }
        }
        Pantalla::Bloqueada | Pantalla::Confirmar(_) => {
            if !texto.is_empty() {
                comprobar(app, texto.into_bytes());
            }
        }
    }
}

/// Argon2 y PAM tardan a propósito: en otro hilo para no congelar la ventana.
fn comprobar(app: &AppHandle, mut secreto: Vec<u8>) {
    OCUPADO.set(true);
    avisar("");
    refrescar();
    let (metodo, hash) = {
        let a = ajustes();
        (a.metodo, a.pin.clone())
    };
    let app = app.clone();
    std::thread::spawn(move || {
        let bien = match metodo {
            Metodo::Sistema => {
                autenticar::contrasena(&autenticar::usuario(), &secreto) == Veredicto::Correcto
            }
            Metodo::Pin => hash.as_deref().is_some_and(|h| pin_vale(h, &secreto)),
            // No se llega aquí: sin método no hay pantalla. Si se llegara,
            // mejor no abrir.
            Metodo::Ninguno => false,
        };
        autenticar::borrar(&mut secreto);
        let a = app.clone();
        let _ = app.run_on_main_thread(move || {
            OCUPADO.set(false);
            if bien {
                superado(&a);
            } else {
                fallo(if metodo == Metodo::Pin {
                    "PIN incorrecto"
                } else {
                    "Contraseña incorrecta"
                });
            }
        });
    });
}

fn fallo(texto: &str) {
    let fallos = FALLOS.get() + 1;
    FALLOS.set(fallos);
    if fallos.is_multiple_of(FALLOS_ANTES_DE_ESPERAR) {
        ESPERANDO.set(true);
        avisar(&format!("Demasiados intentos. Espera {ESPERA_S} s"));
        gtk::glib::timeout_add_seconds_local_once(ESPERA_S, || {
            ESPERANDO.set(false);
            avisar("");
            refrescar();
        });
    } else {
        avisar(texto);
    }
    refrescar();
    VISTA.with_borrow(|v| v.iter().for_each(|v| v.entrada.grab_focus()));
}

fn superado(app: &AppHandle) {
    FALLOS.set(0);
    match PANTALLA.with_borrow(Clone::clone) {
        Pantalla::Bloqueada => ocultar(app),
        Pantalla::Confirmar(cambio) => aplicar(app, cambio),
        _ => {}
    }
}

fn guardar_pin(app: &AppHandle, mut pin: Vec<u8>) {
    OCUPADO.set(true);
    refrescar();
    let app = app.clone();
    std::thread::spawn(move || {
        let hash = pin_hash(&pin);
        autenticar::borrar(&mut pin);
        let a = app.clone();
        let _ = app.run_on_main_thread(move || {
            OCUPADO.set(false);
            let Some(hash) = hash else {
                refrescar();
                return avisar("No se pudo guardar el PIN");
            };
            {
                let mut ajustes = ajustes();
                ajustes.metodo = Metodo::Pin;
                ajustes.pin = Some(hash);
            }
            persistir(&a);
            ocultar(&a);
        });
    });
}

fn persistir(app: &AppHandle) {
    if let Err(e) = crate::barra::guardar(app) {
        eprintln!("lumina: no se pudo guardar el bloqueo: {e}");
    }
}

/// Lo que llega del menú de la barra.
pub fn pedir_cambio(app: &AppHandle, cambio: Cambio) {
    if ajustes().metodo == Metodo::Ninguno {
        aplicar(app, cambio);
    } else {
        mostrar(app, Pantalla::Confirmar(cambio));
    }
}

fn aplicar(app: &AppHandle, cambio: Cambio) {
    {
        let mut a = ajustes();
        match cambio {
            Cambio::Metodo(Metodo::Pin) | Cambio::CambiarPin => {
                drop(a);
                return mostrar(app, Pantalla::PinNuevo);
            }
            Cambio::Metodo(metodo) => {
                a.metodo = metodo;
                a.pin = None;
            }
            Cambio::Huella(huella) => a.huella = huella,
            Cambio::Plazo(plazo) => a.plazo = plazo,
        }
    }
    persistir(app);
    ocultar(app);
}

/// Solo se cancela un cambio de ajustes; la app bloqueada no tiene salida.
fn cancelar_cambio(app: &AppHandle) {
    if PANTALLA.with_borrow(|p| !matches!(p, Pantalla::Bloqueada | Pantalla::Oculta)) {
        ocultar(app);
    }
}

/// Como en Android, la huella se ofrece sola al aparecer el bloqueo. Solo
/// con la ventana enfocada: el lector es uno y el bloqueo del escritorio
/// también lo quiere.
fn escuchar_huella(app: &AppHandle) {
    let activa = {
        let a = ajustes();
        a.huella && a.metodo != Metodo::Ninguno
    };
    let pide = PANTALLA.with_borrow(|p| matches!(p, Pantalla::Bloqueada | Pantalla::Confirmar(_)));
    let enfocada = app
        .get_window("main")
        .and_then(|v| v.is_focused().ok())
        .unwrap_or(false);
    if !activa
        || !pide
        || !enfocada
        || HUELLA.get() != Huella::Parada
        || !autenticar::huella_instalada()
    {
        return;
    }
    HUELLA.set(Huella::Escuchando);
    avisar("");
    refrescar();
    let usuario = autenticar::usuario();
    let app = app.clone();
    std::thread::spawn(move || {
        let aviso = app.clone();
        let veredicto = autenticar::huella(&usuario, &move || {
            let _ = aviso.run_on_main_thread(|| avisar("Huella no reconocida"));
        });
        let a = app.clone();
        let _ = app.run_on_main_thread(move || {
            HUELLA.set(if veredicto == Veredicto::NoDisponible {
                Huella::SinLector
            } else {
                Huella::Parada
            });
            let pide =
                PANTALLA.with_borrow(|p| matches!(p, Pantalla::Bloqueada | Pantalla::Confirmar(_)));
            match veredicto {
                Veredicto::Correcto if pide => superado(&a),
                Veredicto::Rechazado if pide => {
                    avisar("Huella no reconocida. Toca la huella para volver a intentarlo")
                }
                _ => {}
            }
            refrescar();
        });
    });
}

#[cfg(debug_assertions)]
pub fn probar(app: &AppHandle, como: &str) {
    match como {
        "nuevo" => mostrar(app, Pantalla::PinNuevo),
        "pin" | "sistema" => {
            {
                let mut a = ajustes();
                a.metodo = if como == "pin" {
                    Metodo::Pin
                } else {
                    Metodo::Sistema
                };
            }
            mostrar(app, Pantalla::Bloqueada);
        }
        _ => eprintln!("lumina: LUMINA_CAPTURA_BLOQUEO={como} no existe"),
    }
}

#[cfg(debug_assertions)]
pub fn capturar(ruta: &std::path::Path) {
    VISTA.with_borrow(|v| {
        let Some(v) = v else { return };
        if !v.raiz.is_visible() {
            return;
        }
        let resultado = (|| -> Result<(), String> {
            let escala = v.raiz.scale_factor();
            let superficie = gtk::cairo::ImageSurface::create(
                gtk::cairo::Format::ARgb32,
                v.raiz.allocated_width() * escala,
                v.raiz.allocated_height() * escala,
            )
            .map_err(|e| e.to_string())?;
            superficie.set_device_scale(escala as f64, escala as f64);
            let contexto = gtk::cairo::Context::new(&superficie).map_err(|e| e.to_string())?;
            v.raiz.draw(&contexto);
            drop(contexto);
            let imagen = gtk::gdk::pixbuf_get_from_surface(
                &superficie,
                0,
                0,
                superficie.width(),
                superficie.height(),
            )
            .ok_or("captura vacía")?;
            imagen
                .savev(ruta.with_extension("bloqueo.png"), "png", &[])
                .map_err(|e| e.to_string())
        })();
        if let Err(e) = resultado {
            eprintln!("lumina: no se pudo capturar el bloqueo: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_pin_se_comprueba_contra_su_hash_y_no_contra_otro() {
        let hash = pin_hash(b"2468").expect("se puede calcular el hash");
        assert!(hash.starts_with("$argon2id$"), "hash inesperado: {hash}");
        assert!(pin_vale(&hash, b"2468"));
        assert!(!pin_vale(&hash, b"2469"));
        assert!(!pin_vale("no-es-un-hash", b"2468"));
    }

    #[test]
    fn dos_hashes_del_mismo_pin_llevan_sal_distinta() {
        assert_ne!(pin_hash(b"1234"), pin_hash(b"1234"));
    }

    #[test]
    fn unas_preferencias_raras_no_dejan_la_app_abierta() {
        restaurar(&serde_json::json!({"metodo": "pin"}));
        assert!(ajustes().metodo == Metodo::Sistema);
        restaurar(&serde_json::json!({"metodo": "inventado", "plazo": 7}));
        assert!(ajustes().metodo == Metodo::Sistema);
        assert_eq!(ajustes().plazo, 300);
        restaurar(&serde_json::json!(null));
        assert!(ajustes().metodo == Metodo::Ninguno);
    }
}
