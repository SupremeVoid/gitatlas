//! Simple string interning for paths and author identities. Keeps the in-memory
//! history compact (u32 ids instead of repeated strings) and lets us precompute
//! per-author avatars once.

use std::collections::HashMap;

#[derive(Default)]
pub struct Interner {
    map: HashMap<String, u32>,
    names: Vec<String>,
}

impl Interner {
    pub fn new() -> Self {
        Interner::default()
    }

    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = self.names.len() as u32;
        self.names.push(s.to_string());
        self.map.insert(s.to_string(), id);
        id
    }

    pub fn get(&self, id: u32) -> &str {
        &self.names[id as usize]
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Consume the interner, returning just the id->string table.
    pub fn into_names(self) -> Vec<String> {
        self.names
    }
}
