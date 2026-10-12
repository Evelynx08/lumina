// Lo que Lumina inyecta en WhatsApp Web: el menú contextual de BookOS y el
// arreglo para pegar imágenes.
//
// ── Menú contextual ──
// Lo abre Rust desde la señal `context-menu` de WebKitGTK, que se cancela para
// que no salga el menú de GTK. Va en un Shadow DOM: las hojas de WhatsApp no lo
// alcanzan, y las variables de BookOS sí, porque se heredan a través de él.
(function () {
  'use strict';
  if (location.host !== 'web.whatsapp.com') return;

  var CSS =
    ':host{all:initial}' +
    '.menu{position:fixed;z-index:2147483647;min-width:200px;padding:6px;' +
    'background:color-mix(in srgb,var(--card) 94%,transparent);' +
    'border:1px solid color-mix(in srgb,var(--tx) 9%,transparent);border-radius:18px;' +
    'box-shadow:0 6px 22px rgba(0,0,0,.18);transform-origin:top left;' +
    'font-family:-apple-system,BlinkMacSystemFont,"Segoe UI","Noto Sans",Roboto,sans-serif;' +
    'animation:aparecer 180ms cubic-bezier(.32,1.3,.5,1)}' +
    '@keyframes aparecer{from{opacity:0;transform:scale(.94)}}' +
    '@media (prefers-reduced-motion:reduce){.menu{animation:none}}' +
    'button{display:block;width:100%;min-height:36px;padding:0 12px;border:0;border-radius:13px;' +
    'background:none;color:var(--tx);font:inherit;font-size:14px;font-weight:500;text-align:left;' +
    'cursor:pointer;transition:background-color 120ms cubic-bezier(.4,0,.2,1)}' +
    'button:hover{background:var(--hov)}button:active{background:var(--act)}';

  var host = null;

  function cerrar() {
    if (!host) return;
    host.remove();
    host = null;
    document.removeEventListener('mousedown', fuera, true);
    window.removeEventListener('blur', cerrar);
    window.removeEventListener('resize', cerrar);
    document.removeEventListener('scroll', cerrar, true);
    document.removeEventListener('keydown', tecla, true);
  }
  function fuera(e) { if (e.composedPath().indexOf(host) < 0) cerrar(); }
  function tecla(e) { if (e.key === 'Escape') { e.stopPropagation(); cerrar(); } }

  function copiarTexto(texto) {
    navigator.clipboard.writeText(texto).catch(function (e) {
      console.error('lumina: no se pudo copiar', e);
    });
  }

  window.__luminaMenu = function (d) {
    cerrar();
    var opciones = [];
    if (d.enlace) {
      opciones.push(['Abrir enlace', function () { window.open(d.enlace, '_blank', 'noopener'); }]);
      opciones.push(['Copiar enlace', function () { copiarTexto(d.enlace); }]);
    }
    if (d.editable && d.seleccion) opciones.push(['Cortar', function () { document.execCommand('cut'); }]);
    if (d.seleccion) opciones.push(['Copiar', function () { document.execCommand('copy'); }]);
    if (d.editable) opciones.push(['Pegar', function () { document.execCommand('paste'); }]);
    if (!opciones.length) return;

    host = document.createElement('lumina-menu');
    var raiz = host.attachShadow({ mode: 'closed' });
    var estilo = document.createElement('style');
    estilo.textContent = CSS;
    var menu = document.createElement('div');
    menu.className = 'menu';
    menu.setAttribute('role', 'menu');
    opciones.forEach(function (o) {
      var b = document.createElement('button');
      b.setAttribute('role', 'menuitem');
      b.textContent = o[0];
      // Sin esto el clic quita el foco al campo de texto y «Pegar» o «Cortar»
      // ya no tienen dónde actuar.
      b.addEventListener('mousedown', function (e) { e.preventDefault(); });
      b.addEventListener('click', function () { cerrar(); o[1](); });
      menu.appendChild(b);
    });
    raiz.append(estilo, menu);
    document.documentElement.appendChild(host);

    // Se mide ya montado para no salirse por la derecha ni por abajo.
    var r = menu.getBoundingClientRect();
    var x = Math.min(d.x, innerWidth - r.width - 8);
    var y = d.y + r.height > innerHeight - 8 ? d.y - r.height : d.y;
    menu.style.left = Math.max(8, x) + 'px';
    menu.style.top = Math.max(8, y) + 'px';

    document.addEventListener('mousedown', fuera, true);
    window.addEventListener('blur', cerrar);
    window.addEventListener('resize', cerrar);
    document.addEventListener('scroll', cerrar, true);
    document.addEventListener('keydown', tecla, true);
  };

  // ── Pegar imágenes ──
  //
  // WebKitGTK (GTK3) entrega el evento `paste` **vacío** cuando el
  // portapapeles solo ofrece una imagen, que es justo lo que deja una captura
  // de BookOS (`image/png` y nada más): sin tipos, sin archivos, y WhatsApp no
  // ve nada que pegar. La API asíncrona sí la lee (medido con un Ctrl+V real:
  // `paste` vacío, `clipboard.read()` → ["image/png"] con los 1991 bytes del
  // PNG de prueba), así que se lee por ahí y se le reenvía a WhatsApp un
  // `paste` con la imagen como archivo. En fase de captura, para ir antes que
  // los oyentes de WhatsApp; el `paste` vacío sigue su camino, inofensivo.
  document.addEventListener('paste', function (ev) {
    if (ev.__lumina) return;
    var datos = ev.clipboardData;
    if (!datos || datos.types.length || datos.files.length) return;
    var destino = ev.target;
    // Tiene que llamarse ya, dentro del gesto del Ctrl+V: fuera de él WebKit
    // contesta `NotAllowedError`.
    navigator.clipboard.read().then(function (elementos) {
      var lista = new DataTransfer();
      return Promise.all(elementos.map(function (elemento) {
        var tipo = elemento.types.find(function (t) { return t.indexOf('image/') === 0; });
        if (!tipo) return null;
        return elemento.getType(tipo).then(function (imagen) {
          lista.items.add(new File([imagen], 'captura.' + tipo.slice(6), { type: tipo }));
        });
      })).then(function () {
        if (!lista.files.length) return;
        var reenviado = new ClipboardEvent('paste', {
          clipboardData: lista, bubbles: true, cancelable: true, composed: true
        });
        reenviado.__lumina = true;
        destino.dispatchEvent(reenviado);
      });
    }).catch(function (e) {
      console.error('lumina: no se pudo leer la imagen del portapapeles', e);
    });
  }, true);
  // Responder desde un aviso de BookOS. El compositor devuelve el texto por
  // D-Bus y Rust abre el chat (`Notification.onclick`) y llama a esto.
  //
  // El chat se abre solo, un instante después, así que lo primero que aparece
  // puede ser la caja de **otra** conversación. Se espera a que la cabecera
  // diga quién es el del aviso antes de escribir: mandar el mensaje a otra
  // persona es peor que no mandarlo.
  window.__luminaResponder = function (titulo, texto) {
    var intentos = 0;
    (function esperar() {
      var cabecera = document.querySelector('[data-testid="conversation-header"]');
      var caja = document.querySelector('[data-testid="compose-box"] div[contenteditable="true"]');
      if (!cabecera || !caja || cabecera.textContent.indexOf(titulo) === -1) {
        // Cien ms por intento: con la ventana oculta WebKit alarga los
        // temporizadores, y 80 intentos siguen dando margen de sobra.
        if (++intentos < 80) setTimeout(esperar, 100);
        else console.error('lumina: el chat de «' + titulo + '» no llegó a abrirse');
        return;
      }
      caja.focus();
      document.execCommand('insertText', false, texto);
      // El botón de enviar nace al haber texto; se pulsa en el siguiente
      // turno para que WhatsApp haya reaccionado a lo escrito.
      setTimeout(function () {
        var icono = document.querySelector('[data-icon="send"]');
        var enviar = icono && icono.closest('button, [role="button"]');
        if (enviar) {
          enviar.click();
        } else {
          caja.dispatchEvent(new KeyboardEvent('keydown', {
            key: 'Enter', code: 'Enter', keyCode: 13, which: 13, bubbles: true
          }));
        }
      }, 50);
    })();
  };

  // ── Audio en reposo ──
  //
  // WhatsApp crea un `AudioContext` al cargar y lo deja en marcha aunque no
  // suene nada. En WebKitGTK eso es una tubería de GStreamer renderizando
  // silencio: medido en reposo, 13 % de CPU del proceso de la página, y el
  // 86 % de eso en los hilos `webaudioSrc*` y el de PulseAudio. Los contextos
  // nacen suspendidos, despiertan cuando algo va a sonar o se conecta un
  // micrófono o un elemento multimedia, y vuelven a dormir al quedar sin uso.
  //
  // No se apaga WebAudio entero: las notas de voz pueden depender de él y
  // no hay forma de probarlo sin grabar en un chat de verdad.
  var AudioOriginal = window.AudioContext;
  if (!AudioOriginal) return;
  var contextos = [];
  var sonando = 0;
  var flujos = [];
  var elementos = [];

  function despertar(contexto) {
    if (contexto.state === 'suspended') contexto.resume();
  }
  function enUso() {
    return sonando > 0 ||
      flujos.some(function (f) {
        return f.getTracks().some(function (p) { return p.readyState === 'live'; });
      }) ||
      elementos.some(function (e) { return !e.paused; });
  }

  // Con `class` y no con una función: así `instanceof`, los métodos y los
  // getters nativos siguen funcionando sobre el objeto devuelto.
  window.AudioContext = class extends AudioOriginal {
    constructor(opciones) {
      super(opciones);
      contextos.push(this);
      this.suspend();
    }
  };

  var empezar = AudioScheduledSourceNode.prototype.start;
  AudioScheduledSourceNode.prototype.start = function () {
    // Primero el original: si lanza (un segundo `start`), no hay `ended` que
    // devuelva la cuenta.
    var resultado = empezar.apply(this, arguments);
    sonando++;
    this.addEventListener('ended', function () { sonando--; }, { once: true });
    despertar(this.context);
    return resultado;
  };
  var desdeFlujo = AudioOriginal.prototype.createMediaStreamSource;
  AudioOriginal.prototype.createMediaStreamSource = function (flujo) {
    flujos.push(flujo);
    despertar(this);
    return desdeFlujo.apply(this, arguments);
  };
  var desdeElemento = AudioOriginal.prototype.createMediaElementSource;
  AudioOriginal.prototype.createMediaElementSource = function (elemento) {
    elementos.push(elemento);
    despertar(this);
    return desdeElemento.apply(this, arguments);
  };

  // Parar una pista con `stop()` no lanza ningún evento, así que el fin de
  // una grabación solo se ve preguntando. Un temporizador cada 5 s no se nota
  // al lado de los hilos de audio que duerme.
  setInterval(function () {
    contextos = contextos.filter(function (c) { return c.state !== 'closed'; });
    flujos = flujos.filter(function (f) { return f.active; });
    elementos = elementos.filter(function (e) { return e.isConnected || !e.paused; });
    if (enUso()) return;
    contextos.forEach(function (c) { if (c.state === 'running') c.suspend(); });
  }, 5000);
})();
