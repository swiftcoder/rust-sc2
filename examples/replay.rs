use clap::Parser;
use rust_sc2::client::run_replay;
use rust_sc2::prelude::*;

#[derive(Parser)]
#[clap(version, author)]
struct Args {
    #[clap(short = 'r', long = "replay")]
    replay: String,
}

fn main() -> SC2Result<()> {
    let args = Args::parse();

    run_replay(args.replay)?;

    Ok(())
}
