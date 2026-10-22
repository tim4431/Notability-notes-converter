mod archive;
mod cli;
mod geometry;
mod model;
mod note;
mod note_writer;
mod ntb;
mod render;
mod util;

pub fn run_cli(args: Vec<String>) -> crate::Result<()> {
    cli::run(args)
}
