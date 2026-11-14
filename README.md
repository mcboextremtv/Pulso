# Pulso

Reproductor de música libre, rápido y liviano. Sin telemetría, sin publicidad, sin WebView.

- **Núcleo en Rust** (`crates/core`): audio (symphonia vía rodio: MP3, FLAC, OGG, AAC/M4A, WAV), biblioteca SQLite con escaneo incremental, etiquetas (lofty) y letras sincronizadas (LRCLIB). No depende de la interfaz.
- **Interfaz en Slint** (`crates/app`) con renderizador por software: no necesita GPU, drivers ni runtimes externos. Un solo `.exe`.
- El audio abre el dispositivo solo al reproducir y corre en su propio hilo. La red solo se usa al pedir una letra.

## Hito 1.4: listas de reproducción, logo y clave de AcoustID
- **Listas tipo iTunes:** pestañas *Carpetas* / *Listas* en la barra lateral; tú eliges cómo navegar. En *Listas*: *Nueva lista*, *Renombrar*, *Eliminar* (no borra canciones). El botón **+** de cada fila abre «añadir a…» (lista existente o nueva); **Añadir todas a una lista** añade de una vez lo que estás viendo (una carpeta, o los resultados de una búsqueda). Dentro de una lista el botón es **−** (quitar de la lista). Las listas se enlazan por ruta de archivo: si un disco se desconecta y reaparece, sus canciones vuelven a la lista. No admiten canciones repetidas.
- **Logo e icono:** `assets/` trae `icon.svg`, `icon-*.png`, `icon.ico` y `favicon.ico`/`favicon.svg` (para la página del repositorio). El icono se incrusta en el `.exe` y se usa en la ventana.
- **Ajustes → AcoustID:** cuadro para la clave de *aplicación* (no la de usuario) y botón que abre https://acoustid.org/new-application. Por ahora solo se guarda; la identificación de canciones viene después.
- El desplazamiento al pasar de pista mueve la lista lo mínimo necesario.

## Hito 1.3: tabla, paneles ajustables y etiquetas
- **Paneles ajustables:** arrastra los divisores entre carpetas, canciones y letra; los tamaños se guardan (`%APPDATA%\Pulso\settings.txt`).
- **Tabla con columnas alineadas:** Título, Artista, Álbum y Duración. Clic en una cabecera ordena ascendente, otro clic descendente, un tercero vuelve al orden de carpeta. Arrastra el borde de una cabecera para cambiar su ancho.
- **Agrupar por artista** (botón sobre la tabla).
- **Editar etiquetas:** el lápiz de cada fila escribe título, artista y álbum dentro del archivo. Si es la canción que suena, se suelta un instante y se retoma en el mismo punto.
- **Lista que sigue a la canción:** al pasar a la siguiente pista (botón, o sola) la lista se desplaza hasta mostrarla; no se mueve si ya se ve ni mientras tú te desplazas.
- **Letra con líneas largas ajustadas al ancho** y desplazamiento automático a la línea activa.
- **Audio:** búfer de salida por defecto de 8192 tramas, cola de decodificación de 4 s y hilo de decodificación con prioridad algo mayor. En *Ajustes* puedes subir el búfer (Pequeño/Normal/Grande/Máximo); junto al botón aparece «cortes: N» si el decodificador no llegó a tiempo.

## Hito 1.2: carpetas y letras editables
- **Panel de carpetas** (izquierda): árbol real de tu disco con contadores; al elegir una carpeta se ven sus canciones y las de sus subcarpetas. «Todas las canciones» quita el filtro. La búsqueda funciona sobre la carpeta elegida.
- **Letras que tú controlas:** botones *Editar* y *Eliminar* sobre el panel de letra. En el editor puedes cambiar artista/título, pulsar *Buscar letra* (LRCLIB), *Buscar en la web* (abre tu navegador con la búsqueda; copias y pegas aquí), corregir el texto y *Guardar*. Con marcas `[mm:ss.xx]` se guarda como `.lrc` (sincronizada); sin marcas, como `.lyrics.txt`. Todo queda junto a la canción.
- Archivos sin etiquetas: el artista y el título se deducen del nombre (`017 - Artista - Título`); si no se puede, no se consulta la red y se avisa. Los errores de red ahora dicen la causa real.

## Hito 1.1: estabilidad del audio y del dibujo
- **Audio sin cortes:** un hilo aparte decodifica por adelantado y mantiene ~2 s de colchón; la tarjeta de sonido solo copia de ahí. El búfer de salida es de 4096 tramas (antes, el mínimo de Windows). Se prueba con `PULSO_AUDIO_BUFFER=<tramas>` (0 = el de Windows, 8192 = más margen).
- **Contador de cortes:** con `PULSO_BENCH=1` la consola imprime `cortes de audio acumulados: N` cuando el decodificador no llega a tiempo.
- **Dos modos de dibujo:** `PULSO_RENDERER=femtovg` (GPU, por defecto, con relanzamiento automático a software si la ventana no se puede crear) o `PULSO_RENDERER=software`. Probado en Windows 10: femtovg dibuja bien; software deja la ventana a medio pintar al restaurarla (no usar).
- Corregido: faltaban los códecs de `.wav` y `.ogg` (los MP3 funcionaban).

## Estado (hito 1)
Hecho: escanear carpetas, buscar, reproducir, pausa/anterior/siguiente, volumen, búsqueda en la barra de progreso (clic), letra sincronizada (se guarda como `.lrc` junto a la canción), paso automático a la siguiente pista.
Pendiente: MusicBrainz/AcoustID (completar etiquetas y carátulas), arrastrar en la barra de progreso, cola y listas, ecualizador, controles multimedia del sistema.

## Compilar en Windows 10 (equipo con 4 GB)
**Opción A, recomendada: no compilar en la laptop.** Sube el repo a GitHub; la pestaña *Actions* compila y publica `pulso.exe` como artefacto. Descárgalo y pruébalo.

**Opción B, compilar local:**
1. Instala *Build Tools for Visual Studio* con la carga "Desarrollo para el escritorio con C++" y luego `rustup-init.exe` (https://rustup.rs).
2. `cargo build --profile fast -p pulso -j 2`  (rápido, poca RAM, ejecutable en `target\fast\`)
3. Versión final: `cargo build --release -p pulso -j 2` (más lenta, binario más optimizado).

Prueba solo del audio y las letras, sin interfaz (compila en pocos minutos):
```
cargo run -p pulso-core --example play -- "C:\Musica\cancion.mp3"
```

## Medir la velocidad en la laptop
```
$env:PULSO_BENCH=1; .\pulso.exe     # imprime "ventana lista en X ms" (ejecútalo desde PowerShell)
```
Memoria: Administrador de tareas → pestaña Detalles → `pulso.exe` → Memoria (espacio de trabajo privado), en reposo y con una canción sonando.
Presupuesto: ventana en < 500 ms, sonido en < 1 s tras el doble clic, < 80 MB en reposo.

## Datos
Base de datos en `%APPDATA%\Pulso\library.db`. Los `.lrc` se guardan junto a cada canción.

## Licencia
GPL-3.0-or-later (por Slint). Añade el texto de la licencia como `LICENSE`.
