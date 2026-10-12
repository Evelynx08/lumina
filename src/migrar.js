'use strict';
// Se ejecuta una sola vez en el origen local antiguo para conservar las preferencias.
window.__TAURI__.core.invoke('migrar_preferencias', {
  datos: {
    paleta: localStorage.getItem('bookos.dynamicPalette') !== 'false',
    fondo: localStorage.getItem('lumina.fondo') || 'escritorio',
    imagen: localStorage.getItem('lumina.fondoImagen'),
  },
}).catch((error) => console.error('lumina: no se pudieron migrar las preferencias', error));
