# simple-clangd-gen
Very simple tool to generate clang compilation database file

This tool scans for C/C++ files according to a configured layout. This tool is intended to cope
with random build-environment setups, that prevents usage of [clangd](https://clangd.llvm.org/) as LSP server.

## Configuration example

Example input file (in [YAML](https://en.wikipedia.org/wiki/YAML) format):
```yaml
compile_flags: "-Wall"           # compilation flags for all targets
include_paths:
    - "/usr/include"             # include path for all targets
branches:
    - branch: "*/source"         # glob mask for C/C++ files
      compile_flags: "-Wextra"   # additional compile flags
      include_paths:             # additional include paths
          - "../include"         # (when starting with . relative from source path
      mask: ["*.c", "*.cpp"]     # glob mask to be included files
    - branch: "*/tests"
      include_paths:
          - "../include"
          - "../test_include"
      tool: "collect_test_cases" # mutual exclusive with "mask", a tool the
                                 # returns files that need to be included.
```

## Example usage
```
simple-clangd-gen input.yaml compile_commands.json
```

## Reference
[JSON Compilation Database specification](https://clang.llvm.org/docs/JSONCompilationDatabase.html)
