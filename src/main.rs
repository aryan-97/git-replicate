mod cli;
mod config;
mod diag;
mod git;
mod hook;
mod out;
mod push;
mod repo;
mod setup;
mod state;
mod util;

use clap::Parser;
use clap::error::ErrorKind;
use cli::{Cli, Cmd, HookCmd, MirrorCmd};
use out::{CliError, Out};
use std::path::PathBuf;
use std::process::ExitCode;

pub struct Ctx {
    pub out: Out,
    pub cwd: PathBuf,
}

fn truthy(v: Option<std::ffi::OsString>) -> bool {
    v.map(|v| {
        !matches!(
            v.to_string_lossy().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
    .unwrap_or(false)
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                let _ = e.print();
                return ExitCode::SUCCESS;
            }
            let uiless = std::env::args().any(|a| a == "--uiless")
                || truthy(std::env::var_os("GIT_REPLICATE_UILESS"));
            if uiless {
                let first = e.to_string();
                let msg = first
                    .lines()
                    .next()
                    .unwrap_or("usage error")
                    .trim_start_matches("error: ")
                    .to_string();
                Out::new(true, true, false, "usage")
                    .error(&CliError::usage(msg).hint("Run git-replicate --help"));
            } else {
                let _ = e.print();
            }
            return ExitCode::from(2);
        }
    };
    let op = match &cli.cmd {
        Cmd::Setup(_) => "setup",
        Cmd::Mirror(MirrorCmd::Add { .. }) => "mirror add",
        Cmd::Mirror(MirrorCmd::List) => "mirror list",
        Cmd::Mirror(MirrorCmd::Remove { .. }) => "mirror remove",
        Cmd::Push(_) => "push",
        Cmd::Retry(_) => "retry",
        Cmd::Status(_) => "status",
        Cmd::Doctor(_) => "doctor",
        Cmd::Hook(HookCmd::Install { .. }) => "hook install",
        Cmd::Hook(HookCmd::Uninstall { .. }) => "hook uninstall",
        Cmd::Hook(HookCmd::Status) => "hook status",
        Cmd::Hook(HookCmd::Run { .. }) => "hook run",
    };
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            Out::new(cli.uiless, true, false, op).error(&CliError::usage(format!(
                "cannot determine current directory: {e}"
            )));
            return ExitCode::from(2);
        }
    };
    let ctx = Ctx {
        out: Out::new(cli.uiless, cli.no_color, cli.verbose, op),
        cwd,
    };
    let res = match cli.cmd {
        Cmd::Setup(a) => setup::setup(&ctx, a),
        Cmd::Mirror(MirrorCmd::Add { name, url, remote }) => {
            setup::mirror_add(&ctx, &name, url, remote)
        }
        Cmd::Mirror(MirrorCmd::List) => setup::mirror_list(&ctx),
        Cmd::Mirror(MirrorCmd::Remove {
            name,
            remove_managed_remote,
            yes,
        }) => setup::mirror_remove(&ctx, &name, remove_managed_remote, yes),
        Cmd::Push(a) => push::push(&ctx, a.branch, a.dry_run),
        Cmd::Retry(a) => push::retry(&ctx, a.branch),
        Cmd::Status(a) => diag::status(&ctx, a.remote),
        Cmd::Doctor(a) => diag::doctor(&ctx, a.check_remote),
        Cmd::Hook(HookCmd::Install { strict, .. }) => hook::install(&ctx, strict),
        Cmd::Hook(HookCmd::Uninstall { .. }) => hook::uninstall(&ctx),
        Cmd::Hook(HookCmd::Status) => hook::status(&ctx),
        Cmd::Hook(HookCmd::Run { strict, args, .. }) => hook::run(&ctx, strict, args),
    };
    let code = match res {
        Ok(c) => c,
        Err(e) => {
            ctx.out.error(&e);
            e.code
        }
    };
    ExitCode::from(code as u8)
}
