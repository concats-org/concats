/// Initialize a repository with a single committed file for tests.
///
/// # Panics
///
/// Panics if the repository, index, or initial commit cannot be created.
#[must_use]
#[allow(clippy::disallowed_methods)]
pub fn init_repo_with_commit(dir: &std::path::Path) -> git2::Repository {
    let repo = git2::Repository::init(dir).unwrap();
    {
        let mut index = repo.index().unwrap();
        std::fs::write(dir.join("init.txt"), "init").unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = git2::Signature::now("test", "test@test").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
    repo
}
