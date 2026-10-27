//! The grammars the viewer highlights with: the same set, names, aliases and queries as
//! gpui-kit's `tree-sitter-languages`, so every file colors as it did. Queries not shipped with
//! a grammar crate are the kit's, in `languages/` under its Apache-2.0 license.

pub struct Language {
    /// The canonical name, which injections and fold sharing go by.
    pub name: &'static str,
    pub grammar: tree_sitter::Language,
    pub highlights: &'static str,
    pub injections: &'static str,
    pub locals: &'static str,
}

/// The canonical name for a language name or file extension, as the kit resolved them.
fn canonical(name: &str) -> Option<&'static str> {
    Some(match name {
        "json" | "jsonc" => "json",
        "astro" => "astro",
        "bash" | "sh" => "bash",
        "c" => "c",
        "cmake" => "cmake",
        "cpp" | "c++" => "cpp",
        "csharp" | "cs" => "csharp",
        "css" | "scss" => "css",
        "diff" => "diff",
        "ejs" => "ejs",
        "elixir" | "ex" => "elixir",
        "erb" => "erb",
        "go" => "go",
        "graphql" => "graphql",
        "html" => "html",
        "java" => "java",
        "javascript" | "js" => "javascript",
        "jsdoc" => "jsdoc",
        "kt" | "kts" | "ktm" | "kotlin" => "kotlin",
        "lua" => "lua",
        "make" | "makefile" => "make",
        "markdown" | "md" | "mdx" => "markdown",
        "markdown_inline" | "markdown-inline" => "markdown_inline",
        "php" | "php3" | "php4" | "php5" | "phtml" => "php",
        "proto" | "protobuf" => "proto",
        "python" | "py" => "python",
        "ruby" | "rb" => "ruby",
        "rust" | "rs" => "rust",
        "scala" => "scala",
        "sql" => "sql",
        "svelte" => "svelte",
        "swift" => "swift",
        "toml" => "toml",
        "tsx" => "tsx",
        "typescript" | "ts" => "typescript",
        "yaml" | "yml" => "yaml",
        "zig" => "zig",
        _ => return None,
    })
}

/// The language for a name or file extension, or `None` for plain text.
pub fn find(name: &str) -> Option<Language> {
    let name = canonical(name)?;
    let (grammar, highlights, injections, locals): (tree_sitter::Language, &str, &str, &str) = match name {
        "json" => (tree_sitter_json::LANGUAGE.into(), include_str!("languages/json/highlights.scm"), "", ""),
        "markdown" => (
            tree_sitter_md::LANGUAGE.into(),
            include_str!("languages/markdown/highlights.scm"),
            include_str!("languages/markdown/injections.scm"),
            "",
        ),
        "markdown_inline" => {
            (tree_sitter_md::INLINE_LANGUAGE.into(), include_str!("languages/markdown_inline/highlights.scm"), "", "")
        }
        "toml" => (tree_sitter_toml_ng::LANGUAGE.into(), tree_sitter_toml_ng::HIGHLIGHTS_QUERY, "", ""),
        "yaml" => (tree_sitter_yaml::LANGUAGE.into(), tree_sitter_yaml::HIGHLIGHTS_QUERY, "", ""),
        "rust" => (
            tree_sitter_rust::LANGUAGE.into(),
            include_str!("languages/rust/highlights.scm"),
            include_str!("languages/rust/injections.scm"),
            "",
        ),
        "go" => (tree_sitter_go::LANGUAGE.into(), include_str!("languages/go/highlights.scm"), "", ""),
        "c" => (tree_sitter_c::LANGUAGE.into(), tree_sitter_c::HIGHLIGHT_QUERY, "", ""),
        "cpp" => (tree_sitter_cpp::LANGUAGE.into(), tree_sitter_cpp::HIGHLIGHT_QUERY, "", ""),
        "javascript" => (
            tree_sitter_javascript::LANGUAGE.into(),
            include_str!("languages/javascript/highlights.scm"),
            include_str!("languages/javascript/injections.scm"),
            tree_sitter_javascript::LOCALS_QUERY,
        ),
        "jsdoc" => (tree_sitter_jsdoc::LANGUAGE.into(), tree_sitter_jsdoc::HIGHLIGHTS_QUERY, "", ""),
        "zig" => (
            tree_sitter_zig::LANGUAGE.into(),
            include_str!("languages/zig/highlights.scm"),
            include_str!("languages/zig/injections.scm"),
            "",
        ),
        "java" => (tree_sitter_java::LANGUAGE.into(), tree_sitter_java::HIGHLIGHTS_QUERY, "", ""),
        "python" => (tree_sitter_python::LANGUAGE.into(), tree_sitter_python::HIGHLIGHTS_QUERY, "", ""),
        "ruby" => {
            (tree_sitter_ruby::LANGUAGE.into(), tree_sitter_ruby::HIGHLIGHTS_QUERY, "", tree_sitter_ruby::LOCALS_QUERY)
        }
        "bash" => (tree_sitter_bash::LANGUAGE.into(), tree_sitter_bash::HIGHLIGHT_QUERY, "", ""),
        "html" => (
            tree_sitter_html::LANGUAGE.into(),
            include_str!("languages/html/highlights.scm"),
            include_str!("languages/html/injections.scm"),
            "",
        ),
        "css" => (tree_sitter_css::LANGUAGE.into(), tree_sitter_css::HIGHLIGHTS_QUERY, "", ""),
        "swift" => (tree_sitter_swift::LANGUAGE.into(), "", "", ""),
        "scala" => (
            tree_sitter_scala::LANGUAGE.into(),
            tree_sitter_scala::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_scala::LOCALS_QUERY,
        ),
        "sql" => (tree_sitter_sequel::LANGUAGE.into(), tree_sitter_sequel::HIGHLIGHTS_QUERY, "", ""),
        "csharp" => (tree_sitter_c_sharp::LANGUAGE.into(), "", "", ""),
        "graphql" => (tree_sitter_graphql::LANGUAGE.into(), "", "", ""),
        "proto" => (tree_sitter_proto::LANGUAGE.into(), "", "", ""),
        "make" => (tree_sitter_make::LANGUAGE.into(), tree_sitter_make::HIGHLIGHTS_QUERY, "", ""),
        "cmake" => (tree_sitter_cmake::LANGUAGE.into(), "", "", ""),
        "typescript" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            include_str!("languages/typescript/highlights.scm"),
            include_str!("languages/javascript/injections.scm"),
            tree_sitter_typescript::LOCALS_QUERY,
        ),
        "tsx" => (
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_typescript::LOCALS_QUERY,
        ),
        "diff" => (tree_sitter_diff::LANGUAGE.into(), tree_sitter_diff::HIGHLIGHTS_QUERY, "", ""),
        "elixir" => (
            tree_sitter_elixir::LANGUAGE.into(),
            tree_sitter_elixir::HIGHLIGHTS_QUERY,
            tree_sitter_elixir::INJECTIONS_QUERY,
            "",
        ),
        "erb" | "ejs" => (
            tree_sitter_embedded_template::LANGUAGE.into(),
            tree_sitter_embedded_template::HIGHLIGHTS_QUERY,
            tree_sitter_embedded_template::INJECTIONS_EJS_QUERY,
            "",
        ),
        "php" => (
            tree_sitter_php::LANGUAGE_PHP.into(),
            tree_sitter_php::HIGHLIGHTS_QUERY,
            include_str!("languages/php/injections.scm"),
            "",
        ),
        "astro" => (
            tree_sitter_astro_next::LANGUAGE.into(),
            tree_sitter_astro_next::HIGHLIGHTS_QUERY,
            tree_sitter_astro_next::INJECTIONS_QUERY,
            "",
        ),
        "kotlin" => (tree_sitter_kotlin_sg::LANGUAGE.into(), include_str!("languages/kotlin/highlights.scm"), "", ""),
        "lua" => (
            tree_sitter_lua::LANGUAGE.into(),
            include_str!("languages/lua/highlights.scm"),
            tree_sitter_lua::INJECTIONS_QUERY,
            tree_sitter_lua::LOCALS_QUERY,
        ),
        "svelte" => (
            tree_sitter_svelte_next::LANGUAGE.into(),
            tree_sitter_svelte_next::HIGHLIGHTS_QUERY,
            tree_sitter_svelte_next::INJECTIONS_QUERY,
            tree_sitter_svelte_next::LOCALS_QUERY,
        ),
        _ => return None,
    };
    Some(Language { name, grammar, highlights, injections, locals })
}

#[cfg(test)]
mod tests {
    use super::find;

    #[test]
    fn aliases_resolve_like_the_kit() {
        assert_eq!(find("rs").unwrap().name, "rust");
        assert_eq!(find("ex").unwrap().name, "elixir");
        assert_eq!(find("yml").unwrap().name, "yaml");
        assert!(find("text").is_none());
        assert!(find("exs").is_none());
    }

    #[test]
    fn every_language_builds_its_queries() {
        let names = [
            "json", "astro", "bash", "c", "cmake", "cpp", "csharp", "css", "diff", "ejs", "elixir", "erb", "go",
            "graphql", "html", "java", "javascript", "jsdoc", "kotlin", "lua", "make", "markdown", "markdown_inline",
            "php", "proto", "python", "ruby", "rust", "scala", "sql", "svelte", "swift", "toml", "tsx", "typescript",
            "yaml", "zig",
        ];
        for name in names {
            let language = find(name).unwrap_or_else(|| panic!("{name} missing"));
            let source = format!("{}{}{}", language.injections, language.locals, language.highlights);
            tree_sitter::Query::new(&language.grammar, &source).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
}
