# Pulso

Reproductor de música libre, rápido y liviano. Sin telemetría, sin publicidad, sin WebView.

- **Núcleo en Rust** (`crates/core`): audio (symphonia vía rodio: MP3, FLAC, OGG, AAC/M4A, WAV), biblioteca SQLite con escaneo incremental, etiquetas (lofty) y letras sincronizadas (LRCLIB). No depende de la interfaz.
- **Interfaz en Slint** (`crates/app`) con renderizador por software: no necesita GPU, drivers ni runtimes externos. Un solo `.exe`.
- El audio abre el dispositivo solo al reproducir y corre en su propio hilo. La red solo se usa al pedir una letra.

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
