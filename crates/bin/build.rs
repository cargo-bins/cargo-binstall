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

fn main() {
    let handle = thread::spawn(|| {
        println!("cargo:rerun-if-changed=build.rs");
        println!("cargo:rerun-if-changed=manifest.rc");
        println!("cargo:rerun-if-changed=windows.manifest");

        embed_resource::compile("manifest.rc", embed_resource::NONE);
    });

    let git = Command::new("git").arg("--version").spawn();

    // .git is usually a dir, but it also can be a file containing
    // path to another .git if it is a submodule.
    //
    // If build.rs is run on a git repository, then ../../.git
    // should exists.
    let is_git_repo = Path::new("../../.git").exists();

    let mut builder = vergen::EmitBuilder::builder();
    builder.all_build().all_cargo().all_rustc();

    if is_git_repo && succeeds(git) {
        builder.all_git();
    } else {
        builder.disable_git();
    }

    builder.emit().unwrap();

    handle.join().unwrap();
}
