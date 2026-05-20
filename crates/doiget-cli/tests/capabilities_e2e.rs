//! End-to-end tests for `doiget capabilities` (#214).
//!
//! Exercises the real `doiget` binary so the clap walk uses the
//! production `Cli` tree (not the shadow `test_cli` in the lib-level
//! tests). The contract surface is:
//!
//! 1. Single valid JSON value on stdout.
//! 2. Top-level keys: `version`, `features`, `modes`, `global_flags`,
//!    `subcommands`, `env_vars`, `mcp_tools`, `docs`.
//! 3. Every documented CLI subcommand (`fetch`, `batch`, `info`,
//!    `list-recent`, `search`, `bib`, `csl`, `audit-log`,
//!    `provenance`, `config`, `serve`, `capabilities`) is in
//!    `subcommands[]`.
//! 4. Honors ADR-0017: `--mode quiet` produces empty stdout.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// Build a `doiget capabilities` command with env scaffolding that
/// avoids touching the developer's real `~/.config/doiget/`.
fn doiget(dir: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("locate doiget binary");
    let p = dir.path().to_str().expect("tempdir path utf-8");
    cmd.env("HOME", p)
        .env("USERPROFILE", p)
        .env("APPDATA", p)
        .env("XDG_CONFIG_HOME", p)
        // capabilities is a product-output command (JSON regardless of
        // mode). Force `human` so the non-TTY default-to-quiet rule
        // (#203) doesn't suppress stdout in this assert_cmd context.
        .env("DOIGET_MODE", "human");
    cmd
}

fn parse_capabilities(stdout: Vec<u8>) -> Value {
    let s = String::from_utf8(stdout).expect("stdout utf-8");
    serde_json::from_str(&s).expect("capabilities stdout parses as JSON")
}

#[test]
fn capabilities_emits_valid_json_with_all_top_level_keys() {
    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .arg("capabilities")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_capabilities(out);
    for key in [
        "version",
        "features",
        "modes",
        "global_flags",
        "subcommands",
        "env_vars",
        "mcp_tools",
        "docs",
    ] {
        assert!(
            v.get(key).is_some(),
            "top-level key `{key}` missing from capabilities JSON"
        );
    }
}

#[test]
fn capabilities_inventory_includes_every_subcommand() {
    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .arg("capabilities")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_capabilities(out);
    let subs = v["subcommands"]
        .as_array()
        .expect("subcommands is an array");
    let names: Vec<&str> = subs
        .iter()
        .map(|s| s["name"].as_str().expect("subcommand name is a string"))
        .collect();
    for expected in [
        "fetch",
        "batch",
        "info",
        "list-recent",
        "search",
        "bib",
        "csl",
        "audit-log",
        "provenance",
        "config",
        "serve",
        "capabilities",
    ] {
        assert!(
            names.contains(&expected),
            "subcommand `{expected}` missing from capabilities inventory; got {names:?}"
        );
    }

    // `graph` is `#[cfg(feature = "citation")]` in main.rs. Under
    // that feature it MUST appear; without it, it MUST NOT. A silent
    // drop of `graph` in a citation build would otherwise pass this
    // test (#215 N2).
    let has_graph = names.contains(&"graph");
    if cfg!(feature = "citation") {
        assert!(
            has_graph,
            "`citation` feature was compiled in but `graph` is missing \
             from capabilities inventory; got {names:?}"
        );
    } else {
        assert!(
            !has_graph,
            "`graph` appeared in capabilities inventory without the \
             `citation` feature; got {names:?}"
        );
    }
}

#[test]
fn capabilities_fetch_subcommand_carries_artifact_status() {
    // #215 N1: the `#[serde(tag = "status")]` wire shape is part of
    // the public contract. Pin a representative subcommand's
    // `json_mode` so an accidental revert (removing the tag, renaming
    // the discriminant) fails at the e2e layer.
    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .arg("capabilities")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_capabilities(out);
    let fetch = v["subcommands"]
        .as_array()
        .expect("subcommands array")
        .iter()
        .find(|s| s["name"] == "fetch")
        .expect("fetch subcommand present");
    assert_eq!(
        fetch["json_mode"]["status"], "artifact",
        "fetch.json_mode MUST carry a status discriminant, got {fetch:?}"
    );
}

#[test]
fn capabilities_quiet_mode_produces_no_stdout() {
    // ADR-0017 / #203: Quiet suppresses stdout even for product-output
    // commands. The exit is still 0 (capabilities never errors on a
    // missing env / store; pure introspection).
    let dir = TempDir::new().expect("tempdir");
    Command::cargo_bin("doiget")
        .expect("locate doiget binary")
        .env("HOME", dir.path())
        .env("USERPROFILE", dir.path())
        .env("APPDATA", dir.path())
        .env("XDG_CONFIG_HOME", dir.path())
        .args(["--quiet", "capabilities"])
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}

#[test]
fn capabilities_modes_field_matches_output_mode_enum() {
    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .arg("capabilities")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_capabilities(out);
    let modes = v["modes"]
        .as_array()
        .expect("modes is an array")
        .iter()
        .map(|m| m.as_str().expect("mode string").to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        modes,
        vec!["human", "json", "quiet", "mcp"],
        "modes field MUST mirror the OutputMode enum (ADR-0017)"
    );
}

#[test]
fn capabilities_env_vars_list_is_non_empty_and_doiget_prefixed() {
    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .arg("capabilities")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_capabilities(out);
    let env_vars = v["env_vars"].as_array().expect("env_vars is an array");
    assert!(!env_vars.is_empty(), "env_vars MUST be non-empty");
    for ev in env_vars {
        let name = ev["name"].as_str().expect("env var has a name");
        assert!(
            name.starts_with("DOIGET_"),
            "env var name MUST use DOIGET_ prefix, got `{name}`"
        );
    }
}

#[test]
fn capabilities_mcp_tools_list_is_non_empty_and_doiget_prefixed() {
    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .arg("capabilities")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = parse_capabilities(out);
    let tools = v["mcp_tools"].as_array().expect("mcp_tools is an array");
    assert!(!tools.is_empty(), "mcp_tools MUST be non-empty");
    for t in tools {
        let name = t["name"].as_str().expect("mcp tool has a name");
        assert!(
            name.starts_with("doiget_"),
            "MCP tool name MUST use doiget_ prefix, got `{name}`"
        );
    }
}
