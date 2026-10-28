# Nananeas Torrent Downloader 0.1

[English](README.md)

Aplicación de escritorio en Rust para conservar fuentes magnet y .torrent, buscar archivos en su índice local y descargarlos individualmente.

## Funciones

Catálogo SQLite persistente; importación/exportación JSON; borrado y reintento individual de fuentes; copia del magnet completo; búsqueda por palabras literales en cualquier orden (no regex); descarga selectiva con confirmación; progreso individual y gráfica de velocidad total; opciones excluyentes de seeding y mover a la raíz; traducciones XML editables; confirmación de cierre.

## Estructura para Git

Conservar en la raíz Cargo.toml, Cargo.lock, build.rs, nananeas.sh, README.md, README.es.md, .gitignore y .gitattributes. Incluir src/, assets/, translations/, docs/ y la copia completa modificada vendor/librqbit-9.0.1/, con sus archivos LICENSE y NOTICE originales.

No subir target/, registros de diagnóstico, catálogos personales ni exportaciones que contengan enlaces privados. El ejecutable conserva el nombre torrent-finder por compatibilidad.

## Compilar y ejecutar en Linux

Necesitas Rust estable actual, compilador C/C++, pkg-config, bibliotecas de desarrollo OpenSSL, X11/XKB y controladores gráficos. Reutiliza la instalación Rust con la que ya compilaste.

Dependencias del sistema en Arch:

```bash
sudo pacman -S --needed base-devel git pkgconf openssl libxkbcommon libxcb mesa xdg-desktop-portal
```

Instala el backend del portal correspondiente a tu escritorio: por ejemplo xdg-desktop-portal-kde en KDE o xdg-desktop-portal-gtk en un escritorio GTK.

Desde la raíz:

```bash
rustc --version
cargo --version
cargo test --manifest-path vendor/librqbit-9.0.1/Cargo.toml --lib
cargo test --locked && cargo build --release --locked
chmod +x nananeas.sh
./nananeas.sh
```

Si no existe Cargo.lock al preparar el primer commit, créalo antes con cargo generate-lockfile e inclúyelo en Git. No fija la versión del compilador. No hemos establecido una versión mínima de Rust; registra rustc -Vv con los resultados de compilación.

El lanzador funciona desde cualquier carpeta y ajusta únicamente el límite blando de archivos abiertos heredado por la aplicación. Usa 32768 por defecto: es un valor práctico tras el fallo con 1024 descriptores y torrents grandes, no un requisito universal medido. Varias descargas simultáneas podrían necesitar más. No reduce un límite que ya sea superior.

```bash
NANANEAS_NOFILE=65536 ./nananeas.sh
```

Si el límite duro no permite alcanzar el valor solicitado, muestra un error. No modifica permanentemente los límites del sistema.

Diagnóstico opcional, que puede generar registros grandes:

```bash
NANANEAS_DIAGNOSTIC=1 ./nananeas.sh 2>&1 | tee diagnostico.log
```

## Compilar nativamente en Windows

La versión Windows todavía no está validada. Instala Rust para x86_64-pc-windows-msvc y Visual Studio Build Tools con Desarrollo para el escritorio con C++ y un Windows SDK. Abre Developer PowerShell for Visual Studio. El lanzador Bash es solamente para Linux.

```powershell
rustup target add x86_64-pc-windows-msvc
cargo test --manifest-path vendor/librqbit-9.0.1/Cargo.toml --lib
cargo test --locked
if ($LASTEXITCODE -ne 0) { throw "Tests failed" }
cargo build --release --locked
if ($LASTEXITCODE -ne 0) { throw "Build failed" }
.\target\release\torrent-finder.exe
```

build.rs incorpora assets/cat-icon.ico al ejecutable de Windows. Estas instrucciones son para compilar en Windows; compilar Windows desde Arch es otro flujo y no se ha validado aquí. Después hay que probar interfaz, selector de carpetas, indexado, descarga, seeding, mover y cierre.

## Datos y funcionamiento

En la primera apertura, la salida predeterminada es Downloads/Torrents dentro del usuario; se conserva una ruta configurada previamente. Por compatibilidad, los datos siguen usando el nombre torrent-finder. En Linux: catálogo en ~/.local/share/torrent-finder, caché en ~/.cache/torrent-finder y traducciones en ~/.config/torrent-finder/translations. En Windows: catálogo en el directorio local de datos de aplicaciones y traducciones en %APPDATA%\torrent-finder\userdata\translations.

Al cerrar Configuración se procesan las fuentes pendientes, omitiendo las ya indexadas. El timeout de resolución del magnet es de 120 segundos. La recuperación de metadatos no tiene un porcentaje fiable en la interfaz; el guardado del índice sí muestra progreso. Los errores de fuente se conservan durante la sesión actual. Ver [cambios del motor](docs/ENGINE_PATCHES.md).

## Validación y límites

Comprobado por el usuario en Arch: indexado de los magnets grandes que fallaban y descarga de archivos seleccionados. Las pruebas del búfer dieron siete correctas y una Miri omitida según su configuración; también pasó la prueba de búsqueda. Los cambios posteriores del protocolo se verificaron mediante indexación real por el usuario. Windows sigue sin validar. No se garantiza todo torrent ni todo caso límite.

Esta versión no reanuda descargas interrumpidas ni el seeding una vez detenido. No tiene cifras separadas de seeders y leechers. El seeding necesita mantener la aplicación abierta. El diagnóstico es opcional. Revisa los archivos preparados antes de publicar.

## Fuentes y licencias

- https://github.com/ikatson/rqbit
- https://github.com/emilk/egui
- https://www.rust-lang.org/tools/install
- https://doc.rust-lang.org/rustc/platform-support/windows-msvc.html

El motor local usa Apache-2.0: conserva sus archivos LICENSE/NOTICE y la descripción de modificaciones en docs/ENGINE_PATCHES.md. Todavía no se ha elegido una licencia para el código original de Nananeas; esta estructura no asigna una.
