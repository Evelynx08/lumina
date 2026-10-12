//! Contraseña del sistema y huella, por PAM, como la pantalla de bloqueo de
//! BookOS.
//!
//! Adaptado de `bookos-desktop/crates/bookos-comp/src/autenticar.rs`: los
//! mismos servicios (`bookos` y `bookos-fingerprint`) y la misma conversación.
//! Lumina no guarda ningún secreto del sistema: solo recibe el sí o el no.
//! Todo bloquea (una contraseña mal puesta tarda segundos a propósito), así
//! que se llama siempre desde un hilo aparte, nunca desde el de GTK.
//! `libpam.so.0` se carga en tiempo de ejecución para compilar sin sus
//! cabeceras de desarrollo.

use std::ffi::{c_char, c_int, c_void, CStr, CString};

const PAM_SUCCESS: c_int = 0;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;
const PAM_CONV_ERR: c_int = 19;
/// Lo que contesta `pam_fprintd` sin lector de huellas: no es un rechazo,
/// es que no había con qué preguntar.
const PAM_AUTHINFO_UNAVAIL: c_int = 9;

const SERVICIO_HUELLA: &str = "bookos-fingerprint";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Veredicto {
    Correcto,
    /// Contraseña incorrecta, o huella que no casa en ningún intento.
    Rechazado,
    /// No se pudo ni preguntar: no hay lector, o PAM no está.
    NoDisponible,
}

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type Conversar = unsafe extern "C" fn(
    c_int,
    *mut *const PamMessage,
    *mut *mut PamResponse,
    *mut c_void,
) -> c_int;

#[repr(C)]
struct PamConv {
    conv: Option<Conversar>,
    appdata_ptr: *mut c_void,
}

struct Secretos<'a> {
    usuario: CString,
    contrasena: CString,
    /// Con `max-tries`, `pam_fprintd` avisa de cada dedo que no casa con un
    /// `PAM_ERROR_MSG` y sigue esperando el siguiente.
    dedo_no_casa: Option<&'a dyn Fn()>,
}

/// A ceros, sin que el optimizador quite unas escrituras que nadie vuelve a
/// leer: Lumina vive toda la sesión y puede acabar en un volcado.
pub fn borrar(bytes: &mut [u8]) {
    for b in bytes {
        // SAFETY: el puntero sale de una referencia mutable viva.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
}

/// El usuario de la sesión, por su UID: nunca una variable de entorno.
pub fn usuario() -> String {
    let mut entrada: libc::passwd = unsafe { std::mem::zeroed() };
    let mut resultado = std::ptr::null_mut();
    let mut buffer = vec![0_u8; 65536];
    // SAFETY: los punteros apuntan a memoria propia viva durante la llamada.
    let estado = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut entrada,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut resultado,
        )
    };
    if estado != 0 || resultado.is_null() || entrada.pw_name.is_null() {
        return String::new();
    }
    // SAFETY: `pw_name` apunta dentro de `buffer`, que sigue vivo.
    unsafe { CStr::from_ptr(entrada.pw_name) }
        .to_string_lossy()
        .into_owned()
}

/// Hay servicio de huella instalado. Que haya un dedo registrado solo se sabe
/// al preguntar: sin él, PAM contesta `NoDisponible`.
pub fn huella_instalada() -> bool {
    std::path::Path::new("/etc/pam.d")
        .join(SERVICIO_HUELLA)
        .is_file()
}

fn servicio_contrasena() -> &'static str {
    if std::path::Path::new("/etc/pam.d/bookos").is_file() {
        "bookos"
    } else {
        // Las pilas generales pueden incluir `pam_fprintd` y hacer esperar al
        // lector antes de mirar la contraseña.
        ["system-auth", "common-auth", "login"]
            .into_iter()
            .find(|nombre| std::path::Path::new("/etc/pam.d").join(nombre).is_file())
            .unwrap_or("login")
    }
}

pub fn contrasena(usuario: &str, contrasena: &[u8]) -> Veredicto {
    autenticar(servicio_contrasena(), usuario, contrasena, None)
}

/// Espera al lector hasta que casa un dedo o se acaban los intentos que
/// configure `bookos-fingerprint` (3 en 15 s en BookOS).
pub fn huella(usuario: &str, dedo_no_casa: &dyn Fn()) -> Veredicto {
    autenticar(SERVICIO_HUELLA, usuario, b"", Some(dedo_no_casa))
}

fn autenticar(
    nombre: &str,
    usuario: &str,
    contrasena: &[u8],
    dedo_no_casa: Option<&dyn Fn()>,
) -> Veredicto {
    let (Ok(usuario), Ok(contrasena), Ok(servicio)) = (
        CString::new(usuario),
        CString::new(contrasena),
        CString::new(nombre),
    ) else {
        return Veredicto::Rechazado;
    };
    if usuario.is_empty() {
        return Veredicto::Rechazado;
    }

    // SAFETY: la biblioteca vive hasta después de `pam_end`; las firmas son
    // las de la ABI estable de PAM.
    unsafe {
        let Ok(pam) = libloading::Library::new("libpam.so.0") else {
            eprintln!("lumina: no se pudo cargar libpam.so.0");
            return Veredicto::NoDisponible;
        };
        type Start = unsafe extern "C" fn(
            *const c_char,
            *const c_char,
            *const PamConv,
            *mut *mut c_void,
        ) -> c_int;
        type Paso = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
        let (Ok(start), Ok(authenticate), Ok(account), Ok(end)) = (
            pam.get::<Start>(b"pam_start\0"),
            pam.get::<Paso>(b"pam_authenticate\0"),
            pam.get::<Paso>(b"pam_acct_mgmt\0"),
            pam.get::<Paso>(b"pam_end\0"),
        ) else {
            return Veredicto::NoDisponible;
        };

        let mut secretos = Secretos {
            usuario,
            contrasena,
            dedo_no_casa,
        };
        let conversacion = PamConv {
            conv: Some(conversar),
            appdata_ptr: (&mut secretos as *mut Secretos).cast(),
        };
        let mut handle: *mut c_void = std::ptr::null_mut();
        let inicio = start(
            servicio.as_ptr(),
            secretos.usuario.as_ptr(),
            &conversacion,
            &mut handle,
        );
        if inicio != PAM_SUCCESS || handle.is_null() {
            eprintln!("lumina: pam_start ({nombre}) falló con el código {inicio}");
            return Veredicto::NoDisponible;
        }
        let autenticacion = authenticate(handle, 0);
        let resultado = if autenticacion == PAM_SUCCESS {
            account(handle, 0)
        } else {
            autenticacion
        };
        let _ = end(handle, resultado);
        let mut bytes = secretos.contrasena.into_bytes_with_nul();
        borrar(&mut bytes);
        match resultado {
            PAM_SUCCESS => Veredicto::Correcto,
            PAM_AUTHINFO_UNAVAIL => Veredicto::NoDisponible,
            _ => Veredicto::Rechazado,
        }
    }
}

unsafe extern "C" fn conversar(
    cuantos: c_int,
    mensajes: *mut *const PamMessage,
    respuestas: *mut *mut PamResponse,
    datos: *mut c_void,
) -> c_int {
    if cuantos <= 0 || mensajes.is_null() || respuestas.is_null() || datos.is_null() {
        return PAM_CONV_ERR;
    }
    // SAFETY: `datos` es el `Secretos` de `autenticar`, vivo durante toda la
    // conversación, que corre en el mismo hilo.
    let secretos = unsafe { &*(datos.cast::<Secretos>()) };
    let bloque = unsafe {
        libc::calloc(cuantos as usize, std::mem::size_of::<PamResponse>()).cast::<PamResponse>()
    };
    if bloque.is_null() {
        return PAM_CONV_ERR;
    }
    for i in 0..cuantos as usize {
        let mensaje = unsafe { *mensajes.add(i) };
        if mensaje.is_null() {
            liberar_respuestas(bloque, i);
            return PAM_CONV_ERR;
        }
        let texto = match unsafe { (*mensaje).msg_style } {
            PAM_PROMPT_ECHO_OFF => Some(secretos.contrasena.as_c_str()),
            PAM_PROMPT_ECHO_ON => Some(secretos.usuario.as_c_str()),
            PAM_ERROR_MSG => {
                if let Some(aviso) = secretos.dedo_no_casa {
                    aviso();
                }
                None
            }
            PAM_TEXT_INFO => None,
            _ => {
                liberar_respuestas(bloque, i);
                return PAM_CONV_ERR;
            }
        };
        if let Some(texto) = texto {
            // PAM libera la respuesta con `free`: tiene que salir de `malloc`.
            let origen = texto.to_bytes_with_nul();
            let copia = unsafe { libc::malloc(origen.len()).cast::<c_char>() };
            if copia.is_null() {
                liberar_respuestas(bloque, i);
                return PAM_CONV_ERR;
            }
            unsafe {
                std::ptr::copy_nonoverlapping(origen.as_ptr().cast(), copia, origen.len());
                (*bloque.add(i)).resp = copia;
            }
        }
    }
    unsafe { *respuestas = bloque };
    PAM_SUCCESS
}

fn liberar_respuestas(respuestas: *mut PamResponse, cuantos: usize) {
    for i in 0..cuantos {
        unsafe { libc::free((*respuestas.add(i)).resp.cast()) };
    }
    unsafe { libc::free(respuestas.cast()) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn una_identidad_ausente_nunca_autentica() {
        assert_ne!(contrasena("", b"x"), Veredicto::Correcto);
        assert_ne!(contrasena("usuario\0invalido", b"x"), Veredicto::Correcto);
    }

    #[test]
    fn una_contrasena_falsa_no_pasa() {
        assert_ne!(
            contrasena(&usuario(), b"no-es-esta-contrasena-12345"),
            Veredicto::Correcto
        );
    }

    #[test]
    fn el_usuario_es_el_de_la_sesion() {
        assert!(!usuario().is_empty());
    }
}
