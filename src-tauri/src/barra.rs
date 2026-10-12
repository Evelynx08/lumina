//! Controles nativos: una barra de 36 px no necesita otro proceso de navegador.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use gtk::prelude::*;
use tauri::{AppHandle, Manager, Theme, Window};

struct Barra {
    cabecera: gtk::EventBox,
    mas: gtk::MenuButton,
    #[cfg(debug_assertions)]
    menu: gtk::PopoverMenu,
    paleta: gtk::CheckButton,
    escritorio: gtk::RadioButton,
    imagen: gtk::RadioButton,
    ninguno: gtk::RadioButton,
    bloquear_ahora: gtk::Button,
    /// En el orden de `Metodo`: desactivado, contraseña del sistema, PIN.
    metodos: [gtk::RadioButton; 3],
    huella: gtk::CheckButton,
    cambiar_pin: gtk::Button,
    /// En el orden de `bloqueo::PLAZOS`.
    plazos: Vec<gtk::RadioButton>,
    proveedor: gtk::CssProvider,
}

thread_local! {
    static SINCRONIZANDO: Cell<bool> = const { Cell::new(false) };
    static BARRA: RefCell<Option<Barra>> = const { RefCell::new(None) };
}

fn ruta_preferencias(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|p| p.join("preferencias.json"))
        .map_err(|e| e.to_string())
}

pub fn cargar(app: &AppHandle) -> Result<bool, String> {
    let texto = match std::fs::read_to_string(ruta_preferencias(app)?) {
        Ok(texto) => texto,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    let datos: serde_json::Value = serde_json::from_str(&texto).map_err(|e| e.to_string())?;
    restaurar(&datos)?;
    Ok(true)
}

fn restaurar(datos: &serde_json::Value) -> Result<(), String> {
    let activa = datos["paleta"]
        .as_bool()
        .ok_or("falta la preferencia de color")?;
    let modo = datos["fondo"].as_str().ok_or("falta el modo del fondo")?;
    let imagen = datos["imagen"].as_str().map(PathBuf::from);
    let fondo = match (modo, imagen.as_ref()) {
        ("ninguno", _) => crate::fondo::Fondo::Ninguno,
        ("escritorio", _) | ("imagen", None) => crate::fondo::Fondo::Escritorio,
        ("imagen", Some(ruta)) => crate::fondo::Fondo::Imagen(ruta.clone()),
        _ => return Err(format!("fondo desconocido: {modo}")),
    };
    crate::bloqueo::restaurar(&datos["bloqueo"]);
    crate::PALETA_ACTIVA.store(activa, Ordering::Relaxed);
    *crate::FONDO.lock().unwrap_or_else(|e| e.into_inner()) = fondo;
    *crate::IMAGEN_GUARDADA
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = imagen;
    Ok(())
}

pub fn guardar(app: &AppHandle) -> Result<(), String> {
    let fondo = crate::FONDO.lock().unwrap_or_else(|e| e.into_inner());
    let modo = match &*fondo {
        crate::fondo::Fondo::Ninguno => "ninguno",
        crate::fondo::Fondo::Escritorio => "escritorio",
        crate::fondo::Fondo::Imagen(_) => "imagen",
    };
    let imagen = crate::IMAGEN_GUARDADA
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let datos = serde_json::json!({
        "paleta": crate::PALETA_ACTIVA.load(Ordering::Relaxed),
        "fondo": modo,
        "imagen": *imagen,
        "bloqueo": crate::bloqueo::a_json(),
    });
    let ruta = ruta_preferencias(app)?;
    std::fs::create_dir_all(ruta.parent().expect("preferencias dentro de una carpeta"))
        .map_err(|e| e.to_string())?;
    let temporal = ruta.with_extension("tmp");
    std::fs::write(&temporal, datos.to_string()).map_err(|e| e.to_string())?;
    // Lleva el hash del PIN: que no lo lea nadie más que el usuario.
    std::fs::set_permissions(
        &temporal,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(temporal, ruta).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn migrar_preferencias(app: AppHandle, datos: serde_json::Value) -> Result<(), String> {
    // Si los datos viejos no valen, `restaurar` no toca nada y se guardan los
    // de por defecto: sin fichero, cada arranque volvería a lanzar el lector
    // para fallar igual.
    let migrado = restaurar(&datos);
    let guardado = guardar(&app);
    actualizar(&app);
    crate::aplicar_estilos(&app);
    // El lector conserva el origen del localStorage antiguo, pero solo vive
    // durante la primera migración. No toca la sesión ni los datos de WhatsApp.
    // Es un proceso web entero, así que se cierra también si algo falló.
    if let Some(lector) = app.get_webview("barra") {
        lector.close().map_err(|e| e.to_string())?;
    }
    migrado.and(guardado)
}

fn registrar(resultado: Result<(), String>) {
    if let Err(e) = resultado {
        eprintln!("lumina: no se pudo cambiar la preferencia: {e}");
    }
}

fn cambiar_fondo(app: &AppHandle, modo: &str, ruta: Option<PathBuf>) {
    registrar(crate::fondo_chats(app.clone(), modo, ruta).and_then(|()| guardar(app)));
    actualizar(app);
}

/// Los controles de ventana de Ajustes de BookOS (`BookOS-Settings/src/index.html`):
/// raya, cuadrado redondeado y X a trazo 2. Los iconos del tema GTK no valen,
/// porque BookOS-Dark dibuja minimizar y maximizar como flechas.
#[derive(Clone, Copy)]
enum Icono {
    Mas,
    Minimizar,
    Maximizar,
    Cerrar,
}

/// Se pinta con cairo y no con un pixbuf: así el trazo usa el `color` del CSS
/// del botón y cambia solo al pasar el ratón (la X, blanca sobre el rojo).
fn icono(icono: Icono) -> gtk::DrawingArea {
    let lienzo = gtk::DrawingArea::new();
    lienzo.set_size_request(15, 15);
    // Sin centrar, el botón estira el lienzo a sus 32 × 28 y el icono salía
    // casi al doble que en Ajustes.
    lienzo.set_halign(gtk::Align::Center);
    lienzo.set_valign(gtk::Align::Center);
    lienzo.connect_draw(move |lienzo, cr| {
        let contexto = lienzo.style_context();
        let color = contexto.color(contexto.state());
        cr.set_source_rgba(color.red(), color.green(), color.blue(), color.alpha());
        // Coordenadas del viewBox de 24 de los SVG de Ajustes.
        let lado = f64::from(lienzo.allocated_width().min(lienzo.allocated_height()));
        cr.scale(lado / 24.0, lado / 24.0);
        cr.set_line_width(2.0);
        cr.set_line_cap(gtk::cairo::LineCap::Round);
        cr.set_line_join(gtk::cairo::LineJoin::Round);
        match icono {
            Icono::Mas => {
                for y in [5.25, 12.0, 18.75] {
                    cr.arc(12.0, y, 2.0, 0.0, std::f64::consts::TAU);
                    cr.close_path();
                }
                if let Err(e) = cr.fill() {
                    eprintln!("lumina: no se pudo dibujar un icono: {e}");
                }
                return gtk::glib::Propagation::Proceed;
            }
            Icono::Minimizar => {
                cr.move_to(5.0, 12.0);
                cr.line_to(19.0, 12.0);
            }
            Icono::Maximizar => {
                use std::f64::consts::{FRAC_PI_2, PI};
                let r = 2.5;
                cr.new_sub_path();
                cr.arc(19.0 - r, 5.0 + r, r, -FRAC_PI_2, 0.0);
                cr.arc(19.0 - r, 19.0 - r, r, 0.0, FRAC_PI_2);
                cr.arc(5.0 + r, 19.0 - r, r, FRAC_PI_2, PI);
                cr.arc(5.0 + r, 5.0 + r, r, PI, PI + FRAC_PI_2);
                cr.close_path();
            }
            Icono::Cerrar => {
                cr.move_to(6.0, 18.0);
                cr.line_to(18.0, 6.0);
                cr.move_to(6.0, 6.0);
                cr.line_to(18.0, 18.0);
            }
        }
        if let Err(e) = cr.stroke() {
            eprintln!("lumina: no se pudo dibujar un icono: {e}");
        }
        gtk::glib::Propagation::Proceed
    });
    lienzo
}

fn boton(dibujo: Icono, ayuda: &str) -> gtk::Button {
    let boton = gtk::Button::new();
    boton.add(&icono(dibujo));
    boton.set_tooltip_text(Some(ayuda));
    boton.set_size_request(32, 28);
    boton
}

pub fn crear(ventana: &Window) -> tauri::Result<()> {
    let caja = ventana.default_vbox()?;
    let cabecera = gtk::EventBox::new();
    cabecera.set_widget_name("lumina-barra");
    cabecera.set_size_request(-1, crate::ALTO_BARRA as i32);
    let fila = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    fila.set_margin_start(20);
    fila.set_margin_end(10);
    let titulo = gtk::Label::new(Some("Lumina"));
    titulo.set_halign(gtk::Align::Start);
    fila.pack_start(&titulo, true, true, 0);
    let mas = gtk::MenuButton::new();
    mas.set_image(Some(&icono(Icono::Mas)));
    mas.set_tooltip_text(Some("Más opciones"));
    mas.set_size_request(32, 28);
    let minimizar = boton(Icono::Minimizar, "Minimizar");
    let maximizar = boton(Icono::Maximizar, "Maximizar o restaurar");
    let cerrar = boton(Icono::Cerrar, "Cerrar (sigue recibiendo mensajes)");
    cerrar.set_widget_name("lumina-cerrar");
    for boton in [
        mas.clone().upcast::<gtk::Widget>(),
        minimizar.clone().upcast(),
        maximizar.clone().upcast(),
        cerrar.clone().upcast(),
    ] {
        boton.set_valign(gtk::Align::Center);
        fila.pack_start(&boton, false, false, 0);
    }
    cabecera.add(&fila);
    caja.pack_start(&cabecera, false, false, 0);
    caja.reorder_child(&cabecera, 0);
    cabecera.show_all();

    let v = ventana.clone();
    minimizar.connect_clicked(move |_| {
        if let Err(e) = v.minimize() {
            eprintln!("lumina: no se pudo minimizar: {e}");
        }
    });
    let v = ventana.clone();
    maximizar.connect_clicked(move |_| {
        if let Err(e) = v
            .is_maximized()
            .and_then(|m| if m { v.unmaximize() } else { v.maximize() })
        {
            eprintln!("lumina: no se pudo cambiar el tamaño: {e}");
        }
    });
    let v = ventana.clone();
    cerrar.connect_clicked(move |_| {
        if let Err(e) = v.close() {
            eprintln!("lumina: no se pudo ocultar la ventana: {e}");
        }
    });
    let v = ventana.clone();
    cabecera.connect_button_press_event(move |_, evento| {
        if evento.button() != 1 {
            return gtk::glib::Propagation::Proceed;
        }
        let resultado = if evento.event_type() == gtk::gdk::EventType::DoubleButtonPress {
            v.is_maximized()
                .and_then(|m| if m { v.unmaximize() } else { v.maximize() })
        } else {
            v.start_dragging()
        };
        if let Err(e) = resultado {
            eprintln!("lumina: no se pudo mover la ventana: {e}");
        }
        gtk::glib::Propagation::Stop
    });

    let menu = gtk::PopoverMenu::new();
    menu.set_relative_to(Some(&mas));
    menu.set_widget_name("lumina-menu");
    menu.set_position(gtk::PositionType::Bottom);
    mas.set_popover(Some(&menu));
    let opciones = gtk::Box::new(gtk::Orientation::Vertical, 2);
    opciones.set_border_width(6);
    let recargar = gtk::Button::with_label("Recargar WhatsApp");
    let paleta = gtk::CheckButton::with_label("Color dinámico");
    let escritorio = gtk::RadioButton::with_label("Fondo del escritorio");
    let imagen = gtk::RadioButton::with_label_from_widget(&escritorio, "Imagen propia…");
    let ninguno = gtk::RadioButton::with_label_from_widget(&escritorio, "Ninguno");
    let salir = gtk::Button::with_label("Salir de Lumina");
    let bloquear_ahora = gtk::Button::with_label("Bloquear ahora");
    let a_bloqueo = gtk::ModelButton::builder()
        .text("Bloqueo de la app")
        .menu_name("bloqueo")
        .build();
    opciones.add(&recargar);
    opciones.add(&paleta);
    opciones.add(&gtk::Separator::new(gtk::Orientation::Horizontal));
    opciones.add(&gtk::Label::new(Some("Fondo de los chats")));
    for opcion in [&escritorio, &imagen, &ninguno] {
        opciones.add(opcion);
    }
    opciones.add(&gtk::Separator::new(gtk::Orientation::Horizontal));
    opciones.add(&bloquear_ahora);
    opciones.add(&a_bloqueo);
    opciones.add(&gtk::Separator::new(gtk::Orientation::Horizontal));
    opciones.add(&salir);
    menu.add(&opciones);
    opciones.show_all();

    // Submenú, para no alargar el principal con siete opciones más.
    let bloqueo = gtk::Box::new(gtk::Orientation::Vertical, 2);
    bloqueo.set_border_width(6);
    let volver = gtk::ModelButton::builder()
        .text("Bloqueo de la app")
        .menu_name("main")
        .inverted(true)
        .centered(true)
        .build();
    let desactivado = gtk::RadioButton::with_label("Desactivado");
    let metodos = [
        desactivado.clone(),
        gtk::RadioButton::with_label_from_widget(&desactivado, "Contraseña del sistema"),
        gtk::RadioButton::with_label_from_widget(&desactivado, "PIN"),
    ];
    let huella = gtk::CheckButton::with_label("Usar la huella");
    let cambiar_pin = gtk::Button::with_label("Cambiar el PIN");
    let primero = gtk::RadioButton::with_label(crate::bloqueo::PLAZOS[0].1);
    let plazos: Vec<gtk::RadioButton> = crate::bloqueo::PLAZOS
        .iter()
        .enumerate()
        .map(|(i, (_, texto))| {
            if i == 0 {
                primero.clone()
            } else {
                gtk::RadioButton::with_label_from_widget(&primero, texto)
            }
        })
        .collect();
    bloqueo.add(&volver);
    bloqueo.add(&encabezado("Desbloquear con"));
    metodos.iter().for_each(|m| bloqueo.add(m));
    bloqueo.add(&huella);
    bloqueo.add(&cambiar_pin);
    bloqueo.add(&gtk::Separator::new(gtk::Orientation::Horizontal));
    bloqueo.add(&encabezado("Bloquear"));
    plazos.iter().for_each(|p| bloqueo.add(p));
    menu.add(&bloqueo);
    menu.child_set_property(&bloqueo, "submenu", &"bloqueo");
    bloqueo.show_all();

    // Todo cambio de bloqueo pasa por `bloqueo::pedir_cambio`, que pide la
    // identidad antes si el bloqueo está puesto. El menú se cierra porque la
    // pregunta sale en la pantalla de bloqueo, y si se cancela, `actualizar`
    // devuelve las casillas a lo que había.
    let pedir = {
        let app = ventana.app_handle().clone();
        let desplegable = menu.downgrade();
        move |cambio| {
            if SINCRONIZANDO.with(Cell::get) {
                return;
            }
            if let Some(menu) = desplegable.upgrade() {
                menu.popdown();
            }
            crate::bloqueo::pedir_cambio(&app, cambio);
            actualizar(&app);
        }
    };
    for (control, metodo) in metodos.iter().zip([
        crate::bloqueo::Metodo::Ninguno,
        crate::bloqueo::Metodo::Sistema,
        crate::bloqueo::Metodo::Pin,
    ]) {
        let pedir = pedir.clone();
        control.connect_toggled(move |c| {
            if c.is_active() {
                pedir(crate::bloqueo::Cambio::Metodo(metodo));
            }
        });
    }
    let p = pedir.clone();
    huella.connect_toggled(move |c| p(crate::bloqueo::Cambio::Huella(c.is_active())));
    let p = pedir.clone();
    cambiar_pin.connect_clicked(move |_| p(crate::bloqueo::Cambio::CambiarPin));
    for (control, (segundos, _)) in plazos.iter().zip(crate::bloqueo::PLAZOS) {
        let pedir = pedir.clone();
        control.connect_toggled(move |c| {
            if c.is_active() {
                pedir(crate::bloqueo::Cambio::Plazo(segundos));
            }
        });
    }
    let app = ventana.app_handle().clone();
    let desplegable = menu.downgrade();
    bloquear_ahora.connect_clicked(move |_| {
        if let Some(menu) = desplegable.upgrade() {
            menu.popdown();
        }
        crate::bloqueo::bloquear(&app);
    });

    let app = ventana.app_handle().clone();
    let desplegable = menu.downgrade();
    recargar.connect_clicked(move |_| {
        if let Some(menu) = desplegable.upgrade() {
            menu.popdown();
        }
        registrar(crate::recargar_whatsapp(app.clone()));
    });
    let app = ventana.app_handle().clone();
    paleta.connect_clicked(move |control| {
        if SINCRONIZANDO.with(Cell::get) {
            return;
        }
        crate::paleta_whatsapp(app.clone(), control.is_active());
        registrar(guardar(&app));
        actualizar(&app);
    });
    let app = ventana.app_handle().clone();
    escritorio.connect_clicked(move |_| {
        if !SINCRONIZANDO.with(Cell::get) {
            cambiar_fondo(&app, "escritorio", None);
        }
    });
    let app = ventana.app_handle().clone();
    ninguno.connect_clicked(move |_| {
        if !SINCRONIZANDO.with(Cell::get) {
            cambiar_fondo(&app, "ninguno", None);
        }
    });
    let app = ventana.app_handle().clone();
    let desplegable = menu.downgrade();
    imagen.connect_clicked(move |_| {
        if SINCRONIZANDO.with(Cell::get) {
            return;
        }
        if let Some(menu) = desplegable.upgrade() {
            menu.popdown();
        }
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let resultado = crate::fondo::elegir().await;
            let destino = app.clone();
            if let Err(e) = app.run_on_main_thread(move || match resultado {
                Ok(Some(ruta)) => cambiar_fondo(&destino, "imagen", Some(ruta)),
                Ok(None) => actualizar(&destino),
                Err(e) => {
                    eprintln!("lumina: no se pudo elegir el fondo: {e}");
                    actualizar(&destino);
                }
            }) {
                eprintln!("lumina: no se pudo actualizar el fondo elegido: {e}");
            }
        });
    });
    let app = ventana.app_handle().clone();
    salir.connect_clicked(move |_| app.exit(0));

    let proveedor = gtk::CssProvider::new();
    gtk::StyleContext::add_provider_for_screen(
        &cabecera
            .screen()
            .expect("barra en la pantalla de la ventana"),
        &proveedor,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    BARRA.with_borrow_mut(|estado| {
        *estado = Some(Barra {
            cabecera,
            mas,
            #[cfg(debug_assertions)]
            menu,
            paleta,
            escritorio,
            imagen,
            ninguno,
            bloquear_ahora,
            metodos,
            huella,
            cambiar_pin,
            plazos,
            proveedor,
        })
    });
    actualizar(ventana.app_handle());
    Ok(())
}

/// Encabezado de grupo (HIG §2.2: 12/Medium, `--tx2`).
fn encabezado(texto: &str) -> gtk::Label {
    let etiqueta = gtk::Label::new(Some(texto));
    etiqueta.set_halign(gtk::Align::Start);
    etiqueta.style_context().add_class("encabezado");
    etiqueta
}

fn color(paleta: &str, oscuro: bool, nombre: &str, reserva: &str) -> String {
    // Settings publica estos dos bloques; solo aceptamos colores válidos para
    // GTK, no copiamos selectores ni sintaxis de navegador a su hoja nativa.
    let bloque = if oscuro {
        ":root.dark-mode{"
    } else {
        ":root,:root.light-mode{"
    };
    paleta
        .split_once(bloque)
        .and_then(|(_, resto)| resto.split_once('}'))
        .and_then(|(reglas, _)| {
            reglas.split(';').find_map(|regla| {
                let (clave, valor) = regla.split_once(':')?;
                (clave.trim() == nombre).then_some(valor.trim())
            })
        })
        .and_then(|valor| gtk::gdk::RGBA::parse(valor).ok())
        .map(|c| c.to_string().to_string())
        .unwrap_or_else(|| reserva.to_string())
}

pub fn actualizar(app: &AppHandle) {
    let oscuro = app
        .get_window("main")
        .and_then(|v| v.theme().ok())
        .is_some_and(|t| t == Theme::Dark);
    let activa = crate::PALETA_ACTIVA.load(Ordering::Relaxed);
    let paleta = if activa {
        crate::paleta_css()
    } else {
        String::new()
    };
    let fondo = crate::FONDO
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let imagen_guardada = crate::IMAGEN_GUARDADA
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    BARRA.with_borrow(|estado| {
        let Some(barra) = estado else { return; };
        SINCRONIZANDO.with(|estado| estado.set(true));
        barra.paleta.set_active(activa);
        match fondo {
            crate::fondo::Fondo::Escritorio => barra.escritorio.set_active(true),
            crate::fondo::Fondo::Ninguno => barra.ninguno.set_active(true),
            crate::fondo::Fondo::Imagen(_) => barra.imagen.set_active(true),
        }
        barra.imagen.set_label(&imagen_guardada.as_deref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy()).unwrap_or("Imagen propia…".into()));
        {
            use crate::bloqueo::Metodo;
            let bloqueo = crate::bloqueo::ajustes();
            let puesto = bloqueo.metodo != Metodo::Ninguno;
            let i = match bloqueo.metodo { Metodo::Ninguno => 0, Metodo::Sistema => 1, Metodo::Pin => 2 };
            barra.metodos[i].set_active(true);
            barra.huella.set_active(bloqueo.huella);
            barra.huella.set_sensitive(puesto && crate::autenticar::huella_instalada());
            barra.cambiar_pin.set_visible(bloqueo.metodo == Metodo::Pin);
            barra.bloquear_ahora.set_sensitive(puesto);
            for (control, (segundos, _)) in barra.plazos.iter().zip(crate::bloqueo::PLAZOS) {
                control.set_sensitive(puesto);
                if segundos == bloqueo.plazo { control.set_active(true); }
            }
        }
        // Con WhatsApp tapado no hay nada que tocar desde el menú.
        barra.mas.set_sensitive(!crate::bloqueo::bloqueada());
        SINCRONIZANDO.with(|estado| estado.set(false));
        let contexto = barra.cabecera.style_context();
        if crate::MAXIMIZADA.load(Ordering::Relaxed) { contexto.add_class("maximizada"); } else { contexto.remove_class("maximizada"); }
        let bg = color(&paleta, oscuro, "--bg", if oscuro { "#000000" } else { "#f2f2f7" });
        let card = color(&paleta, oscuro, "--card", if oscuro { "#1c1c1e" } else { "#ffffff" });
        let tx = color(&paleta, oscuro, "--tx", if oscuro { "#ffffff" } else { "#000000" });
        let tx2 = color(&paleta, oscuro, "--tx2", "#8e8e93");
        let blue = color(&paleta, oscuro, "--blue", if oscuro { "#0a84ff" } else { "#007aff" });
        let hov = color(&paleta, oscuro, "--hov", if oscuro { "rgba(255,255,255,.06)" } else { "rgba(0,0,0,.04)" });
        let sobre_acento = color(&paleta, oscuro, "--on-accent", "#ffffff");
        let red = color(&paleta, oscuro, "--red", if oscuro { "#ff453a" } else { "#ff3b30" });
        let css = format!(r#"
#lumina-barra {{ background: {bg}; color: {tx2}; border-radius: 20px 20px 0 0; font-size: 12px; }}
#lumina-barra.maximizada {{ border-radius: 0; }}
#lumina-barra button {{ min-width: 0; min-height: 0; padding: 0; margin: 0; border: 0; box-shadow: none; background: transparent; color: alpha({tx}, 0.6); border-radius: 6px; }}
#lumina-barra button:hover {{ background: {hov}; color: {tx}; }}
#lumina-barra #lumina-cerrar:hover {{ background: {red}; color: white; }}
#lumina-menu {{ background: {card}; color: {tx}; border-radius: 18px; border: 1px solid alpha({tx}, 0.10); }}
#lumina-menu button {{ min-height: 32px; padding: 4px 12px; border: 0; box-shadow: none; background: transparent; color: {tx}; border-radius: 13px; }}
#lumina-menu button:hover {{ background: alpha({tx}, 0.06); }}
#lumina-menu radio:checked, #lumina-menu check:checked {{ background: {blue}; color: {bg}; }}
#lumina-menu label {{ font-size: 14px; }}
#lumina-menu button:disabled, #lumina-menu radiobutton:disabled, #lumina-menu checkbutton:disabled {{ opacity: 0.4; }}
#lumina-menu .encabezado {{ font-size: 12px; font-weight: 500; color: {tx2}; padding: 6px 12px 2px; }}
#lumina-menu modelbutton {{ min-height: 32px; padding: 4px 12px; border-radius: 13px; color: {tx}; }}
#lumina-menu modelbutton:hover {{ background: alpha({tx}, 0.06); }}
#lumina-bloqueo {{ background: {bg}; color: {tx}; border-radius: 0 0 20px 20px; }}
#lumina-bloqueo.maximizada {{ border-radius: 0; }}
#lumina-bloqueo scrolledwindow, #lumina-bloqueo viewport {{ background: transparent; border: 0; }}
#lumina-bloqueo-titulo {{ font-size: 22px; font-weight: bold; letter-spacing: -0.3px; color: {tx}; }}
#lumina-bloqueo-subtitulo {{ font-size: 13px; color: {tx2}; }}
#lumina-bloqueo-error {{ font-size: 13px; color: {red}; }}
#lumina-bloqueo entry {{ min-height: 48px; padding: 0 14px; border-radius: 14px; border: 0; box-shadow: none; background: {card}; color: {tx}; font-size: 17px; caret-color: {blue}; }}
#lumina-bloqueo entry:focus {{ box-shadow: inset 0 0 0 2px {blue}; }}
#lumina-bloqueo button {{ min-width: 0; min-height: 0; padding: 0; border: 0; box-shadow: none; outline: none; transition: background-color 120ms cubic-bezier(.4,0,.2,1); }}
#lumina-bloqueo button.tecla, #lumina-huella {{ border-radius: 999px; background: {hov}; color: {tx}; font-size: 22px; font-weight: 500; }}
#lumina-bloqueo button.tecla:hover, #lumina-huella:hover {{ background: alpha({tx}, 0.10); }}
#lumina-bloqueo button.tecla:active, #lumina-huella:active {{ background: alpha({tx}, 0.14); }}
#lumina-bloqueo button.tecla.aceptar {{ background: {blue}; }}
#lumina-bloqueo button.primario, #lumina-bloqueo button.secundario {{ min-height: 48px; border-radius: 14px; font-size: 15px; font-weight: 600; }}
#lumina-bloqueo button.primario {{ background: {blue}; color: {sobre_acento}; }}
#lumina-bloqueo button.secundario {{ background: {hov}; color: {tx}; }}
#lumina-bloqueo button:disabled {{ opacity: 0.5; }}
"#);
        if let Err(e) = barra.proveedor.load_from_data(css.as_bytes()) { eprintln!("lumina: no se pudo aplicar la paleta nativa: {e}"); }
        crate::bloqueo::recolorear(&tx, &tx2, &blue, &sobre_acento, crate::MAXIMIZADA.load(Ordering::Relaxed));
    });
}

#[cfg(debug_assertions)]
pub fn capturar(ruta: &std::path::Path) {
    BARRA.with_borrow(|estado| {
        let Some(barra) = estado else {
            return;
        };
        for (nombre, widget) in [
            ("barra", barra.cabecera.clone().upcast::<gtk::Widget>()),
            ("menu", barra.menu.clone().upcast()),
        ] {
            if !widget.is_visible() {
                continue;
            }
            let resultado = (|| -> Result<(), String> {
                let superficie = gtk::cairo::ImageSurface::create(
                    gtk::cairo::Format::ARgb32,
                    widget.allocated_width(),
                    widget.allocated_height(),
                )
                .map_err(|e| e.to_string())?;
                let contexto = gtk::cairo::Context::new(&superficie).map_err(|e| e.to_string())?;
                widget.draw(&contexto);
                let imagen = gtk::gdk::pixbuf_get_from_surface(
                    &superficie,
                    0,
                    0,
                    superficie.width(),
                    superficie.height(),
                )
                .ok_or("captura vacía")?;
                imagen
                    .savev(ruta.with_extension(format!("{nombre}.png")), "png", &[])
                    .map_err(|e| e.to_string())
            })();
            if let Err(e) = resultado {
                eprintln!("lumina: no se pudo capturar {nombre}: {e}");
            }
        }
    });
}

#[cfg(debug_assertions)]
pub fn abrir_menu() {
    BARRA.with_borrow(|estado| {
        if let Some(barra) = estado {
            barra.menu.popup();
            // `LUMINA_CAPTURA_MENU=bloqueo` abre el submenú.
            if std::env::var("LUMINA_CAPTURA_MENU").as_deref() == Ok("bloqueo") {
                barra.menu.open_submenu("bloqueo");
            }
        }
    });
}
