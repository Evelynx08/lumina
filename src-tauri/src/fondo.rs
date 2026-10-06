//! El fondo de las conversaciones.
//!
//! La página es `https://web.whatsapp.com` y no puede cargar un `file://`, así
//! que la imagen entra en la hoja de usuario como `data:`. Se reduce antes a
//! 1920 px de lado y JPEG: un fondo 4K en PNG pesa varios MB y la hoja se
//! reaplica cada vez que la ventana recupera el foco.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use futures_util::StreamExt;

/// Lo que eligió el usuario en el menú de la barra.
#[derive(Clone, PartialEq)]
pub enum Fondo {
    Ninguno,
    /// El fondo de pantalla de BookOS, el del tema que esté puesto.
    Escritorio,
    Imagen(PathBuf),
}

/// El fondo de pantalla que pinta el escritorio ahora mismo.
///
/// Mismas reglas que `bookos-shell` (`config.rs`): `fondo_claro` y
/// `fondo_oscuro` mandan sobre `fondo`, que vale para los dos temas.
fn del_escritorio(oscuro: bool) -> Option<PathBuf> {
    let conf = std::fs::read_to_string(dirs::config_dir()?.join("bookos/panel.conf")).ok()?;
    let valor = |clave: &str| {
        conf.lines().find_map(|linea| {
            let (k, v) = linea.split_once('=')?;
            (k.trim() == clave).then(|| v.trim().to_string())
        })
    };
    valor(if oscuro {
        "fondo_oscuro"
    } else {
        "fondo_claro"
    })
    .or_else(|| valor("fondo"))
    .filter(|v| !v.is_empty())
    .map(PathBuf::from)
}

/// La última imagen codificada, por ruta y fecha de modificación: recodificar
/// un fondo 4K en cada cambio de foco costaba más que el propio repintado.
static CACHE: Mutex<Option<(PathBuf, SystemTime, String)>> = Mutex::new(None);

fn como_data_url(ruta: &Path) -> Option<String> {
    let modificado = std::fs::metadata(ruta).and_then(|m| m.modified()).ok()?;
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((r, m, url)) = cache.as_ref() {
        if r == ruta && *m == modificado {
            return Some(url.clone());
        }
    }
    let imagen = match gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(ruta, 1920, 1920, true) {
        Ok(imagen) => imagen,
        Err(e) => {
            eprintln!("lumina: no se pudo abrir el fondo {}: {e}", ruta.display());
            return None;
        }
    };
    // El JPEG no tiene alfa: un PNG con transparencia se aplana sobre negro,
    // que es lo que deja ver el escritorio debajo de un fondo así.
    // `copy_area` no sirve aquí: de una imagen con alfa a otra sin él no copia
    // nada (aserción de GdkPixbuf) y el JPEG salía de memoria sin rellenar, en
    // negro. `composite` sí mezcla sobre un destino sin alfa.
    let imagen = if imagen.has_alpha() {
        let (ancho, alto) = (imagen.width(), imagen.height());
        let plano =
            gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, false, 8, ancho, alto)?;
        plano.fill(0x000000ff);
        imagen.composite(
            &plano,
            0,
            0,
            ancho,
            alto,
            0.0,
            0.0,
            1.0,
            1.0,
            gtk::gdk_pixbuf::InterpType::Nearest,
            255,
        );
        plano
    } else {
        imagen
    };
    let bytes = match imagen.save_to_bufferv("jpeg", &[("quality", "82")]) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("lumina: no se pudo codificar el fondo: {e}");
            return None;
        }
    };
    let url = format!(
        "data:image/jpeg;base64,{}",
        gtk::glib::base64_encode(&bytes)
    );
    *cache = Some((ruta.to_path_buf(), modificado, url.clone()));
    Some(url)
}

/// La regla CSS del fondo, o nada si no hay imagen que poner.
pub fn css(fondo: &Fondo, oscuro: bool) -> String {
    let ruta = match fondo {
        Fondo::Ninguno => return String::new(),
        Fondo::Escritorio => del_escritorio(oscuro),
        Fondo::Imagen(ruta) => Some(ruta.clone()),
    };
    let Some(url) = ruta.as_deref().and_then(como_data_url) else {
        return String::new();
    };
    // El velo del color del lienzo encima de la imagen es lo que deja leer las
    // burbujas sobre un fondo con mucho contraste; sin él, un fondo claro con
    // el tema oscuro se comía las recibidas.
    //
    // Va en `#main`, el contenedor de toda la conversación, y mensajes y caja
    // de texto quedan transparentes encima: un solo fondo de arriba abajo. Con
    // `background-attachment: fixed` en cada capa, WebKit escalaba la imagen a
    // cada una por separado y la franja de escribir enseñaba otro trozo, más
    // grande y sin velo. Las transparencias cuelgan de `#main`: si WhatsApp lo
    // quitara, queda la regla de solo los mensajes en vez de un corte.
    //
    // Los garabatos de WhatsApp son otra capa encima, con su propio dibujo:
    // fuera, o se mezclan con el fondo. `data-asset-chat-background-*` es el
    // atributo que llevan en WhatsApp Web; no viene de la extensión del
    // usuario, así que está sin comprobar con su sesión.
    let capa = format!(
        "background:linear-gradient(color-mix(in srgb,var(--bg) 35%,transparent),color-mix(in srgb,var(--bg) 35%,transparent)),url(\"{url}\") center/cover no-repeat !important"
    );
    format!(
        r#"[data-testid="conversation-panel-messages"],#main{{{capa}}}#main [data-testid="conversation-panel-messages"],#main [data-testid="compose-box"],#main footer{{background:transparent !important}}[data-asset-chat-background-dark],[data-asset-chat-background-light]{{display:none !important}}"#
    )
}

/// Pide una imagen por el portal `FileChooser`, que en BookOS abre el
/// explorador del sistema. `None` si el usuario cancela.
pub async fn elegir() -> zbus::Result<Option<PathBuf>> {
    let conexion = zbus::Connection::session().await?;
    // La respuesta llega por señal en una ruta que se puede calcular antes de
    // llamar; suscribirse después perdería una respuesta rápida.
    let token = format!("lumina_{}", std::process::id());
    let remitente = conexion
        .unique_name()
        .expect("una conexión al bus de sesión siempre tiene nombre único")
        .trim_start_matches(':')
        .replace('.', "_");
    let ruta = format!("/org/freedesktop/portal/desktop/request/{remitente}/{token}");
    let peticion = zbus::Proxy::new(
        &conexion,
        "org.freedesktop.portal.Desktop",
        ruta.as_str(),
        "org.freedesktop.portal.Request",
    )
    .await?;
    let mut respuestas = peticion.receive_signal("Response").await?;

    let portal = zbus::Proxy::new(
        &conexion,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.FileChooser",
    )
    .await?;
    let filtros = vec![(
        "Imágenes",
        vec![
            (0u32, "*.png"),
            (0, "*.jpg"),
            (0, "*.jpeg"),
            (0, "*.webp"),
            (0, "*.svg"),
        ],
    )];
    let mut opciones: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
    opciones.insert("handle_token", token.as_str().into());
    opciones.insert("modal", true.into());
    opciones.insert("filters", filtros.into());
    portal
        .call::<_, _, zbus::zvariant::OwnedObjectPath>(
            "OpenFile",
            &("", "Fondo de los chats", opciones),
        )
        .await?;

    let Some(mensaje) = respuestas.next().await else {
        return Ok(None);
    };
    let (codigo, resultados): (u32, HashMap<String, zbus::zvariant::OwnedValue>) =
        mensaje.body().deserialize()?;
    if codigo != 0 {
        return Ok(None);
    }
    let uris = resultados
        .get("uris")
        .and_then(|v| Vec::<String>::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default();
    Ok(uris
        .first()
        .and_then(|u| tauri::Url::parse(u).ok()?.to_file_path().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::gdk_pixbuf::prelude::*;

    /// El píxel central de una imagen `data:` ya decodificada.
    fn color_central(url: &str) -> [u8; 3] {
        let base64 = url
            .strip_prefix("data:image/jpeg;base64,")
            .expect("la regla lleva un JPEG en base64");
        let bytes = gtk::glib::base64_decode(base64);
        let cargador = gtk::gdk_pixbuf::PixbufLoader::new();
        cargador.write(&bytes).expect("JPEG legible");
        cargador.close().expect("JPEG completo");
        let imagen = cargador.pixbuf().expect("imagen decodificada");
        let (x, y) = (imagen.width() / 2, imagen.height() / 2);
        let i = (y * imagen.rowstride() + x * imagen.n_channels()) as usize;
        let pixeles = imagen.read_pixel_bytes();
        [pixeles[i], pixeles[i + 1], pixeles[i + 2]]
    }

    #[test]
    fn un_png_con_transparencia_se_aplana_a_jpeg_y_se_cachea() {
        let dir = std::env::temp_dir().join(format!("lumina-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("carpeta temporal");
        let ruta = dir.join("fondo.png");
        let imagen =
            gtk::gdk_pixbuf::Pixbuf::new(gtk::gdk_pixbuf::Colorspace::Rgb, true, 8, 3000, 2000)
                .expect("pixbuf de prueba");
        imagen.fill(0x6e4ab080);
        imagen
            .savev(&ruta, "png", &[])
            .expect("guardar el PNG de prueba");

        let inicio = std::time::Instant::now();
        let url = como_data_url(&ruta).expect("un PNG válido se codifica");
        let primera = inicio.elapsed();
        let color = color_central(&url);
        // #6e4ab0 al 50 % sobre negro, con margen para el JPEG. El aplanado
        // roto de antes dejaba aquí (0, 0, 0).
        for (canal, esperado) in color.into_iter().zip([55u8, 37, 88]) {
            assert!(
                canal.abs_diff(esperado) <= 6,
                "color {color:?}, esperado ≈ (55, 37, 88)"
            );
        }

        let inicio = std::time::Instant::now();
        assert_eq!(como_data_url(&ruta).as_deref(), Some(url.as_str()));
        let segunda = inicio.elapsed();
        eprintln!(
            "codificar: {primera:?}; desde la caché: {segunda:?}; {} KiB",
            url.len() / 1024
        );

        std::fs::remove_dir_all(&dir).expect("limpiar la carpeta temporal");
    }

    #[test]
    fn sin_imagen_no_hay_regla() {
        assert_eq!(css(&Fondo::Ninguno, false), "");
        assert_eq!(
            css(&Fondo::Imagen(PathBuf::from("/no/existe.png")), true),
            ""
        );
    }

    /// Depende del `panel.conf` de quien lo ejecute: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn el_fondo_del_escritorio_de_esta_maquina_se_codifica() {
        for oscuro in [false, true] {
            let inicio = std::time::Instant::now();
            let regla = css(&Fondo::Escritorio, oscuro);
            eprintln!(
                "oscuro={oscuro}: {:?}, {} KiB",
                inicio.elapsed(),
                regla.len() / 1024
            );
            assert!(regla.contains("data:image/jpeg;base64,"));
        }
    }
}
