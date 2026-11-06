use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".venv",
    ".venv313",
    ".nuitka-venv",
    ".pyo3-forge",
    "venv",
    "env",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    "build",
    "dist",
    "target",
    "node_modules",
    "site-packages",
];

const HEAVY_MODULES: &[&str] = &[
    "transformers",
    "tokenizers",
    "torch",
    "tensorflow",
    "flax",
    "jax",
    "jaxlib",
    "accelerate",
    "safetensors",
    "sentencepiece",
    "datasets",
    "sklearn",
    "scipy",
    "cv2",
];

const DEFAULT_HEAVY_DEPS: &[&str] = &[
    "transformers",
    "tokenizers",
    "safetensors",
    "huggingface-hub",
    "accelerate",
];

#[derive(Debug, Clone)]
struct Config {
    root: PathBuf,
    forge_dir: PathBuf,
    package: String,
    python: String,
    requirements: Option<PathBuf>,
    copy_python: bool,
    wheel_only: bool,
    release: bool,
    modules: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        let root = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let forge_dir = root.join(".pyo3-forge");
        Self {
            root,
            forge_dir,
            package: "pyo3_forge_bridge".to_string(),
            python: "python".to_string(),
            requirements: None,
            copy_python: false,
            wheel_only: true,
            release: true,
            modules: DEFAULT_HEAVY_DEPS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Debug, Clone)]
struct ScanFile {
    path: PathBuf,
    imports: BTreeSet<String>,
    heavy_imports: BTreeSet<String>,
    top_level_heavy_imports: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct ScanReport {
    files: Vec<ScanFile>,
    imports: BTreeSet<String>,
    heavy: BTreeSet<String>,
}

fn main() {
    let mut args: Vec<String> = env::args().collect();
    let exe = args.remove(0);

    if args.is_empty() || args[0] == "help" || args[0] == "--help" || args[0] == "-h" {
        print_help(&exe);
        return;
    }

    let command = args.remove(0);
    let cfg = match parse_config(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    let result = match command.as_str() {
        "scan" => cmd_scan(&cfg),
        "init" => cmd_init(&cfg),
        "deps" => cmd_deps(&cfg),
        "develop" => cmd_maturin(&cfg, "develop"),
        "build" => cmd_maturin(&cfg, "build"),
        "check" => cmd_check(&cfg),
        "doctor" => cmd_doctor(&cfg),
        _ => {
            eprintln!("unknown command: {command}\n");
            print_help(&exe);
            std::process::exit(2);
        }
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn print_help(exe: &str) {
    println!(
        r#"pyo3-maturin-forge

A Rust CLI that wraps an existing Python repo in a mixed Rust/Python PyO3 + maturin host.
It does not Nuitka-compile Python. Instead, it creates a Python wheel with a Rust native
module, optional vendored Python source, and wheel-only import checks for heavy modules
such as transformers/tokenizers/torch.

USAGE:
  {exe} scan [options]
  {exe} init [options]
  {exe} deps [options]
  {exe} develop [options]
  {exe} build [options]
  {exe} check [options]
  {exe} doctor [options]

COMMON OPTIONS:
  --root <dir>              Python repo root. Default: current directory
  --forge-dir <dir>         Generated mixed PyO3 project dir. Default: <root>/.pyo3-forge
  --package <name>          Python package name for the bridge. Default: pyo3_forge_bridge
  --python <python>         Python executable. Default: python
  --requirements <file>     Requirements file to inspect/install
  --copy-python             Copy .py files from repo into the wheel payload during init
  --module <name>           Module/dependency to check or install. Can be repeated
  --allow-source            For deps: allow source builds. Default is wheel-only
  --debug                   For build: do not pass --release

EXAMPLES:
  # 1. Inspect repo imports
  {exe} scan --root .

  # 2. Create a mixed Rust/Python bridge project and vendor repo .py files
  {exe} init --root . --copy-python --package ostadnet_host

  # 3. Install build tools + HF stack using wheels only
  {exe} deps --python .venv313/bin/python --module transformers --module tokenizers

  # 4. Build wheel with maturin
  {exe} build --python .venv313/bin/python

  # 5. Install bridge into active venv for testing
  {exe} develop --python .venv313/bin/python

  # 6. Check imports through Python before/after building
  {exe} check --python .venv313/bin/python --module transformers --module tokenizers

PYTHON USAGE AFTER `develop`:
  import pyo3_forge_bridge as forge
  print(forge.import_check(["transformers", "tokenizers"]))
  forge.run_module("ostadnet_core", args=[])
"#
    );
}

fn parse_config(args: Vec<String>) -> Result<Config, String> {
    let mut cfg = Config::default();
    let mut forge_dir_override = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                i += 1;
                cfg.root = PathBuf::from(args.get(i).ok_or("--root needs a value")?);
            }
            "--forge-dir" => {
                i += 1;
                forge_dir_override = Some(PathBuf::from(args.get(i).ok_or("--forge-dir needs a value")?));
            }
            "--package" => {
                i += 1;
                cfg.package = sanitize_package(args.get(i).ok_or("--package needs a value")?);
            }
            "--python" => {
                i += 1;
                cfg.python = args.get(i).ok_or("--python needs a value")?.clone();
            }
            "--requirements" => {
                i += 1;
                cfg.requirements = Some(PathBuf::from(args.get(i).ok_or("--requirements needs a value")?));
            }
            "--copy-python" => cfg.copy_python = true,
            "--allow-source" => cfg.wheel_only = false,
            "--debug" => cfg.release = false,
            "--module" => {
                i += 1;
                let m = args.get(i).ok_or("--module needs a value")?.clone();
                if cfg.modules == DEFAULT_HEAVY_DEPS.iter().map(|s| s.to_string()).collect::<Vec<_>>() {
                    cfg.modules.clear();
                }
                cfg.modules.push(m);
            }
            unknown => return Err(format!("unknown option: {unknown}")),
        }
        i += 1;
    }

    cfg.root = absolutize(&cfg.root)?;
    cfg.forge_dir = match forge_dir_override {
        Some(path) if path.is_absolute() => path,
        Some(path) => cfg.root.join(path),
        None => cfg.root.join(".pyo3-forge"),
    };
    Ok(cfg)
}

fn sanitize_package(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else if c == '-' || c == '.' {
            out.push('_');
        }
    }
    if out.is_empty() {
        "pyo3_forge_bridge".to_string()
    } else if out.chars().next().unwrap().is_ascii_digit() {
        format!("_{out}")
    } else {
        out
    }
}

fn absolutize(p: &Path) -> Result<PathBuf, String> {
    if p.is_absolute() {
        Ok(p.to_path_buf())
    } else {
        env::current_dir()
            .map(|cwd| cwd.join(p))
            .map_err(|e| format!("could not resolve path {p:?}: {e}"))
    }
}

fn cmd_scan(cfg: &Config) -> Result<(), String> {
    let report = scan_repo(&cfg.root)?;
    print_scan_report(&cfg.root, &report);
    Ok(())
}

fn cmd_init(cfg: &Config) -> Result<(), String> {
    ensure_dir(&cfg.forge_dir)?;
    ensure_dir(&cfg.forge_dir.join("src"))?;
    ensure_dir(&cfg.forge_dir.join("python").join(&cfg.package))?;

    write_file(&cfg.forge_dir.join("Cargo.toml"), &cargo_toml())?;
    write_file(&cfg.forge_dir.join("pyproject.toml"), &pyproject_toml(&cfg.package))?;
    write_file(&cfg.forge_dir.join("README.md"), &generated_readme(&cfg.package))?;
    write_file(&cfg.forge_dir.join("src").join("lib.rs"), &lib_rs())?;
    write_file(
        &cfg.forge_dir.join("python").join(&cfg.package).join("__init__.py"),
        &python_init(),
    )?;
    write_file(
        &cfg.forge_dir.join("python").join(&cfg.package).join("runner.py"),
        &python_runner(),
    )?;
    write_file(
        &cfg.forge_dir.join("python").join(&cfg.package).join("py.typed"),
        "",
    )?;

    if cfg.copy_python {
        let payload = cfg.forge_dir.join("python").join(&cfg.package).join("payload");
        if payload.exists() {
            fs::remove_dir_all(&payload).map_err(|e| format!("remove old payload failed: {e}"))?;
        }
        ensure_dir(&payload)?;
        copy_python_sources(&cfg.root, &payload, &cfg.forge_dir)?;
    }

    println!("created mixed PyO3/maturin project at {}", cfg.forge_dir.display());
    println!("next:");
    println!("  {} deps --python <venv-python>", current_exe_name());
    println!("  {} develop --python <venv-python>", current_exe_name());
    println!("  {} build --python <venv-python>", current_exe_name());
    Ok(())
}

fn cmd_deps(cfg: &Config) -> Result<(), String> {
    run_status(&cfg.python, &["-m", "pip", "install", "-U", "pip", "setuptools", "wheel", "maturin"], &cfg.root)?;

    let mut modules = BTreeSet::new();
    for m in &cfg.modules {
        modules.insert(m.clone());
    }

    if let Some(req) = &cfg.requirements {
        let req_path = if req.is_absolute() { req.clone() } else { cfg.root.join(req) };
        for dep in read_requirements(&req_path)? {
            let name = requirement_root(&dep);
            if HEAVY_MODULES.iter().any(|h| h.replace('_', "-") == name || *h == name) {
                modules.insert(dep);
            }
        }
    }

    if modules.is_empty() {
        println!("no modules selected for dependency install");
        return Ok(());
    }

    let mut args: Vec<String> = vec!["-m".into(), "pip".into(), "install".into()];
    if cfg.wheel_only {
        args.push("--only-binary=:all:".into());
    }
    args.extend(modules.iter().cloned());

    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run_status(&cfg.python, &str_args, &cfg.root)?;
    Ok(())
}

fn cmd_maturin(cfg: &Config, subcommand: &str) -> Result<(), String> {
    if !cfg.forge_dir.join("Cargo.toml").exists() {
        return Err(format!("{} does not look initialized; run `init` first", cfg.forge_dir.display()));
    }

    let args = vec!["-m", "maturin", subcommand, "--manifest-path"];
    let manifest = cfg.forge_dir.join("Cargo.toml");
    let manifest_s = manifest.to_string_lossy().to_string();
    let mut owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    owned.push(manifest_s);

    if cfg.release && subcommand == "build" {
        owned.push("--release".to_string());
    }

    let str_args: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    run_status(&cfg.python, &str_args, &cfg.root)
}

fn cmd_check(cfg: &Config) -> Result<(), String> {
    let modules = if cfg.modules.is_empty() {
        DEFAULT_HEAVY_DEPS.iter().map(|s| s.to_string()).collect::<Vec<_>>()
    } else {
        cfg.modules.clone()
    };

    let code = format!(
        r#"import importlib, json
mods = {mods:?}
out = {{}}
for m in mods:
    try:
        importlib.import_module(m)
        out[m] = {{"ok": True, "error": ""}}
    except Exception as e:
        out[m] = {{"ok": False, "error": type(e).__name__ + ": " + str(e)}}
print(json.dumps(out, indent=2, sort_keys=True))
"#,
        mods = modules
    );
    run_status(&cfg.python, &["-c", &code], &cfg.root)
}

fn cmd_doctor(cfg: &Config) -> Result<(), String> {
    println!("root: {}", cfg.root.display());
    println!("forge-dir: {}", cfg.forge_dir.display());
    println!("package: {}", cfg.package);
    println!("python: {}", cfg.python);

    run_status_allow_fail(&cfg.python, &["--version"], &cfg.root)?;
    run_status_allow_fail(&cfg.python, &["-m", "pip", "--version"], &cfg.root)?;
    run_status_allow_fail(&cfg.python, &["-m", "maturin", "--version"], &cfg.root)?;
    run_status_allow_fail("cargo", &["--version"], &cfg.root)?;
    run_status_allow_fail("rustc", &["--version"], &cfg.root)?;

    let report = scan_repo(&cfg.root)?;
    print_scan_report(&cfg.root, &report);
    Ok(())
}

fn scan_repo(root: &Path) -> Result<ScanReport, String> {
    let mut files = Vec::new();
    collect_py_files(root, root, &mut files)?;

    let mut scan_files = Vec::new();
    let mut imports = BTreeSet::new();
    let mut heavy = BTreeSet::new();

    for file in files {
        let sf = scan_file(root, &file)?;
        imports.extend(sf.imports.iter().cloned());
        heavy.extend(sf.heavy_imports.iter().cloned());
        scan_files.push(sf);
    }

    Ok(ScanReport { files: scan_files, imports, heavy })
}

fn collect_py_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("read_dir {} failed: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        // Do not recurse through aliases or copy files from outside the selected tree.
        if entry.file_type().map_err(|e| e.to_string())?.is_symlink() {
            continue;
        }
        let path = entry.path();
        let name = path.file_name().and_then(OsStr::to_str).unwrap_or("");
        if path.is_dir() {
            if SKIP_DIRS.contains(&name) || name.ends_with(".dist") || name.ends_with(".build") {
                continue;
            }
            collect_py_files(root, &path, out)?;
        } else if path.extension().and_then(OsStr::to_str) == Some("py") {
            if path.starts_with(root) {
                out.push(path);
            }
        }
    }
    Ok(())
}

fn scan_file(root: &Path, file: &Path) -> Result<ScanFile, String> {
    let text = fs::read_to_string(file).map_err(|e| format!("read {} failed: {e}", file.display()))?;
    let mut imports = BTreeSet::new();
    let mut heavy_imports = BTreeSet::new();
    let mut top_level_heavy_imports = BTreeSet::new();

    for raw in text.lines() {
        let indent = raw.chars().take_while(|c| c.is_whitespace()).count();
        let line = raw.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }

        let roots = parse_import_roots(line);
        for root_name in roots {
            imports.insert(root_name.clone());
            if HEAVY_MODULES.contains(&root_name.as_str()) {
                heavy_imports.insert(root_name.clone());
                if indent == 0 {
                    top_level_heavy_imports.insert(root_name);
                }
            }
        }
    }

    Ok(ScanFile {
        path: file.strip_prefix(root).unwrap_or(file).to_path_buf(),
        imports,
        heavy_imports,
        top_level_heavy_imports,
    })
}

fn parse_import_roots(line: &str) -> Vec<String> {
    let mut out = Vec::new();

    if let Some(rest) = line.strip_prefix("import ") {
        for chunk in rest.split(',') {
            let name = chunk.trim().split_whitespace().next().unwrap_or("");
            let root = name.split('.').next().unwrap_or("").trim();
            if is_identish(root) {
                out.push(root.to_string());
            }
        }
    } else if let Some(rest) = line.strip_prefix("from ") {
        let name = rest.split_whitespace().next().unwrap_or("");
        if !name.starts_with('.') {
            let root = name.split('.').next().unwrap_or("").trim();
            if is_identish(root) {
                out.push(root.to_string());
            }
        }
    }

    out
}

fn is_identish(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn print_scan_report(root: &Path, report: &ScanReport) {
    println!("=== pyo3-maturin-forge scan ===");
    println!("root: {}", root.display());
    println!("python files: {}", report.files.len());
    println!("unique imports: {}", report.imports.len());
    println!("heavy modules seen: {}", join_set(&report.heavy));

    let offenders: Vec<&ScanFile> = report.files.iter().filter(|f| !f.top_level_heavy_imports.is_empty()).collect();
    if offenders.is_empty() {
        println!("top-level heavy imports: none");
    } else {
        println!("top-level heavy imports:");
        for f in offenders {
            println!("  {}: {}", f.path.display(), join_set(&f.top_level_heavy_imports));
        }
    }
}

fn join_set(set: &BTreeSet<String>) -> String {
    if set.is_empty() {
        "none".to_string()
    } else {
        set.iter().cloned().collect::<Vec<_>>().join(", ")
    }
}

fn ensure_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| format!("mkdir {} failed: {e}", path.display()))
}

fn write_file(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    fs::write(path, content).map_err(|e| format!("write {} failed: {e}", path.display()))
}

fn copy_python_sources(root: &Path, payload: &Path, generated_root: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    collect_py_files(root, root, &mut files)?;
    for src in files {
        if src.starts_with(generated_root) {
            continue;
        }
        let rel = src.strip_prefix(root).unwrap();
        let dst = payload.join(rel);
        if let Some(parent) = dst.parent() {
            ensure_dir(parent)?;
        }
        fs::copy(&src, &dst).map_err(|e| format!("copy {} -> {} failed: {e}", src.display(), dst.display()))?;
    }
    println!("copied Python sources into {}", payload.display());
    Ok(())
}

fn read_requirements(path: &Path) -> Result<Vec<String>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path).map_err(|e| format!("read requirements {} failed: {e}", path.display()))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('-'))
        .map(str::to_string)
        .collect())
}

fn requirement_root(req: &str) -> String {
    let mut end = req.len();
    for pat in ["==", ">=", "<=", "~=", "!=", ">", "<", "["].iter() {
        if let Some(i) = req.find(pat) {
            end = end.min(i);
        }
    }
    req[..end].trim().replace('_', "-").to_lowercase()
}

fn run_status(program: &str, args: &[&str], cwd: &Path) -> Result<(), String> {
    let status = run_status_inner(program, args, cwd)?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("command failed with status {status}: {} {}", program, args.join(" ")))
    }
}

fn run_status_allow_fail(program: &str, args: &[&str], cwd: &Path) -> Result<(), String> {
    let _ = run_status_inner(program, args, cwd)?;
    Ok(())
}

fn run_status_inner(program: &str, args: &[&str], cwd: &Path) -> Result<ExitStatus, String> {
    eprintln!("+ {} {}", program, args.join(" "));
    Command::new(program)
        .args(args)
        .current_dir(cwd)
        .status()
        .map_err(|e| format!("failed to run `{program}`: {e}"))
}

fn current_exe_name() -> String {
    env::args().next().unwrap_or_else(|| "pyo3-maturin-forge".to_string())
}

fn cargo_toml() -> String {
    r#"[package]
name = "pyo3-forge-bridge"
version = "0.1.0"
edition = "2021"

[lib]
name = "_native"
crate-type = ["cdylib"]

[dependencies]
pyo3 = "0.29"
"#
    .to_string()
}

fn pyproject_toml(package: &str) -> String {
    format!(
        r#"[build-system]
requires = ["maturin>=1.9.4"]
build-backend = "maturin"

[project]
name = "{dist_name}"
version = "0.1.0"
description = "PyO3/maturin bridge generated by pyo3-maturin-forge"
requires-python = ">=3.9"
dependencies = []

[project.optional-dependencies]
hf = ["transformers", "tokenizers", "safetensors", "huggingface-hub", "accelerate"]
torch = ["torch"]
all = ["transformers", "tokenizers", "safetensors", "huggingface-hub", "accelerate", "torch"]

[tool.maturin]
python-source = "python"
module-name = "{package}._native"
bindings = "pyo3"
"#,
        dist_name = package.replace('_', "-"),
        package = package
    )
}

fn generated_readme(package: &str) -> String {
    format!(
        r#"# {package}

Generated mixed Rust/Python project.

## Develop

```bash
python -m pip install -U maturin
python -m maturin develop --manifest-path Cargo.toml
```

## Build wheel

```bash
python -m maturin build --release --manifest-path Cargo.toml
```

## Use

```python
import {package} as forge
print(forge.import_check(["transformers", "tokenizers"]))
forge.run_module("ostadnet_core", args=[])
```

If this project was initialized with `--copy-python`, the Python source payload is inside:

```text
python/{package}/payload/
```
"#
    )
}

fn python_init() -> &'static str {
    r#"from ._native import import_check, run_module, call_object
from .runner import payload_root, run_payload_module, call_payload_object

__all__ = [
    "import_check",
    "run_module",
    "call_object",
    "payload_root",
    "run_payload_module",
    "call_payload_object",
]
"#
}

fn python_runner() -> &'static str {
    r#"from __future__ import annotations

from pathlib import Path
from . import _native


def payload_root() -> str:
    return str(Path(__file__).resolve().parent / "payload")


def run_payload_module(module: str, args: list[str] | None = None):
    return _native.run_module(module, args or [], payload_root())


def call_payload_object(qualified_name: str, args: list[str] | None = None) -> str:
    return _native.call_object(qualified_name, args or [], payload_root())
"#
}

fn lib_rs() -> &'static str {
    r#"use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyModule};

#[pyfunction]
fn import_check(py: Python<'_>, modules: Vec<String>) -> PyResult<Vec<(String, bool, String)>> {
    let importlib = py.import("importlib")?;
    let import_module = importlib.getattr("import_module")?;
    let mut out = Vec::new();

    for module in modules {
        match import_module.call1((module.as_str(),)) {
            Ok(_) => out.push((module, true, "ok".to_string())),
            Err(err) => out.push((module, false, err.to_string())),
        }
    }

    Ok(out)
}

#[pyfunction]
fn run_module(
    py: Python<'_>,
    module: &str,
    args: Vec<String>,
    source_root: Option<String>,
) -> PyResult<()> {
    if let Some(root) = source_root {
        let sys = py.import("sys")?;
        let path = sys.getattr("path")?;
        path.call_method1("insert", (0, root))?;
    }

    let sys = py.import("sys")?;
    let argv_values: Vec<String> = std::iter::once(module.to_string()).chain(args.into_iter()).collect();
    let argv = PyList::new(py, argv_values)?;
    sys.setattr("argv", argv)?;

    let runpy = py.import("runpy")?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("run_name", "__main__")?;
    runpy.getattr("run_module")?.call((module,), Some(&kwargs))?;
    Ok(())
}

#[pyfunction]
fn call_object(
    py: Python<'_>,
    qualified_name: &str,
    args: Vec<String>,
    source_root: Option<String>,
) -> PyResult<String> {
    if let Some(root) = source_root {
        let sys = py.import("sys")?;
        let path = sys.getattr("path")?;
        path.call_method1("insert", (0, root))?;
    }

    let Some((module_name, attr_name)) = qualified_name.rsplit_once(':') else {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "qualified_name must look like 'module.submodule:function_name'",
        ));
    };

    let importlib = py.import("importlib")?;
    let module = importlib.getattr("import_module")?.call1((module_name,))?;
    let func = module.getattr(attr_name)?;
    let py_args = PyList::new(py, args)?;
    let result = func.call1((py_args,))?;
    Ok(result.repr()?.to_string())
}

#[pymodule]
#[pyo3(name = "_native")]
fn native_bridge(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(import_check, m)?)?;
    m.add_function(wrap_pyfunction!(run_module, m)?)?;
    m.add_function(wrap_pyfunction!(call_object, m)?)?;
    Ok(())
}
"#
}

#[allow(dead_code)]
fn prompt_yes_no(question: &str) -> io::Result<bool> {
    print!("{question} [y/N] ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim().to_lowercase().as_str(), "y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(args: &[&str]) -> Config {
        parse_config(args.iter().map(|s| s.to_string()).collect()).unwrap()
    }

    #[test]
    fn root_controls_default_output_directory() {
        let cfg = options(&["--root", "/tmp/forge-project"]);
        assert_eq!(cfg.forge_dir, Path::new("/tmp/forge-project/.pyo3-forge"));
    }

    #[test]
    fn relative_output_is_relative_to_root_in_either_option_order() {
        for args in [
            ["--root", "/tmp/forge-project", "--forge-dir", "generated"],
            ["--forge-dir", "generated", "--root", "/tmp/forge-project"],
        ] {
            assert_eq!(options(&args).forge_dir, Path::new("/tmp/forge-project/generated"));
        }
    }

    #[test]
    fn absolute_output_is_preserved() {
        let cfg = options(&["--forge-dir", "/tmp/elsewhere", "--root", "/tmp/forge-project"]);
        assert_eq!(cfg.forge_dir, Path::new("/tmp/elsewhere"));
    }

    #[test]
    fn scanner_recognizes_static_import_roots_and_skips_relative_imports() {
        assert_eq!(parse_import_roots("import os, torch.nn as nn"), ["os", "torch"]);
        assert_eq!(parse_import_roots("from transformers.models import AutoModel"), ["transformers"]);
        assert!(parse_import_roots("from .helpers import work").is_empty());
    }
}
