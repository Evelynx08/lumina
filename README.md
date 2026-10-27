<div align="center">

# Lumina

**WhatsApp Web as a native BookOS app.**

[![License](https://img.shields.io/badge/license-proprietary-lightgrey?style=flat-square)](#license)
[![Rust](https://img.shields.io/badge/Rust-2021-000000?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Tauri](https://img.shields.io/badge/Tauri-2-24c8db?style=flat-square&logo=tauri&logoColor=white)](https://tauri.app)
[![WebKitGTK](https://img.shields.io/badge/WebKitGTK-4.1-blue?style=flat-square)](https://webkitgtk.org)

</div>

---

## What this is

Lumina wraps `https://web.whatsapp.com` in a small Tauri 2 window and makes it
behave like it belongs to the desktop: BookOS toasts for messages, the BookOS
context menu, the wallpaper-derived colour palette, and a title bar that
follows the BookOS HIG.

It is built **in tune with [bookos-desktop](../bookos-desktop)**, the BookOS
Wayland compositor and shell, and it relies on how that session works rather
than reimplementing it:

- **Notifications** go to `org.freedesktop.Notifications`, which in a BookOS
  session is served by the compositor itself, so WhatsApp messages show up as
  regular BookOS toasts.
- **Dynamic colour**: Settings generates a palette from the wallpaper into
  `~/.config/bookos/palette.css`; Lumina injects it into WhatsApp and re-syncs
  it when the window regains focus.
- **Wallpaper as chat background**: the current BookOS desktop wallpaper can be
  used behind the conversations.
- **Design tokens** (`src/tokens.css`) and the title bar height (36 px) come
  from the BookOS HIG, so the window matches the rest of the apps.

It also runs on any other Linux desktop with WebKitGTK 4.1; the BookOS-specific
bits simply fall back to their defaults when the files or services are absent.

## Features

- **Native notifications** for incoming messages, with a toast when a download
  finishes.
- **Dynamic BookOS palette** applied to WhatsApp, with a switch to turn it off.
- **Chat background**: desktop wallpaper, your own image, or none. Images are
  downscaled to 1920 px JPEG before being injected, so a 4K wallpaper does not
  bloat the stylesheet.
- **BookOS context menu** (Cut / Copy / Paste and link actions) drawn inside the
  page in a Shadow DOM, so WhatsApp's styles cannot touch it.
- **Pasting screenshots works.** WebKitGTK fires an empty `paste` event when the
  clipboard only holds an image; Lumina reads it through the async clipboard API
  and re-dispatches it to WhatsApp as a file.
- **Voice notes and photos**: microphone and camera permissions are granted;
  everything else (location, pointer lock…) is denied without asking.
- **Downloads** land in your Downloads folder without overwriting existing files.
- **External links** open in your default browser through `xdg-open`.
- **Closing the window keeps the app running**, so messages keep arriving.
- **Single instance**: launching it again focuses the existing window.
- **Light and dark** follow the system theme.

## Design notes

- **WhatsApp has no IPC.** The title bar and WhatsApp are two webviews in the
  same window. Only the bar can call Tauri commands; the remote page gets none.
  Everything the app needs from WhatsApp (notifications, permissions, context
  menu, colours) is resolved on the Rust side through native WebKitGTK signals.
- **Linux only.** It depends on `webkit2gtk` and `gtk` directly.
- Small release binary: `opt-level = "z"`, LTO, `panic = "abort"`, stripped.

## Install

### BookOS (Fedora)

```sh
sudo dnf install webkit2gtk4.1-devel gtk3-devel libsoup3-devel glib2-devel \
  xdg-utils rust cargo pkgconf-pkg-config
cd src-tauri
cargo tauri build        # produces .rpm and .deb in target/release/bundle/
sudo dnf install target/release/bundle/rpm/*.rpm
```

### Other distributions

`cargo tauri build` also produces the `.deb`. On Arch there is a `PKGBUILD`
(`makepkg -si`).

### Dependencies

Runtime: `webkit2gtk4.1`, `gtk3`, `libsoup3`, `glib2`, `xdg-utils`.
Build: `rust`, `cargo`, `pkgconf-pkg-config` and the `-devel` packages above.

## Build and run

```sh
cd src-tauri
cargo run                # debug build
cargo build --release    # optimised binary
```

### Debug screenshots

Debug builds can capture each webview to PNG without any desktop tool:

```sh
LUMINA_CAPTURA=/tmp/ventana.png LUMINA_CAPTURA_ESPERA=15 cargo run
# → /tmp/ventana.barra.png and /tmp/ventana.whatsapp.png
```

See the header of `src-tauri/src/captura.rs` for the rest of the variables.

## Memoria

Lumina usa una caché moderada de recursos de WebKit y no retiene páginas para
volver atrás. El proceso de red revisa su memoria cada 10 segundos y comienza
a recuperarla a partir de 128 MiB, con una política más estricta a partir de
192 MiB. Estos umbrales son solo del proceso de red, no un límite de toda la
aplicación. No se cierran procesos ni se recarga WhatsApp para liberar RAM.

Las actualizaciones de estilos se agrupan en un único trabajador, con una sola
petición pendiente, y la hoja de usuario solo se sustituye si cambia su contenido.
Esto evita acumular hilos y volver a procesar el fondo al recuperar el foco.

Para comprobar el objetivo de 1,2 GB hay que medir el binario `release` junto
con todos sus procesos hijos durante el uso habitual. La suma de PSS reparte
la memoria compartida; la suma de RSS la cuenta en cada proceso que la utiliza.
Los ajustes no garantizan un máximo absoluto: el historial y los archivos
multimedia que mantenga vivos WhatsApp también influyen en el consumo.

Comprobación del 6 de octubre de 2026, WebKitGTK 2.54.0, `release`, sesión
iniciada y lista de chats en reposo durante 90 segundos, muestreada cada segundo:
805 MB de PSS y 1311 MB de RSS sumado al terminar; picos de 869 MB y 1384 MB,
respectivamente (MB decimales). No se observó una reducción concluyente en
reposo frente al binario anterior. No se han validado sesiones largas, llamadas
ni archivos multimedia grandes; el objetivo estricto de 1,2 GB de RSS sigue
pendiente.

## Project layout

```
src/                 title bar UI (HTML, CSS, JS) and BookOS tokens
src-tauri/src/
  main.rs            window, webviews, commands, downloads
  webkit.rs          WebKitGTK signals: permissions, notifications, menu
  notificaciones.rs  toasts via org.freedesktop.Notifications (zbus)
  fondo.rs           chat background
  pagina.js          injected into WhatsApp: context menu, image paste
  whatsapp.css       user stylesheet over WhatsApp
  captura.rs         debug-only screenshots
PKGBUILD             Arch package (secondary; BookOS ships the .rpm)
```

## License

Proprietary, all rights reserved (`LicenseRef-BookOS`). See [`LICENSE`](LICENSE).

Lumina is an unofficial client and is not affiliated with WhatsApp or Meta.
