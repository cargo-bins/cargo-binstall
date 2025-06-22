use std::{
    io,
    path::Path,
    process::{Child, Command},
    thread,
};

fn succeeds(res: io::Result<Child>) -> bool {
    res.and_then(|mut child| child.wait())
        .map(|status| status.success())
        .unwrap_or(false)
}

fn emit_vergen_info() {
    let git = Command::new("git").arg("--version").spawn();

    // .git is usually a dir, but it also can be a file containing
    // path to another .git if it is a submodule.
    //
    // If build.rs is run on a git repository, then ../../.git
    // should exists.
    let is_git_repo = Path::new("../../.git").exists();

    let build = vergen_gitcl::BuildBuilder::all_build().unwrap();
    let cargo = vergen_gitcl::CargoBuilder::all_cargo().dependencies(false).unwrap();
    let rustc = vergen_gitcl::RustcBuilder::all_rustc().unwrap();

    let mut emitter = vergen_gitcl::Emitter::default();
    emitter.fail_on_error()
        .add_instructions(&build)
        .unwrap()
        .add_instructions(&cargo)
        .unwrap()
        .add_instructions(&rustc)
        .unwrap();
    
    let gitcl = (is_git_repo && succeeds(git)).then(|| vergen_gitcl::GitclBuilder::all_git().unwrap());
    if let Some(gitcl) = &gitcl {
        emitter.add_instructions(&gitcl).unwrap();
    }
    emitter.emit().unwrap();
}

const RERUN_INSTRUCTIONS: &str = "cargo:rerun-if-changed=build.rs
cargo:rerun-if-changed=manifest.rc
cargo:rerun-if-changed=windows.manifest";

fn main() {
    thread::scope(|s| {
        let handle = s.spawn(|| {
            println!("{RERUN_INSTRUCTIONS}");

            embed_resource::compile("manifest.rc", embed_resource::NONE)
                .manifest_required()
                .unwrap();
        });

        emit_vergen_info();

        handle.join().unwrap();
    });
}
