//! Oracle tests for the source-code parser (`analysis::parser`).
//!
//! Each test feeds known source code in a specific language (Python, Rust,
//! TypeScript, Go, Java, C/C++) to `parse_bytes` and asserts exact counts
//! of functions, classes, imports, and other structural elements. These
//! serve as regression guards for the tree-sitter extraction logic.
//! Key property: known input must always produce known output (oracle test).

#[cfg(test)]
mod tests {
    use crate::analysis::parser::parse_bytes;

    // ---- Oracle tests: known code -> expected counts ----

    #[test]
    fn oracle_python() {
        let code = br#"
import os
from collections import defaultdict

class Animal:
    def __init__(self, name):
        self.name = name
    def speak(self):
        pass

class Dog(Animal):
    def speak(self):
        return "Woof"

def greet(name):
    print(f"Hello {name}")

def main():
    dog = Dog("Rex")
    dog.speak()
    greet("World")
"#;
        let sa = parse_bytes(code, "python").expect("python parse failed");
        let fns = sa.functions.as_ref().expect("no functions");
        // __init__, speak (Animal), speak (Dog), greet, main = 5
        assert_eq!(fns.len(), 5, "expected 5 functions, got {:?}", fns);
        let cls = sa.cls.as_ref().expect("no classes");
        assert_eq!(cls.len(), 2, "expected 2 classes, got {:?}", cls);
        let imp = sa.imp.as_ref().expect("no imports");
        assert!(imp.len() >= 1, "expected at least 1 import, got {:?}", imp);
        // Calls are distributed to their containing functions via func.co
        let func_call_count: usize = fns.iter()
            .map(|f| f.co.as_ref().map_or(0, |c| c.len()))
            .sum();
        let module_call_count = sa.co.as_ref().map_or(0, |c| c.len());
        assert!(func_call_count + module_call_count >= 2,
            "expected at least 2 calls total, got {} in funcs + {} module-level", func_call_count, module_call_count);
    }

    #[test]
    fn python_dotted_imports_normalized() {
        let code = br#"
from orderflow_ml.config import load_instrument_config
from orderflow_ml.utils.symbol_utils import extract_root_symbol
import os.path
"#;
        let sa = parse_bytes(code, "python").expect("python parse failed");
        let imp = sa.imp.as_ref().expect("no imports");
        // Parser now normalizes dots->slashes for Python (language-aware). [ref:daa66d13]
        assert!(imp.iter().any(|i| i == "orderflow_ml/config"),
            "expected 'orderflow_ml/config', got {:?}", imp);
        assert!(imp.iter().any(|i| i == "orderflow_ml/utils/symbol_utils"),
            "expected 'orderflow_ml/utils/symbol_utils', got {:?}", imp);
    }

    #[test]
    fn oracle_javascript() {
        let code = br#"
import React from 'react';

class Button {
    render() {
        return null;
    }
}

function handleClick(e) {
    console.log(e);
    fetch('/api');
}

function main() {
    handleClick(null);
}
"#;
        let sa = parse_bytes(code, "javascript").expect("js parse failed");
        let fns = sa.functions.as_ref().expect("no functions");
        // render, handleClick, main = 3
        assert_eq!(fns.len(), 3, "expected 3 functions, got {:?}", fns);
        let cls = sa.cls.as_ref().expect("no classes");
        assert_eq!(cls.len(), 1, "expected 1 class");
        assert!(sa.imp.is_some(), "expected imports");
        // Calls are distributed to containing functions -- check func.co
        let func_call_count: usize = fns.iter()
            .map(|f| f.co.as_ref().map_or(0, |c| c.len()))
            .sum();
        let module_call_count = sa.co.as_ref().map_or(0, |c| c.len());
        assert!(func_call_count + module_call_count > 0,
            "expected at least 1 call, got {} in funcs + {} module-level", func_call_count, module_call_count);
    }

    #[test]
    fn oracle_rust() {
        let code = br#"
use std::collections::HashMap;

struct Config {
    name: String,
}

impl Config {
    fn new(name: &str) -> Self {
        Self { name: name.to_string() }
    }
    fn display(&self) {
        println!("{}", self.name);
    }
}

fn main() {
    let c = Config::new("test");
    c.display();
}
"#;
        let sa = parse_bytes(code, "rust").expect("rust parse failed");
        let fns = sa.functions.as_ref().expect("no functions");
        // new, display, main = 3
        assert_eq!(fns.len(), 3, "expected 3 functions, got {:?}", fns);
        let cls = sa.cls.as_ref().expect("no classes");
        // struct Config = 1 (impl blocks are reference.implementation, not class defs)
        assert_eq!(cls.len(), 1, "expected 1 class-like item, got {:?}", cls);
        assert!(sa.imp.is_some(), "expected imports");
    }

    #[test]
    fn rust_mod_declarations_captured_as_imports() {
        let code = br#"
pub mod state;
pub mod channels;
mod helpers;
use crate::analysis::FileNode;

fn main() {}
"#;
        let sa = parse_bytes(code, "rust").expect("rust parse failed");
        let imports = sa.imp.expect("expected imports from mod declarations");
        eprintln!("mod imports captured: {:?}", imports);
        assert!(imports.iter().any(|i| i == "state"),
            "expected 'state' from `pub mod state;`, got {:?}", imports);
        assert!(imports.iter().any(|i| i == "channels"),
            "expected 'channels' from `pub mod channels;`, got {:?}", imports);
        assert!(imports.iter().any(|i| i == "helpers"),
            "expected 'helpers' from `mod helpers;`, got {:?}", imports);
    }

    #[test]
    fn rust_use_tree_braces() {
        // use crate::models::episode::{Episode, Injection} -> module path "crate::models::episode"
        // use crate::models::{episode, primitive}           -> two modules
        let code = br#"
use crate::models::episode::{Episode, Injection};
use crate::models::{episode, primitive};
use std::collections::HashMap;
use anyhow::{Context, Result};

fn main() {}
"#;
        let sa = parse_bytes(code, "rust").expect("rust parse failed");
        let imports = sa.imp.expect("expected imports");
        // After normalization (:: -> /), should contain:
        //   "crate/models/episode"  (from first use -- stripped {Episode, Injection})
        //   "crate/models/episode"  (from second use -- expanded submodule, deduped)
        //   "crate/models/primitive"(from second use -- expanded submodule)
        //   "std/collections/HashMap" (no braces)
        //   "anyhow"               (stripped {Context, Result})
        assert!(imports.contains(&"crate/models/episode".to_string()),
            "missing crate/models/episode, got {:?}", imports);
        assert!(imports.contains(&"crate/models/primitive".to_string()),
            "missing crate/models/primitive, got {:?}", imports);
        assert!(imports.contains(&"std/collections/HashMap".to_string()),
            "missing std/collections/HashMap, got {:?}", imports);
        assert!(imports.contains(&"anyhow".to_string()),
            "missing anyhow, got {:?}", imports);
        // Should NOT contain raw language syntax
        assert!(!imports.iter().any(|i| i.contains('{') || i.contains("::")),
            "imports should be normalized (no braces or ::), got {:?}", imports);
    }

    #[test]
    fn oracle_go() {
        let code = br#"
package main

import "fmt"

type Server struct {
    host string
}

func (s *Server) Start() {
    fmt.Println("starting", s.host)
}

func NewServer(host string) *Server {
    return &Server{host: host}
}

func main() {
    s := NewServer("localhost")
    s.Start()
}
"#;
        let sa = parse_bytes(code, "go").expect("go parse failed");
        let fns = sa.functions.as_ref().expect("no functions");
        // Start (method), NewServer, main = 3
        assert_eq!(fns.len(), 3, "expected 3 functions, got {:?}", fns);
        let cls = sa.cls.as_ref().expect("no classes");
        assert_eq!(cls.len(), 1, "expected 1 type decl");
    }

    #[test]
    fn go_grouped_imports_extracted() {
        let code = br#"
package main

import (
    "context"
    "fmt"
    "net/http"

    "github.com/slush-dev/phonevault/internal/config"
    "github.com/slush-dev/phonevault/internal/handler"
)

func main() {}
"#;
        let sa = parse_bytes(code, "go").expect("go parse failed");
        let imports = sa.imp.expect("expected imports");
        assert!(imports.contains(&"context".to_string()), "missing context, got {:?}", imports);
        assert!(imports.contains(&"fmt".to_string()), "missing fmt, got {:?}", imports);
        assert!(imports.contains(&"net/http".to_string()), "missing net/http, got {:?}", imports);
        assert!(imports.contains(&"github.com/slush-dev/phonevault/internal/config".to_string()),
            "missing internal/config, got {:?}", imports);
        assert!(imports.contains(&"github.com/slush-dev/phonevault/internal/handler".to_string()),
            "missing internal/handler, got {:?}", imports);
        assert_eq!(imports.len(), 5, "expected 5 imports, got {:?}", imports);
    }

    #[test]
    fn go_aliased_imports_only_extract_paths() {
        let code = br#"
package main

import (
    "fmt"
    cfg "github.com/slush-dev/phonevault/internal/config"
    _ "github.com/lib/pq"
    . "github.com/onsi/gomega"
)

func main() {}
"#;
        let sa = parse_bytes(code, "go").expect("go parse failed");
        let imports = sa.imp.expect("expected imports");
        // Only quoted paths should be extracted — aliases (cfg, _, .) are skipped
        assert!(imports.contains(&"fmt".to_string()), "missing fmt");
        assert!(imports.contains(&"github.com/slush-dev/phonevault/internal/config".to_string()),
            "missing config path");
        assert!(imports.contains(&"github.com/lib/pq".to_string()), "missing pq");
        assert!(imports.contains(&"github.com/onsi/gomega".to_string()), "missing gomega");
        assert!(!imports.iter().any(|i| i == "cfg"), "alias 'cfg' should not be in imports");
        assert_eq!(imports.len(), 4, "expected 4 imports, got {:?}", imports);
    }

    #[test]
    fn oracle_java() {
        let code = br#"
import java.util.List;
import java.util.ArrayList;

public class App {
    public App() {}
    public void run() {
        List<String> items = new ArrayList<>();
        items.add("hello");
        System.out.println(items);
    }
    public static void main(String[] args) {
        App app = new App();
        app.run();
    }
}
"#;
        let sa = parse_bytes(code, "java").expect("java parse failed");
        let fns = sa.functions.as_ref().expect("no functions");
        // App (constructor), run, main = 3
        assert_eq!(fns.len(), 3, "expected 3 methods, got {:?}", fns);
        let cls = sa.cls.as_ref().expect("no classes");
        assert_eq!(cls.len(), 1, "expected 1 class");
        assert!(sa.imp.is_some(), "expected imports");
    }

    #[test]
    fn oracle_c() {
        let code = br#"
#include <stdio.h>
#include "mylib.h"

struct Point {
    int x;
    int y;
};

void print_point(struct Point p) {
    printf("(%d, %d)\n", p.x, p.y);
}

int main() {
    struct Point p = {1, 2};
    print_point(p);
    return 0;
}
"#;
        let sa = parse_bytes(code, "c").expect("c parse failed");
        let fns = sa.functions.as_ref().expect("no functions");
        // print_point, main = 2
        assert_eq!(fns.len(), 2, "expected 2 functions, got {:?}", fns);
        let cls = sa.cls.as_ref().expect("no structs");
        assert_eq!(cls.len(), 1, "expected 1 struct");
        let imp = sa.imp.as_ref().expect("no imports");
        assert_eq!(imp.len(), 2, "expected 2 includes, got {:?}", imp);
    }

    #[test]
    fn oracle_cpp() {
        let code = br#"
#include <iostream>
#include <string>

class Greeter {
public:
    void greet(const std::string& name) {
        std::cout << "Hello " << name << std::endl;
    }
};

int main() {
    Greeter g;
    g.greet("world");
    return 0;
}
"#;
        let sa = parse_bytes(code, "cpp").expect("cpp parse failed");
        let fns = sa.functions.as_ref().expect("no functions");
        // greet, main = 2
        assert_eq!(fns.len(), 2, "expected 2 functions, got {:?}", fns);
        let cls = sa.cls.as_ref().expect("no classes");
        assert_eq!(cls.len(), 1, "expected 1 class");
    }

    #[test]
    fn oracle_csharp() {
        let code = br#"
using System;
using System.Collections.Generic;

public class Calculator {
    public Calculator() {}
    public int Add(int a, int b) {
        return a + b;
    }
    public static void Main(string[] args) {
        var calc = new Calculator();
        Console.WriteLine(calc.Add(1, 2));
    }
}
"#;
        let sa = parse_bytes(code, "csharp").expect("csharp parse failed");
        let fns = sa.functions.as_ref().expect("no functions");
        // Calculator (constructor), Add, Main = 3
        assert_eq!(fns.len(), 3, "expected 3 methods, got {:?}", fns);
        let cls = sa.cls.as_ref().expect("no classes");
        assert_eq!(cls.len(), 1, "expected 1 class");
        assert!(sa.imp.is_some(), "expected using directives");
    }

    #[test]
    fn oracle_bash() {
        let code = br#"
#!/bin/bash

greet() {
    echo "Hello $1"
}

cleanup() {
    rm -rf /tmp/test
}

greet "world"
cleanup
ls -la
"#;
        let sa = parse_bytes(code, "bash").expect("bash parse failed");
        let fns = sa.functions.as_ref().expect("no functions");
        assert_eq!(fns.len(), 2, "expected 2 functions, got {:?}", fns);
        let all_calls: Vec<String> = fns.iter()
            .flat_map(|f| f.co.iter().flat_map(|c| c.iter().cloned()))
            .chain(sa.co.iter().flat_map(|c| c.iter().cloned()))
            .collect();
        assert!(all_calls.len() >= 3, "expected at least 3 commands, got {:?}", all_calls);
    }

    #[test]
    fn bash_source_imports() {
        let code = br#"
#!/bin/bash

source ./lib/utils.sh
. ./config.sh
source '/path/to/helpers.sh'

# source ./commented_out.sh
echo "source ./not_real.sh"

# Variable paths are skipped (can't resolve statically)
source $DIR/dynamic.sh

greet() {
    echo "Hello"
}

greet
ls -la
"#;
        let sa = parse_bytes(code, "bash").expect("bash parse failed");
        let imports = sa.imp.as_ref().expect("no imports");
        // Should capture: ./lib/utils.sh, ./config.sh, /path/to/helpers.sh
        // Should NOT capture: commented_out.sh, not_real.sh, $DIR/dynamic.sh
        assert_eq!(imports.len(), 3, "expected 3 imports, got {:?}", imports);
        assert!(imports.contains(&"./lib/utils.sh".to_string()));
        assert!(imports.contains(&"./config.sh".to_string()));
        assert!(imports.contains(&"/path/to/helpers.sh".to_string()));

        // Functions and calls should still work
        let fns = sa.functions.as_ref().expect("no functions");
        assert_eq!(fns.len(), 1, "expected 1 function");
        assert_eq!(fns[0].n, "greet");
    }

    #[test]
    fn bash_install_sh_calls() {
        // Real-world: install.sh with internal function calls but no source imports
        let code = br#"
#!/usr/bin/env bash
set -euo pipefail

detect_platform() {
    echo "darwin-arm64"
}

build_from_source() {
    cargo build --release
}

install_binary() {
    local platform="$1"
    build_from_source
}

main() {
    local platform
    platform="$(detect_platform)"
    install_binary "$platform"
}

main "$@"
"#;
        let sa = parse_bytes(code, "bash").expect("bash parse failed");
        let fns = sa.functions.as_ref().expect("no functions");
        let fn_names: Vec<&str> = fns.iter().map(|f| f.n.as_str()).collect();
        assert_eq!(fns.len(), 4, "expected 4 functions, got {:?}", fn_names);

        // Calls inside functions go to func.co; module-level calls stay in sa.co
        let all_calls: Vec<String> = fns.iter()
            .flat_map(|f| f.co.iter().flat_map(|c| c.iter().cloned()))
            .chain(sa.co.iter().flat_map(|c| c.iter().cloned()))
            .collect();
        assert!(all_calls.contains(&"build_from_source".to_string()), "missing build_from_source call, got {:?}", all_calls);
        assert!(all_calls.contains(&"detect_platform".to_string()), "missing detect_platform call, got {:?}", all_calls);
        assert!(all_calls.contains(&"install_binary".to_string()), "missing install_binary call, got {:?}", all_calls);
        assert!(all_calls.contains(&"main".to_string()), "missing main call, got {:?}", all_calls);

        // No source/. imports
        assert!(sa.imp.is_none() || sa.imp.as_ref().unwrap().is_empty(), "should have no imports");
    }

}
