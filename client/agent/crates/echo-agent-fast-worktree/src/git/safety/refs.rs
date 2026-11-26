pub(super) struct RefsUnreadable;

pub(super) fn visit_refs(
    repo: &gix::Repository,
    mut visit: impl FnMut(gix::Reference<'_>),
) -> Result<(), RefsUnreadable> {
    let platform = repo.references().map_err(|error| {
        tracing::warn!(git_dir = %repo.git_dir().display(), %error, "failed to open refs");
        RefsUnreadable
    })?;
    let references = platform.all().map_err(|error| {
        tracing::warn!(git_dir = %repo.git_dir().display(), %error, "failed to iterate refs");
        RefsUnreadable
    })?;
    for reference in references {
        let reference = reference.map_err(|error| {
            tracing::warn!(git_dir = %repo.git_dir().display(), %error, "failed to read a ref");
            RefsUnreadable
        })?;
        visit(reference);
    }
    Ok(())
}
