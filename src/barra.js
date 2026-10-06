'use strict';
// La barra es la única página con IPC: WhatsApp vive en otra webview sin
// comandos concedidos, y lo que la app necesite de él se resuelve en Rust.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const ventana = window.__TAURI__.window.getCurrentWindow();

const $ = (id) => document.getElementById(id);
const mas = $('mas');
const menu = $('menu');
const velo = $('velo');
const paleta = $('paleta');

$('minimizar').onclick = () => ventana.minimize();
$('maximizar').onclick = () => ventana.toggleMaximize();
$('cerrar').onclick = () => ventana.close();

async function marcarMaximizada() {
  document.documentElement.classList.toggle('maximizada', await ventana.isMaximized());
}
ventana.onResized(marcarMaximizada);
marcarMaximizada();

// Primero se agranda la webview y después se pinta el menú: al revés, el
// primer fotograma lo recortaría la franja de 36 px.
async function abrirMenu() {
  await invoke('desplegar_barra', { abierta: true });
  menu.hidden = false;
  velo.hidden = false;
  mas.setAttribute('aria-expanded', 'true');
}

async function cerrarMenu() {
  if (menu.hidden) return;
  menu.hidden = true;
  velo.hidden = true;
  mas.setAttribute('aria-expanded', 'false');
  await invoke('desplegar_barra', { abierta: false });
}

mas.onclick = () => (menu.hidden ? abrirMenu() : cerrarMenu());
velo.onmousedown = cerrarMenu;
document.addEventListener('keydown', (e) => { if (e.key === 'Escape') cerrarMenu(); });

$('recargar').onclick = async () => {
  await cerrarMenu();
  invoke('recargar_whatsapp');
};
$('salir').onclick = () => invoke('salir');

paleta.checked = window.bookosPalette.enabled();
paleta.onchange = () => {
  window.bookosPalette.setEnabled(paleta.checked);
  invoke('paleta_whatsapp', { activa: paleta.checked });
};
// La preferencia vive aquí, en el localStorage de la barra; Rust la olvida al
// salir y hay que repetírsela en cada arranque.
invoke('paleta_whatsapp', { activa: paleta.checked });

// Fondo de los chats. Como la paleta, se guarda aquí y se repite al arrancar.
const FONDO = 'lumina.fondo';
const FONDO_IMAGEN = 'lumina.fondoImagen';
const leer = (clave) => { try { return localStorage.getItem(clave); } catch (e) { return null; } };
const guardar = (clave, valor) => { try { localStorage.setItem(clave, valor); } catch (e) {} };

function marcarFondo(modo, ruta) {
  document.querySelectorAll('[data-fondo]').forEach((b) => {
    b.setAttribute('aria-checked', String(b.dataset.fondo === modo));
  });
  $('nombre-imagen').textContent = ruta ? ruta.split('/').pop() : 'Imagen propia…';
}

async function ponerFondo(modo, ruta) {
  await invoke('fondo_chats', { modo, ruta: ruta || null });
  guardar(FONDO, modo);
  if (ruta) guardar(FONDO_IMAGEN, ruta);
  marcarFondo(modo, leer(FONDO_IMAGEN));
}

document.querySelectorAll('[data-fondo]').forEach((boton) => {
  boton.onclick = async () => {
    const modo = boton.dataset.fondo;
    if (modo !== 'imagen') return ponerFondo(modo);
    // El selector del sistema es otra ventana: el menú se cierra antes para
    // no quedarse abierto detrás de él.
    await cerrarMenu();
    const ruta = await invoke('elegir_fondo');
    if (ruta) ponerFondo('imagen', ruta);
  };
});

{
  const modo = leer(FONDO) || 'escritorio';
  const ruta = leer(FONDO_IMAGEN);
  // Si la imagen propia se borró del disco, Rust no la podrá abrir y el chat
  // queda sin fondo; la elección se conserva para cuando vuelva.
  ponerFondo(modo === 'imagen' && !ruta ? 'escritorio' : modo, modo === 'imagen' ? ruta : null);
}

// El foco casi nunca lo tiene esta webview sino la de WhatsApp, así que el
// `focus` que escucha bookos-palette.js no basta: Rust avisa por la ventana.
listen('lumina://foco', () => window.bookosPalette.sync());
