//! Fast in-memory TypeScript/TSX compiler and incremental module graph powered by Oxc.
use jsnir::host::DevError;
use oxc::{
    allocator::Allocator,
    ast::ast::{
        BindingPattern, Declaration, ExportDefaultDeclarationKind, Expression,
        ImportDeclarationSpecifier, ModuleExportName, Statement, VariableDeclarator,
    },
    ast_visit::{Visit, walk},
    codegen::Codegen,
    parser::Parser,
    semantic::SemanticBuilder,
    span::{GetSpan, SourceType},
    transformer::{JsxOptions, JsxRuntime, TransformOptions, Transformer},
};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::OnceLock,
    time::{Duration, Instant, SystemTime},
};

/// Embedded JSNIR TSX SDK source (`app/sdk.ts`).
pub const SDK_TS: &str = include_str!("../../../app/sdk.ts");
/// Embedded JSNIR TypeScript contract declarations (`app/jsnir.d.ts`).
pub const JSNIR_DTS: &str = include_str!("../../../app/jsnir.d.ts");

static COMPILED_SDK: OnceLock<String> = OnceLock::new();

/// Returns the JSNIR SDK compiled once in memory with Oxc.
pub fn compiled_sdk() -> Result<&'static str, DevError> {
    if let Some(code) = COMPILED_SDK.get() {
        return Ok(code.as_str());
    }
    let code = transpile_ts(Path::new("sdk.ts"), SDK_TS)?;
    let _ = COMPILED_SDK.set(code);
    Ok(COMPILED_SDK.get().expect("sdk cached").as_str())
}

/// Output of compiling an entry file and all its local module dependencies.
#[derive(Debug, Clone)]
pub struct CompiledBundle {
    /// Complete standalone JS script (SDK + app) ready for `Host::try_new` or `Host::reload`.
    pub code: String,
    /// All source files belonging to this bundle's module graph.
    pub files: Vec<PathBuf>,
    /// Wall-clock duration of this build.
    pub elapsed: Duration,
    /// Number of modules freshly parsed/transformed in this build (rest came from cache).
    pub rebuilt: usize,
}

#[derive(Debug, Clone)]
struct CachedModule {
    id: String,
    mtime: Option<SystemTime>,
    len: u64,
    deps: Vec<PathBuf>,
    has_module_syntax: bool,
    code: String,
}

/// Incremental in-memory module graph inspired by Vite's `ModuleGraph`.
/// Only files that changed on disk (or were invalidated) are re-parsed and transformed.
#[derive(Debug, Default)]
pub struct ModuleGraph {
    root: PathBuf,
    cache: HashMap<PathBuf, CachedModule>,
}

impl ModuleGraph {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: normalize_path(&root.into()),
            cache: HashMap::new(),
        }
    }

    pub fn set_root(&mut self, root: impl Into<PathBuf>) {
        self.root = normalize_path(&root.into());
        self.cache.clear();
    }

    /// Invalidates a single file in the graph so the next `build` recompiles only it.
    pub fn invalidate(&mut self, path: &Path) {
        let key = canonical_or_normalized(path);
        self.cache.remove(&key);
    }

    /// Clears all cached modules.
    pub fn invalidate_all(&mut self) {
        self.cache.clear();
    }

    /// Returns true if `path` is currently part of the compiled graph.
    pub fn contains(&self, path: &Path) -> bool {
        self.cache.contains_key(&canonical_or_normalized(path))
    }

    /// Compiles `entry` and its transitive local imports into a standalone QuickJS bundle.
    pub fn build(&mut self, entry: &Path) -> Result<CompiledBundle, DevError> {
        let started = Instant::now();
        let sdk = compiled_sdk()?;
        let entry_key = canonical_or_normalized(entry);
        let mut order = Vec::new();
        let mut visited = HashSet::new();
        let mut rebuilt = 0usize;

        self.visit(&entry_key, &mut visited, &mut order, &mut rebuilt)?;

        let multi_module = order.len() > 1
            || self
                .cache
                .get(&entry_key)
                .is_some_and(|m| m.has_module_syntax);

        let code = if !multi_module {
            let entry_mod = &self.cache[&entry_key];
            let mut out = String::with_capacity(sdk.len() + entry_mod.code.len() + 2);
            out.push_str(sdk);
            out.push('\n');
            out.push_str(&entry_mod.code);
            out
        } else {
            let entry_id = &self.cache[&entry_key].id;
            let mut out = String::with_capacity(sdk.len() + order.len() * 1024 + 512);
            out.push_str(sdk);
            out.push_str("\n(function(){\nvar __jsnir_m = {\n");
            for path in &order {
                let m = &self.cache[path];
                let id_json = serde_json::to_string(&m.id).unwrap();
                out.push_str(&id_json);
                out.push_str(": function(__jsnir_e, __jsnir_r) {\n");
                out.push_str(&m.code);
                out.push_str("\n},\n");
            }
            out.push_str("};\nvar __jsnir_c = {};\nfunction __jsnir_r(id) {\n");
            out.push_str("  if (__jsnir_c[id]) return __jsnir_c[id].exports;\n");
            out.push_str("  var mod = { exports: {} };\n");
            out.push_str("  __jsnir_c[id] = mod;\n");
            out.push_str("  var fn = __jsnir_m[id];\n");
            out.push_str("  if (!fn) throw new Error('Unknown module: ' + id);\n");
            out.push_str("  fn(mod.exports, __jsnir_r);\n");
            out.push_str("  return mod.exports;\n");
            out.push_str("}\n__jsnir_r(");
            out.push_str(&serde_json::to_string(entry_id).unwrap());
            out.push_str(");\n})();\n");
            out
        };

        Ok(CompiledBundle {
            code,
            files: order,
            elapsed: started.elapsed(),
            rebuilt,
        })
    }

    fn visit(
        &mut self,
        path: &Path,
        visited: &mut HashSet<PathBuf>,
        order: &mut Vec<PathBuf>,
        rebuilt: &mut usize,
    ) -> Result<(), DevError> {
        if !visited.insert(path.to_path_buf()) {
            return Ok(());
        }
        let (mtime, len) = file_stamp(path)?;
        let fresh = self
            .cache
            .get(path)
            .is_some_and(|m| m.mtime == mtime && m.len == len);
        if !fresh {
            let source = fs::read_to_string(path).map_err(|e| DevError {
                title: "Read error".into(),
                message: format!("Could not read {}: {e}", self.display_path(path)),
                location: Some(self.display_path(path)),
                frame: None,
            })?;
            let id = self.module_id(path);
            let compiled = compile_user_module(&self.root, path, &id, &source)?;
            self.cache.insert(
                path.to_path_buf(),
                CachedModule {
                    id,
                    mtime,
                    len,
                    deps: compiled.deps,
                    has_module_syntax: compiled.has_module_syntax,
                    code: compiled.code,
                },
            );
            *rebuilt += 1;
        }
        let deps = self.cache[path].deps.clone();
        for dep in deps {
            self.visit(&dep, visited, order, rebuilt)?;
        }
        order.push(path.to_path_buf());
        Ok(())
    }

    fn module_id(&self, path: &Path) -> String {
        module_id_for(&self.root, path)
    }

    fn display_path(&self, path: &Path) -> String {
        module_id_for(&self.root, path)
    }
}

struct CompiledModule {
    deps: Vec<PathBuf>,
    has_module_syntax: bool,
    code: String,
}

/// Convenience function for one-shot compilation (used by `jsnir build` and embedded hosts).
pub fn compile_entry(root: &Path, entry: &Path) -> Result<CompiledBundle, DevError> {
    let mut graph = ModuleGraph::new(root);
    graph.build(entry)
}

fn transpile_ts(path: &Path, source: &str) -> Result<String, DevError> {
    let allocator = Allocator::default();
    let source_type = source_type_for(path);
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        return Err(format_oxc_error(
            "Syntax error",
            path,
            source,
            &parsed.diagnostics,
        ));
    }
    let mut program = parsed.program;
    let semantic = SemanticBuilder::new().build(&program);
    if !semantic.diagnostics.is_empty() {
        return Err(format_oxc_error(
            "Semantic error",
            path,
            source,
            &semantic.diagnostics,
        ));
    }
    let scoping = semantic.semantic.into_scoping();
    let mut options = TransformOptions::default();
    options.jsx = JsxOptions {
        runtime: JsxRuntime::Classic,
        pragma: Some("h".into()),
        pragma_frag: Some("Fragment".into()),
        ..Default::default()
    };
    let transformed =
        Transformer::new(&allocator, path, &options).build_with_scoping(scoping, &mut program);
    if !transformed.diagnostics.is_empty() {
        return Err(format_oxc_error(
            "Transform error",
            path,
            source,
            &transformed.diagnostics,
        ));
    }
    Ok(Codegen::new().build(&program).code)
}

fn compile_user_module(
    root: &Path,
    path: &Path,
    module_id: &str,
    source: &str,
) -> Result<CompiledModule, DevError> {
    let js = transpile_ts(path, source)?;
    rewrite_module(root, path, module_id, &js)
}

struct Edit {
    start: usize,
    end: usize,
    replacement: String,
}

fn rewrite_module(
    root: &Path,
    path: &Path,
    module_id: &str,
    js: &str,
) -> Result<CompiledModule, DevError> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, js, SourceType::mjs()).parse();
    if !parsed.diagnostics.is_empty() {
        return Err(format_oxc_error(
            "Internal codegen error",
            path,
            js,
            &parsed.diagnostics,
        ));
    }
    let program = &parsed.program;
    let mut edits: Vec<Edit> = Vec::new();
    let mut deps: Vec<PathBuf> = Vec::new();
    let mut has_module_syntax = false;
    let mut tmp_counter = 0usize;

    // 1. Inject stable reload keys into `var/let/const name = signal(init)` / `list(init?)`.
    let mut signal_keys = SignalKeyVisitor {
        module_id,
        edits: &mut edits,
    };
    signal_keys.visit_program(program);

    // 2. Rewrite top-level ES module imports/exports into `__jsnir_r` / `__jsnir_e`.
    let mut hoisted_exports: Vec<String> = Vec::new();
    for stmt in &program.body {
        match stmt {
            Statement::ImportDeclaration(import) => {
                has_module_syntax = true;
                let spec = import.source.value.as_str().unwrap_or("");
                let target_expr = if is_sdk_specifier(spec) {
                    String::from("globalThis")
                } else {
                    let resolved = resolve_import(root, path, spec, js, import.span.start)?;
                    let dep_id = module_id_for(root, &resolved);
                    if !deps.contains(&resolved) {
                        deps.push(resolved);
                    }
                    format!("__jsnir_r({})", serde_json::to_string(&dep_id).unwrap())
                };
                let replacement = match &import.specifiers {
                    None => {
                        if is_sdk_specifier(spec) {
                            String::new()
                        } else {
                            format!("{target_expr};")
                        }
                    }
                    Some(specs) if specs.is_empty() => {
                        if is_sdk_specifier(spec) {
                            String::new()
                        } else {
                            format!("{target_expr};")
                        }
                    }
                    Some(specs) => {
                        let tmp = format!("__jsnir_i{}", tmp_counter);
                        tmp_counter += 1;
                        let mut lines = vec![format!("const {tmp} = {target_expr};")];
                        let mut named = Vec::new();
                        for s in specs {
                            match s {
                                ImportDeclarationSpecifier::ImportDefaultSpecifier(d) => {
                                    let local = d.local.name.as_str();
                                    if is_sdk_specifier(spec) {
                                        lines.push(format!("const {local} = {tmp};"));
                                    } else {
                                        lines.push(format!(
                                            "const {local} = ({tmp} && 'default' in {tmp}) ? {tmp}.default : {tmp};"
                                        ));
                                    }
                                }
                                ImportDeclarationSpecifier::ImportNamespaceSpecifier(n) => {
                                    lines.push(format!("const {} = {tmp};", n.local.name.as_str()));
                                }
                                ImportDeclarationSpecifier::ImportSpecifier(n) => {
                                    let imported = export_name(&n.imported);
                                    let local = n.local.name.as_str();
                                    if imported == local {
                                        named.push(local.to_string());
                                    } else {
                                        named.push(format!("{imported}: {local}"));
                                    }
                                }
                            }
                        }
                        if !named.is_empty() {
                            lines.push(format!("const {{ {} }} = {tmp};", named.join(", ")));
                        }
                        lines.join("\n")
                    }
                };
                edits.push(Edit {
                    start: import.span.start as usize,
                    end: import.span.end as usize,
                    replacement,
                });
            }
            Statement::ExportDeclaration(export) => {
                has_module_syntax = true;
                let decl = &export.declaration;
                let decl_start = decl.span().start as usize;
                edits.push(Edit {
                    start: export.span.start as usize,
                    end: decl_start,
                    replacement: String::new(),
                });
                let mut names = Vec::new();
                collect_decl_names(decl, &mut names);
                let assign: String = names
                    .iter()
                    .map(|n| format!("__jsnir_e.{n} = {n};"))
                    .collect::<Vec<_>>()
                    .join(" ");
                if matches!(decl, Declaration::FunctionDeclaration(_)) {
                    hoisted_exports.push(assign);
                } else if !assign.is_empty() {
                    edits.push(Edit {
                        start: export.span.end as usize,
                        end: export.span.end as usize,
                        replacement: format!("\n{assign}"),
                    });
                }
            }
            Statement::ExportFromDeclaration(export) => {
                has_module_syntax = true;
                let spec = export.source.value.as_str().unwrap_or("");
                let target_expr = if is_sdk_specifier(spec) {
                    String::from("globalThis")
                } else {
                    let resolved = resolve_import(root, path, spec, js, export.span.start)?;
                    let dep_id = module_id_for(root, &resolved);
                    if !deps.contains(&resolved) {
                        deps.push(resolved);
                    }
                    format!("__jsnir_r({})", serde_json::to_string(&dep_id).unwrap())
                };
                let tmp = format!("__jsnir_x{}", tmp_counter);
                tmp_counter += 1;
                let mut parts = vec![format!("const {tmp} = {target_expr};")];
                for s in &export.specifiers {
                    let local = export_name(&s.local);
                    let exported = export_name(&s.exported);
                    parts.push(format!("__jsnir_e.{exported} = {tmp}.{local};"));
                }
                edits.push(Edit {
                    start: export.span.start as usize,
                    end: export.span.end as usize,
                    replacement: format!("{{ {} }}", parts.join(" ")),
                });
            }
            Statement::ExportNamedDeclaration(export) => {
                has_module_syntax = true;
                let parts: Vec<String> = export
                    .specifiers
                    .iter()
                    .map(|s| {
                        format!(
                            "__jsnir_e.{} = {};",
                            export_name(&s.exported),
                            export_name(&s.local)
                        )
                    })
                    .collect();
                edits.push(Edit {
                    start: export.span.start as usize,
                    end: export.span.end as usize,
                    replacement: parts.join(" "),
                });
            }
            Statement::ExportDefaultDeclaration(export) => {
                has_module_syntax = true;
                match &export.declaration {
                    ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                        if let Some(id) = &f.id {
                            edits.push(Edit {
                                start: export.span.start as usize,
                                end: f.span.start as usize,
                                replacement: String::new(),
                            });
                            hoisted_exports
                                .push(format!("__jsnir_e.default = {};", id.name.as_str()));
                        } else {
                            edits.push(Edit {
                                start: export.span.start as usize,
                                end: f.span.start as usize,
                                replacement: String::from("__jsnir_e.default = "),
                            });
                        }
                    }
                    ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                        if let Some(id) = &c.id {
                            edits.push(Edit {
                                start: export.span.start as usize,
                                end: c.span.start as usize,
                                replacement: String::new(),
                            });
                            edits.push(Edit {
                                start: export.span.end as usize,
                                end: export.span.end as usize,
                                replacement: format!("\n__jsnir_e.default = {};", id.name.as_str()),
                            });
                        } else {
                            edits.push(Edit {
                                start: export.span.start as usize,
                                end: c.span.start as usize,
                                replacement: String::from("__jsnir_e.default = "),
                            });
                        }
                    }
                    other => {
                        let expr_start = other.span().start as usize;
                        edits.push(Edit {
                            start: export.span.start as usize,
                            end: expr_start,
                            replacement: String::from("__jsnir_e.default = "),
                        });
                    }
                }
            }
            Statement::ExportAllDeclaration(export) => {
                has_module_syntax = true;
                let spec = export.source.value.as_str().unwrap_or("");
                let resolved = resolve_import(root, path, spec, js, export.span.start)?;
                let dep_id = module_id_for(root, &resolved);
                if !deps.contains(&resolved) {
                    deps.push(resolved);
                }
                let req = format!("__jsnir_r({})", serde_json::to_string(&dep_id).unwrap());
                let replacement = match &export.exported {
                    Some(alias) => format!("__jsnir_e.{} = {req};", export_name(alias)),
                    None => format!("Object.assign(__jsnir_e, {req});"),
                };
                edits.push(Edit {
                    start: export.span.start as usize,
                    end: export.span.end as usize,
                    replacement,
                });
            }
            _ => {}
        }
    }

    let mut code = apply_edits(js, edits);
    if !hoisted_exports.is_empty() {
        code.insert_str(0, &(hoisted_exports.join("\n") + "\n"));
    }

    Ok(CompiledModule {
        deps,
        has_module_syntax,
        code,
    })
}

struct SignalKeyVisitor<'a> {
    module_id: &'a str,
    edits: &'a mut Vec<Edit>,
}

impl<'a> Visit<'_> for SignalKeyVisitor<'a> {
    fn visit_variable_declarator(&mut self, decl: &VariableDeclarator<'_>) {
        if let BindingPattern::BindingIdentifier(ident) = &decl.id
            && let Some(Expression::CallExpression(call)) = &decl.init
            && let Expression::Identifier(callee) = &call.callee
        {
            let name = callee.name.as_str();
            let key = serde_json::to_string(&format!("{}:{}", self.module_id, ident.name.as_str()))
                .unwrap();
            let close = (call.span.end as usize).saturating_sub(1);
            if name == "signal" && call.arguments.len() == 1 {
                self.edits.push(Edit {
                    start: close,
                    end: close,
                    replacement: format!(", {key}"),
                });
            } else if name == "list" && call.arguments.is_empty() {
                self.edits.push(Edit {
                    start: close,
                    end: close,
                    replacement: format!("[], {key}"),
                });
            } else if name == "list" && call.arguments.len() == 1 {
                self.edits.push(Edit {
                    start: close,
                    end: close,
                    replacement: format!(", {key}"),
                });
            }
        }
        walk::walk_variable_declarator(self, decl);
    }
}

fn collect_decl_names(decl: &Declaration<'_>, out: &mut Vec<String>) {
    match decl {
        Declaration::VariableDeclaration(v) => {
            for d in &v.declarations {
                collect_binding_names(&d.id, out);
            }
        }
        Declaration::FunctionDeclaration(f) => {
            if let Some(id) = &f.id {
                out.push(id.name.to_string());
            }
        }
        Declaration::ClassDeclaration(c) => {
            if let Some(id) = &c.id {
                out.push(id.name.to_string());
            }
        }
        _ => {}
    }
}

fn collect_binding_names(pat: &BindingPattern<'_>, out: &mut Vec<String>) {
    match pat {
        BindingPattern::BindingIdentifier(id) => out.push(id.name.to_string()),
        BindingPattern::ObjectPattern(obj) => {
            for prop in &obj.properties {
                collect_binding_names(&prop.value, out);
            }
            if let Some(rest) = &obj.rest {
                collect_binding_names(&rest.argument, out);
            }
        }
        BindingPattern::ArrayPattern(arr) => {
            for elem in arr.elements.iter().flatten() {
                collect_binding_names(elem, out);
            }
            if let Some(rest) = &arr.rest {
                collect_binding_names(&rest.argument, out);
            }
        }
        BindingPattern::AssignmentPattern(assign) => {
            collect_binding_names(&assign.left, out);
        }
    }
}

fn export_name(name: &ModuleExportName<'_>) -> String {
    match name {
        ModuleExportName::IdentifierName(id) => id.name.to_string(),
        ModuleExportName::IdentifierReference(id) => id.name.to_string(),
        ModuleExportName::StringLiteral(s) => s.value.as_str().unwrap_or("").to_string(),
    }
}

fn is_sdk_specifier(spec: &str) -> bool {
    matches!(spec, "jsnir" | "jsnir/sdk")
}

fn resolve_import(
    root: &Path,
    from_file: &Path,
    specifier: &str,
    source: &str,
    offset: u32,
) -> Result<PathBuf, DevError> {
    if !specifier.starts_with('.') && !specifier.starts_with('/') {
        let display = module_id_for(root, from_file);
        let (line, col, frame) = code_frame(source, offset as usize);
        return Err(DevError {
            title: "Unsupported package import".into(),
            message: format!(
                "Cannot resolve bare import {specifier:?} in {display}. Use relative imports (e.g. \"./file\") or \"jsnir\"."
            ),
            location: Some(format!("{display}:{line}:{col}")),
            frame: Some(frame),
        });
    }
    let base_dir = from_file.parent().unwrap_or(root);
    let raw = if specifier.starts_with('/') {
        root.join(specifier.trim_start_matches('/'))
    } else {
        base_dir.join(specifier)
    };
    let candidates = [
        raw.clone(),
        with_added_ext(&raw, "tsx"),
        with_added_ext(&raw, "ts"),
        with_added_ext(&raw, "jsx"),
        with_added_ext(&raw, "js"),
        raw.join("index.tsx"),
        raw.join("index.ts"),
        raw.join("index.jsx"),
        raw.join("index.js"),
    ];
    for candidate in candidates {
        if candidate.is_file() {
            return Ok(canonical_or_normalized(&candidate));
        }
    }
    let display = module_id_for(root, from_file);
    let (line, col, frame) = code_frame(source, offset as usize);
    Err(DevError {
        title: "Import not found".into(),
        message: format!("Cannot resolve {specifier:?} from {display}"),
        location: Some(format!("{display}:{line}:{col}")),
        frame: Some(frame),
    })
}

fn with_added_ext(path: &Path, ext: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".");
    os.push(ext);
    PathBuf::from(os)
}

fn apply_edits(source: &str, mut edits: Vec<Edit>) -> String {
    // Apply from end to start so earlier byte offsets remain valid.
    edits.sort_by(|a, b| b.start.cmp(&a.start).then_with(|| b.end.cmp(&a.end)));
    let mut out = source.to_string();
    for edit in edits {
        if edit.start <= edit.end && edit.end <= out.len() {
            out.replace_range(edit.start..edit.end, &edit.replacement);
        }
    }
    out
}

fn source_type_for(path: &Path) -> SourceType {
    match SourceType::from_path(path) {
        Ok(st) => st,
        Err(_) => SourceType::tsx(),
    }
}

fn format_oxc_error(
    title: &str,
    path: &Path,
    source: &str,
    errors: &[oxc::diagnostics::OxcDiagnostic],
) -> DevError {
    if let Some(first) = errors.first() {
        let offset = first
            .labels
            .first()
            .map(|l| l.offset())
            .unwrap_or(0);
        let (line, col, frame) = code_frame(source, offset as usize);
        let display = path.display().to_string();
        DevError {
            title: title.into(),
            message: first.message.to_string(),
            location: Some(format!("{display}:{line}:{col}")),
            frame: Some(frame),
        }
    } else {
        DevError {
            title: title.into(),
            message: format!("Failed to compile {}", path.display()),
            location: Some(path.display().to_string()),
            frame: None,
        }
    }
}

/// Builds a compact Vite-style code frame around `byte_offset`.
pub fn code_frame(source: &str, byte_offset: usize) -> (usize, usize, String) {
    let clamped = byte_offset.min(source.len());
    let prefix = &source[..clamped];
    let line_idx = prefix.bytes().filter(|&b| b == b'\n').count();
    let line_start = prefix.rfind('\n').map_or(0, |i| i + 1);
    let col = source[line_start..clamped].chars().count() + 1;
    let lines: Vec<&str> = source.lines().collect();
    let start_line = line_idx.saturating_sub(2);
    let end_line = (line_idx + 2).min(lines.len().saturating_sub(1));
    let mut frame = String::new();
    for i in start_line..=end_line {
        if let Some(text) = lines.get(i) {
            let marker = if i == line_idx { ">" } else { " " };
            frame.push_str(&format!("{marker} {:>4} | {text}\n", i + 1));
            if i == line_idx {
                let pad = " ".repeat(col.saturating_sub(1));
                frame.push_str(&format!("       | {pad}^\n"));
            }
        }
    }
    (line_idx + 1, col, frame.trim_end().to_string())
}

fn file_stamp(path: &Path) -> Result<(Option<SystemTime>, u64), DevError> {
    let meta = fs::metadata(path).map_err(|e| DevError {
        title: "File not found".into(),
        message: format!("{}: {e}", path.display()),
        location: Some(path.display().to_string()),
        frame: None,
    })?;
    Ok((meta.modified().ok(), meta.len()))
}

pub fn canonical_or_normalized(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| normalize_path(path))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn module_id_for(root: &Path, path: &Path) -> String {
    let canon_root = canonical_or_normalized(root);
    let canon_path = canonical_or_normalized(path);
    let rel = canon_path
        .strip_prefix(&canon_root)
        .unwrap_or(&canon_path);
    rel.to_string_lossy().replace('\\', "/")
}
