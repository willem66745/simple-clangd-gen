use anyhow::{Context, Error, Result};
use globset::{Glob, GlobMatcher};
use serde::{Deserialize, Serialize};
use structopt::StructOpt;

use std::env::{self, current_dir};
use std::fs::{read_dir, File};
use std::path::{self, Component, Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    compile_flags: Option<String>,
    include_paths: Option<Vec<PathBuf>>,
    branches: Vec<Branch>,
}

impl Config {
    fn write_to(self, file: &Path) -> Result<()> {
        let compile_flags = self.compile_flags.unwrap_or_else(|| String::new());
        let include_paths = self.include_paths.unwrap_or_else(|| Vec::new());
        let mut db = Vec::new();
        for branch in self.branches {
            db.extend(branch.create_clangd_entry(compile_flags.clone(), include_paths.clone())?);
        }

        let out = File::create(file).context("unable to create file for output generation")?;
        serde_json::to_writer_pretty(&out, &db)
            .context("problem while serializing clangd compilation database")?;

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Branch {
    branch: String,
    compile_flags: Option<String>,
    include_paths: Option<Vec<PathBuf>>,
    mask: Option<Vec<String>>,
    tool: Option<String>,
}

impl Branch {
    fn create_clangd_entry(
        mut self,
        mut flags: String,
        mut paths: Vec<PathBuf>,
    ) -> Result<Vec<CLangEntry>> {
        let cur_dir = current_dir()?;

        if let Some(path::MAIN_SEPARATOR) = self.branch.chars().next() {
        } else {
            // put current path to configured branch glob
            self.branch = format!(
                "{}{}{}",
                cur_dir.display(),
                path::MAIN_SEPARATOR,
                self.branch
            );
        }

        let glob = Glob::new(&self.branch)
            .with_context(|| {
                format!(
                    "unable to parse '{}' as 'branch' as a glob pattern",
                    self.branch
                )
            })?
            .compile_matcher();
        if let Some(compile_flags) = self.compile_flags.take() {
            if flags.len() > 0 {
                flags.push(' ');
            }
            flags.push_str(&compile_flags);
        }
        if let Some(include_paths) = self.include_paths.take() {
            paths.extend(include_paths);
        }

        let mut candidates = Vec::new();
        find_directories(&cur_dir, &glob, &mut candidates);

        let mut files = Vec::new();

        for candidate in candidates {
            if let Some(ref mask) = self.mask {
                let masks = mask.iter().map(|x| x.as_str()).collect::<Vec<_>>();
                scan_files(&candidate, &masks, &mut files);
            } else if let Some(ref tool) = self.tool {
                let mut elements = tool.split_whitespace();
                let cmd: Option<&str> = elements.next();
                let arguments: Vec<&str> = elements.collect();
                if let Some(cmd) = cmd {
                    // execute a tool in candidate directory location
                    let cmd = Command::new(cmd)
                        .args(arguments)
                        .current_dir(&candidate)
                        .output()
                        .with_context(|| format!("unable to execute tool '{}'", tool))?;
                    if !cmd.status.success() {
                        return Err(Error::msg(format!(
                            "'{}' returned error: {}\n---stdout:\n{}\n---stderr:\n{}",
                            tool,
                            cmd.status.code().unwrap_or(0),
                            String::from_utf8_lossy(&cmd.stdout),
                            String::from_utf8_lossy(&cmd.stderr),
                        )));
                    }
                    // isolate file-names from response
                    for filename in String::from_utf8_lossy(&cmd.stdout)
                        .split_whitespace()
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                    {
                        let mut file;
                        if filename.starts_with("/") {
                            // appears to be a full path
                            file = PathBuf::from(filename);
                        } else {
                            // appears a fragment so assume the relative location from execution
                            // point
                            file = PathBuf::from(&candidate);
                            file.push(filename);
                        }
                        // only consider existing files
                        if file.exists() {
                            let file = file.canonicalize()?;
                            files.push(file);
                        }
                    }
                }
            } else {
                // scan for the trivial C/C++ extensions
                scan_files(
                    &candidate,
                    &["*.c", "*.C", "*.cc", "*.cpp", "*.cxx", "*.c++"],
                    &mut files,
                );
            }
        }

        let mut db_items = Vec::new();

        for file_path in files {
            let directory = file_path
                .parent()
                .map(|d| d.to_path_buf())
                .ok_or_else(|| Error::msg("unable to find parent directory for file"))?;
            let file = file_path
                .file_name()
                .and_then(|f| f.to_str())
                .map(|f| f.to_string())
                .ok_or_else(|| Error::msg("unable to find file-name"))?;
            let mut object_file = file_path.clone();
            object_file.set_extension("o");
            let object_file = object_file
                .file_name()
                .and_then(|f| f.to_str())
                .map(|f| f.to_string())
                .ok_or_else(|| Error::msg("unable to find file-name"))?;

            // resolve paths for every file iteration (bug-fix where only the first file gets
            // resolved).
            let mut paths = paths.clone();

            for path in paths.iter_mut() {
                let first_component = path.components().next();

                // don't touch paths when they are a root-dir or a windows driver letter
                if !matches!(first_component, Some(Component::RootDir) | Some(Component::Prefix(_)))
                {
                    let mut new;
                    // use execution location as reference when no "." or ".." are used
                    if matches!(first_component, Some(Component::Normal(_))) {
                        new = cur_dir.clone();
                    } else {
                        // when "." or ".." are used, the path relative from file location
                        new = file_path.clone();
                        new.pop();
                    }
                    let error_path = new.clone(); // just to produce a better error message (if any)
                    new.push(&path);
                    *path = new.canonicalize().with_context(|| {
                        format!(
                            "unable to resolve '{}' from '{}'",
                            path.display(),
                            error_path.display()
                        )
                    })?;
                }
            }

            let mut options = paths.iter().map(|p| format!("-I{}", p.display())).fold(
                String::new(),
                |mut s, p| {
                    if !s.is_empty() {
                        s.push(' ');
                    }
                    s.push_str(&p);

                    s
                },
            );
            if !options.is_empty() && !flags.is_empty() {
                options.push(' ');
            }
            options.push_str(&flags);

            let exe = resolve_executable(&file_path)?;

            let command = Some(format!(
                "{} {} -c -o {} {}",
                exe, options, object_file, file
            ));
            //let arguments = None;
            //let output = None;

            db_items.push(CLangEntry {
                directory,
                file,
                command,
                //arguments,
                //output,
            });
        }

        Ok(db_items)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CLangEntry {
    directory: PathBuf,
    file: String,
    command: Option<String>,
    //arguments: Option<Vec<String>>,
    //output: Option<String>,
}

#[derive(StructOpt, Debug)]
#[structopt(name = "simple-clangd-gen")]
struct Opt {
    /// Configuration file (YAML or JSON)
    #[structopt(name = "INPUT", parse(from_os_str))]
    input: PathBuf,
    /// Generated JSON Compilation Database Format Specification
    #[structopt(name = "OUTPUT", parse(from_os_str))]
    output: PathBuf,
}

fn main() -> Result<()> {
    let opt = Opt::from_args();

    let conf = match File::open(&opt.input) {
        Err(e) => {
            eprintln!(
                "ERROR: unable to open file `{}`: {}",
                opt.input.display(),
                e
            );
            std::process::exit(1);
        }
        Ok(f) => f,
    };

    let conf: Config = match opt.input.extension() {
        Some(e) if e == "yml" || e == "yaml" => match serde_yaml::from_reader(conf) {
            Ok(c) => c,
            Err(e) => {
                println!("ERROR: parsing error in `{}`: {}", opt.input.display(), e);
                std::process::exit(1);
            }
        },
        Some(e) if e == "json" => match serde_json::from_reader(conf) {
            Ok(c) => c,
            Err(e) => {
                println!("ERROR: parsing error in `{}`: {}", opt.input.display(), e);
                std::process::exit(1);
            }
        },
        _ => {
            eprintln!("ERROR: only yaml/json files are supported");
            std::process::exit(1);
        }
    };

    conf.write_to(&opt.output)?;
    Ok(())
}

/// Find directories that matches the given path and matcher.
/// It ignores any error and tries to return the best as possible result.
fn find_directories(dir: &Path, matcher: &GlobMatcher, candidates: &mut Vec<PathBuf>) {
    if let Ok(mut dir) = read_dir(dir) {
        while let Some(Ok(entry)) = dir.next() {
            if let Ok(file_type) = entry.file_type() {
                if file_type.is_dir() && !file_type.is_symlink() {
                    let path = entry.path();
                    if matcher.is_match(&path) {
                        candidates.push(path);
                    } else {
                        find_directories(&path, matcher, candidates);
                    }
                }
            }
        }
    }
}

fn scan_files(path: &Path, masks: &[&str], files: &mut Vec<PathBuf>) {
    let masks: Vec<_> = masks
        .iter()
        .filter_map(|d| Glob::new(d).ok())
        .map(|d| d.compile_matcher())
        .collect();

    if let Ok(mut dir) = read_dir(path) {
        while let Some(Ok(entry)) = dir.next() {
            if masks.iter().any(|m| m.is_match(entry.file_name())) {
                files.push(entry.path());
            }
        }
    }
}

fn resolve_executable(source_file: &Path) -> Result<String> {
    let is_c = source_file.extension().map(|s| s == "c").unwrap_or(false);

    let candidates = match is_c {
        false => &["clang++", "g++", "c++"],
        true => &["clang", "gcc", "cc"],
    };

    let paths = env::var_os("PATH")
        .ok_or_else(|| Error::msg("unable to resolve PATH environment variable"))?;

    for candidate in candidates {
        for mut path in env::split_paths(&paths) {
            path.push(candidate);
            if path.exists() {
                return Ok(format!("{}", path.display()));
            }
        }
    }

    Err(Error::msg(format!(
        "unable to locate a compiler for '{}'",
        source_file.display()
    )))
}
