//! Núcleo de Pulso: no depende de ninguna interfaz gráfica.
pub mod library;
pub mod lyrics;
pub mod paths;
pub mod player;
pub mod settings;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
