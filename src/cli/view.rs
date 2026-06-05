use crate::api::schema::{
    DiffSpec, Method, PaneConvertToViewParams, PaneTarget, Request, ViewKindSpec,
};
use crate::integration::HERDR_PANE_ID_ENV_VAR;

pub(super) fn run_view_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_view_help();
        return Ok(2);
    };

    match subcommand {
        "diff" => run_diff(&args[1..]),
        "help" | "--help" | "-h" => {
            print_view_help();
            Ok(0)
        }
        _ => {
            eprintln!("unknown subcommand: {subcommand}");
            print_view_help();
            Ok(2)
        }
    }
}

fn print_view_help() {
    eprintln!("usage: herdr view <subcommand> [args...]");
    eprintln!();
    eprintln!("subcommands:");
    eprintln!("  diff [--baseline <ref>]  Convert the current pane to a diff view");
}

fn run_diff(args: &[String]) -> std::io::Result<i32> {
    let baseline = match parse_baseline_arg(args) {
        Ok(baseline) => baseline,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };

    let Ok(pane_id) = std::env::var(HERDR_PANE_ID_ENV_VAR) else {
        eprintln!(
            "not running inside a herdr pane (set ${HERDR_PANE_ID_ENV_VAR} to opt in manually)"
        );
        return Ok(2);
    };

    let cwd = std::env::current_dir()?;
    if !is_git_repo(&cwd) {
        eprintln!("not a git repository: {}", cwd.display());
        return Ok(2);
    }

    let request = Request {
        id: "cli:view:diff".into(),
        method: Method::PaneConvertToView(PaneConvertToViewParams {
            pane: PaneTarget { pane_id },
            kind: ViewKindSpec::Diff(DiffSpec {
                baseline,
                scope: None,
            }),
        }),
    };

    super::print_response(&super::send_request(&request)?)
}

fn parse_baseline_arg(args: &[String]) -> Result<Option<String>, String> {
    let mut baseline = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--baseline" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --baseline".into());
                };
                baseline = Some(value.clone());
                index += 2;
            }
            other => {
                return Err(format!("unknown argument: {other}"));
            }
        }
    }
    Ok(baseline)
}

fn is_git_repo(cwd: &std::path::Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--git-dir"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = format!(
            "herdr-cli-view-{}-{}-{}",
            label,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn is_git_repo_returns_false_outside_repo() {
        let root = temp_dir("not-a-repo");
        assert!(!is_git_repo(&root));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn parse_baseline_arg_extracts_value() {
        let args = vec!["--baseline".into(), "develop".into()];
        assert_eq!(
            parse_baseline_arg(&args).unwrap().as_deref(),
            Some("develop")
        );
    }

    #[test]
    fn parse_baseline_arg_rejects_unknown_flag() {
        let args = vec!["--what".into()];
        assert!(parse_baseline_arg(&args).is_err());
    }

    #[test]
    fn parse_baseline_arg_defaults_to_none() {
        let args: Vec<String> = vec![];
        assert!(parse_baseline_arg(&args).unwrap().is_none());
    }
}
