// Emit GIT_SHA + GIT_DESCRIBE at build time. NO build timestamp and NO dirty
// flag: both would break emitted-package byte-reproducibility.
//
// vergen-gitcl 1.x API:
//   GitclBuilder::sha(&mut self, short: bool) -> &mut Self
//   GitclBuilder::describe(&mut self, tags: bool, dirty: bool,
//                          matches: Option<&'static str>) -> &mut Self
//   GitclBuilder::build(&self) -> Result<Gitcl, GitclBuilderError>
//   Emitter::default().add_instructions(&gitcl)? .emit()? -> anyhow::Result<()>
// `build()` and `emit()` return DIFFERENT error types, so the two calls are
// chained with `?` behind a `Box<dyn Error>` return rather than `and_then`
// (which would require a single error type and fail to type-check).
//
// The two env vars are emitted UNCONDITIONALLY first so `env!(...)` in the
// binary always resolves at compile time — even when git is unavailable
// (a source tarball, or a build from a git worktree whose `.git` is a
// gitdir-pointer file the container can't follow). When git succeeds, vergen
// prints the same var names later and the real values win (last write wins).
fn main() {
    println!("cargo:rustc-env=VERGEN_GIT_SHA=unknown");
    println!("cargo:rustc-env=VERGEN_GIT_DESCRIBE=unknown");
    if let Err(e) = emit_git() {
        println!("cargo:warning=git version stamp unavailable: {e}");
    }
    println!("cargo:rerun-if-changed=.git/HEAD");
}

fn emit_git() -> Result<(), Box<dyn std::error::Error>> {
    use vergen_gitcl::{Emitter, GitclBuilder};
    let mut builder = GitclBuilder::default();
    builder.sha(true).describe(true, false, None);
    let gitcl = builder.build()?;
    Emitter::default().add_instructions(&gitcl)?.emit()?;
    Ok(())
}
