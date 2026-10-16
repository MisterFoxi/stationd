//! Bibliothèque d'exemplaires de souffles (patchs log-mel bande >=2 kHz,
//! centrés, normalisés L2). Sérialisée en JSON, autonome.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize)]
pub struct Exemplar {
    pub source: String,
    pub center_s: f64,
    #[serde(default)]
    pub note: String,
    pub vector: Vec<f64>, // bande>=2k x W, centré + normé L2
}

#[derive(Serialize, Deserialize)]
pub struct Library {
    pub version: u32,
    pub dim: usize,
    pub exemplars: Vec<Exemplar>,
}

impl Library {
    pub fn load(path: &Path) -> std::io::Result<Library> {
        let s = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&s).expect("JSON biblio invalide"))
    }
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap())
    }
    /// Matrice (n_exemplaires x dim) pour le produit scalaire.
    pub fn vectors(&self) -> &Vec<Exemplar> { &self.exemplars }
}
