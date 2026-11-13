# Pulso

Reproductor de música libre, rápido y liviano. Sin telemetría, sin publicidad, sin WebView.

- **Núcleo en Rust** (`crates/core`): audio (symphonia vía rodio: MP3, FLAC, OGG, AAC/M4A, WAV), biblioteca SQLite con escaneo incremental, etiquetas (lofty) y letras sincronizadas (LRCLIB). No depende de la interfaz.
- **Interfaz en Slint** (`crates/app`) con renderizador por software: no necesita GPU, drivers ni runtimes externos. Un solo `.exe`.
- El audio abre el dispositivo solo al reproducir y corre en su propio hilo. La red solo se usa al pedir una letra.

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
