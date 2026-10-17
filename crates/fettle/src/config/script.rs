use std::path::Path;

use oxc::allocator::Allocator;
use oxc::codegen::{Codegen, CodegenOptions};
use oxc::diagnostics::OxcDiagnostic;
use oxc::parser::Parser;
use oxc::semantic::SemanticBuilder;
use oxc::span::{LabeledSpan, SourceType};
use oxc::transformer::{TransformOptions, Transformer};

use super::error::{Error, ErrorKind, Location, SourceMap};

/// The only module `fettle.ts` can import.
pub(crate) const FETTLE_MODULE: &str = "fettle";

pub(crate) struct Script {
    pub(crate) code: String,
    pub(crate) map: SourceMap,
}

/// Strips the types from `source`, the text of the file at `path`, keeping a map back to it.
pub(crate) fn strip(path: &Path, source: &str) -> Result<Script, Error> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::ts().with_module(true)).parse();
    let at = |offset: u32| Some(location(path, source, offset));
    if let Some(diagnostic) = parsed.diagnostics.first() {
        return Err(diagnostic_error(path, source, diagnostic));
    }
    let static_imports = parsed
        .module_record
        .requested_modules
        .iter()
        .filter(|(specifier, _)| specifier.as_str() != FETTLE_MODULE)
        .flat_map(|(specifier, requests)| {
            requests.iter().map(move |r| {
                (
                    r.span.start,
                    format!(
                        "cannot import \"{specifier}\": fettle.ts can only import \"{FETTLE_MODULE}\""
                    ),
                )
            })
        });
    let dynamic_imports = parsed.module_record.dynamic_imports.iter().map(|import| {
        (
            import.span.start,
            format!(
                "cannot use import(): fettle.ts can only import \"{FETTLE_MODULE}\", statically"
            ),
        )
    });
    if let Some((offset, message)) = static_imports
        .chain(dynamic_imports)
        .min_by_key(|(offset, _)| *offset)
    {
        return Err(Error::new(ErrorKind::Syntax, at(offset), message));
    }

    let mut program = parsed.program;
    let semantic = SemanticBuilder::new()
        .with_check_syntax_error(true)
        .build(&program);
    if let Some(diagnostic) = semantic.diagnostics.first() {
        return Err(diagnostic_error(path, source, diagnostic));
    }
    let transformed = Transformer::new(&allocator, path, &TransformOptions::default())
        .build_with_scoping(semantic.semantic.into_scoping(), &mut program);
    if let Some(diagnostic) = transformed.diagnostics.first() {
        return Err(diagnostic_error(path, source, diagnostic));
    }

    let generated = Codegen::new()
        .with_options(CodegenOptions {
            source_map_path: Some(path.to_path_buf()),
            ..CodegenOptions::default()
        })
        .build(&program);
    let tokens = generated
        .map
        .map(|map| {
            map.get_tokens()
                .map(|t| {
                    (
                        t.get_dst_line(),
                        t.get_dst_col(),
                        t.get_src_line(),
                        t.get_src_col(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Script {
        code: generated.code,
        map: SourceMap::new(tokens),
    })
}

fn diagnostic_error(path: &Path, source: &str, diagnostic: &OxcDiagnostic) -> Error {
    // The earliest label is where an unclosed construct opened, not where the parser gave up.
    let location = diagnostic
        .labels
        .iter()
        .map(LabeledSpan::offset)
        .min()
        .map(|offset| location(path, source, offset));
    Error::new(ErrorKind::Syntax, location, diagnostic.message.to_string())
}

fn location(path: &Path, source: &str, offset: u32) -> Location {
    let before = &source[..(offset as usize).min(source.len())];
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
    Location {
        file: path.to_path_buf(),
        line: count(before.matches('\n').count()) + 1,
        column: count(before[line_start..].encode_utf16().count()) + 1,
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_source(source: &str) -> Result<Script, Error> {
        strip(Path::new("fettle.ts"), source)
    }

    fn error_line(source: &str) -> u32 {
        let error = strip_source(source).err().expect("strip should fail");
        error.location.expect("error should have a location").line
    }

    #[test]
    fn strips_types_to_runnable_code() {
        let script = strip_source(
            "import { pkg } from \"fettle\";\ninterface Role { name: string }\nconst r: Role = { name: \"caddy\" } as Role;\npkg(r.name as string);\n",
        )
        .unwrap();
        assert!(!script.code.contains("interface"));
        assert!(!script.code.contains(" as "));
        assert!(script.code.contains("pkg(r.name)"));

        let runtime = rquickjs::Runtime::new().unwrap();
        let context = rquickjs::Context::full(&runtime).unwrap();
        let code = script.code.replace("import { pkg } from \"fettle\";", "");
        let name: String = context
            .with(|ctx| ctx.eval(format!("let pkg = (n) => n;\n{code}\nr.name")))
            .unwrap();
        assert_eq!(name, "caddy");
    }

    #[test]
    fn syntax_error_names_its_line() {
        assert_eq!(
            error_line(
                "import { pkg } from \"fettle\";\n\nconst a = 1;\npkg(\"htop\"\nconst b = 2;\n"
            ),
            4
        );
    }

    #[test]
    fn rejects_imports_other_than_fettle() {
        let error = strip_source("import { pkg } from \"fettle\";\nimport x from \"./roles\";\n")
            .err()
            .unwrap();
        assert!(error.message.contains("./roles"), "{error}");
        assert_eq!(error.location.unwrap().line, 2);
    }

    #[test]
    fn syntax_error_column_counts_utf16_units() {
        let error = strip_source("const s = \"😀\"; const = 1;\n")
            .err()
            .unwrap();
        assert_eq!(error.location.unwrap().column, 23);
    }

    #[test]
    fn rejects_dynamic_imports() {
        let error = strip_source("import { host } from \"fettle\";\nimport(\"./roles\");\n")
            .err()
            .unwrap();
        assert!(error.message.contains("import()"), "{error}");
        assert_eq!(error.location.unwrap().line, 2);
    }
}
