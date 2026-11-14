fn main() {
    let cfg = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/main.slint", cfg).unwrap();

    // Icono del .exe (el que se ve en el Explorador). Si el compilador de recursos de Windows
    // no está disponible, la compilación sigue: solo se pierde el icono del archivo.
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Pulso");
        res.set("FileDescription", "Pulso - reproductor de musica");
        if let Err(e) = res.compile() {
            println!("cargo:warning=no se pudo incrustar el icono: {e}");
        }
    }
}
