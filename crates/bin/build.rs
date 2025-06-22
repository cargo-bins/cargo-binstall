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
    use vergen_gitcl::*;

    let git = Command::new("git").arg("--version").spawn();

    // .git is usually a dir, but it also can be a file containing
    // path to another .git if it is a submodule.
    //
    // If build.rs is run on a git repository, then ../../.git
    // should exists.
    let is_git_repo = Path::new("../../.git").exists();

    Emitter::default()
        .fail_on_error()
        .add_instructions(&BuildBuilder::default().build_date(true).build().unwrap())
        .unwrap()
        .add_instructions(&CargoBuilder::default().features(true).build().unwrap())
        .unwrap()
        .add_instructions(
            &RustcBuilder::default()
                .semver(true)
                .commit_hash(true)
                .llvm_version(true)
                .build()
                .unwrap(),
        )
        .unwrap()
        .add_instructions(&{
            let mut gitcl_builder = GitclBuilder::default();
            if is_git_repo && succeeds(git) {
                // sha(false) means enable the default sha output but not the short output
                gitcl_builder.commit_date(true).sha(false);
            }
            gitcl_builder.build().unwrap()
        })
        .unwrap()
        .emit()
        .unwrap();
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
