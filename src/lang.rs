//! Programming-language classification by file extension, with GitHub-ish colors,
//! for the HUD language split. Extensions are mapped to a stable language index
//! once per repo (path_id -> lang id) so per-keyframe aggregation is just an
//! array sum.

pub struct Lang {
    pub name: &'static str,
    pub color: [u8; 3],
}

/// Index 0 is always "Other". Keep this array in sync with `lang_index`.
pub static LANGS: &[Lang] = &[
    Lang {
        name: "Other",
        color: [140, 142, 150],
    },
    Lang {
        name: "Go",
        color: [0, 173, 216],
    },
    Lang {
        name: "TypeScript",
        color: [70, 130, 210],
    },
    Lang {
        name: "JavaScript",
        color: [241, 224, 90],
    },
    Lang {
        name: "Python",
        color: [90, 150, 210],
    },
    Lang {
        name: "Rust",
        color: [222, 165, 132],
    },
    Lang {
        name: "Java",
        color: [200, 130, 60],
    },
    Lang {
        name: "C",
        color: [160, 160, 170],
    },
    Lang {
        name: "C++",
        color: [243, 75, 125],
    },
    Lang {
        name: "C#",
        color: [70, 180, 90],
    },
    Lang {
        name: "Ruby",
        color: [214, 70, 62],
    },
    Lang {
        name: "PHP",
        color: [120, 130, 180],
    },
    Lang {
        name: "HTML",
        color: [227, 100, 60],
    },
    Lang {
        name: "CSS",
        color: [150, 110, 200],
    },
    Lang {
        name: "Vue",
        color: [65, 184, 131],
    },
    Lang {
        name: "Shell",
        color: [137, 224, 81],
    },
    Lang {
        name: "SQL",
        color: [227, 150, 40],
    },
    Lang {
        name: "Markdown",
        color: [120, 140, 180],
    },
    Lang {
        name: "JSON",
        color: [180, 160, 60],
    },
    Lang {
        name: "YAML",
        color: [203, 120, 90],
    },
    Lang {
        name: "TOML",
        color: [180, 110, 90],
    },
    Lang {
        name: "Kotlin",
        color: [169, 123, 255],
    },
    Lang {
        name: "Swift",
        color: [240, 90, 60],
    },
    Lang {
        name: "Dart",
        color: [0, 180, 171],
    },
    Lang {
        name: "XML",
        color: [130, 170, 120],
    },
    Lang {
        name: "Protobuf",
        color: [150, 150, 160],
    },
    Lang {
        name: "Scala",
        color: [200, 60, 70],
    },
    Lang {
        name: "Objective-C",
        color: [110, 130, 220],
    },
];

pub const OTHER: u16 = 0;

/// Map a lowercase file extension (without the dot) to a `LANGS` index.
pub fn lang_index(ext: &str) -> u16 {
    match ext {
        "go" => 1,
        "ts" | "tsx" | "mts" | "cts" => 2,
        "js" | "jsx" | "mjs" | "cjs" => 3,
        "py" | "pyi" | "pyw" => 4,
        "rs" => 5,
        "java" => 6,
        "c" | "h" => 7,
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => 8,
        "cs" => 9,
        "rb" | "rake" | "gemspec" => 10,
        "php" => 11,
        "html" | "htm" => 12,
        "css" | "scss" | "sass" | "less" => 13,
        "vue" => 14,
        "sh" | "bash" | "zsh" | "fish" => 15,
        "sql" => 16,
        "md" | "markdown" | "mdx" => 17,
        "json" | "json5" => 18,
        "yml" | "yaml" => 19,
        "toml" => 20,
        "kt" | "kts" => 21,
        "swift" => 22,
        "dart" => 23,
        "xml" | "xsd" | "xsl" => 24,
        "proto" => 25,
        "scala" | "sc" => 26,
        "m" | "mm" => 27,
        _ => OTHER,
    }
}

/// Precompute path_id -> language id for the whole repo (done once).
pub fn build_path_lang(paths: &[String]) -> Vec<u16> {
    paths
        .iter()
        .map(|p| {
            let name = p.rsplit('/').next().unwrap_or(p);
            match name.rsplit_once('.') {
                Some((stem, ext)) if !stem.is_empty() => lang_index(&ext.to_ascii_lowercase()),
                _ => OTHER,
            }
        })
        .collect()
}

/// Top `n` languages (excluding empty) sorted by lines descending, carrying the
/// file count per language: (lang_id, lines, files).
/// (lang_id, lines, files), sorted by lines descending.
pub fn top_langs_fl(lang_lines: &[u64], lang_files: &[u32], n: usize) -> Vec<(u16, u64, u32)> {
    let mut v: Vec<(u16, u64, u32)> = lang_lines
        .iter()
        .enumerate()
        .filter(|(_, l)| **l > 0)
        .map(|(i, l)| (i as u16, *l, lang_files.get(i).copied().unwrap_or(0)))
        .collect();
    v.sort_unstable_by_key(|x| std::cmp::Reverse(x.1));
    v.truncate(n);
    v
}
