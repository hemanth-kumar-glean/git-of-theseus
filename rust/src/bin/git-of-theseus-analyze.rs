use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let code = run("analyze");
    std::process::exit(code);
}

fn run(subcommand: &str) -> i32 {
    let mut args: Vec<String> = env::args().skip(1).collect();
    args.insert(0, subcommand.to_string());
    exec_main_bin(args).unwrap_or(1)
}

fn exec_main_bin(args: Vec<String>) -> Result<i32, ()> {
    let exe = env::current_exe().map_err(|_| ())?;
    let dir = exe.parent().ok_or(())?;
    let candidate = PathBuf::from(dir).join("git-of-theseus-rs");
    let status = Command::new(candidate).args(args).status().map_err(|_| ())?;
    Ok(status.code().unwrap_or(1))
}
