use super::*;

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "smarthistory-sources-{label}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    ensure_sources_table(&conn).unwrap();
    conn
}

fn record(path: &str, is_git: bool, first: i64, last: i64, visits: i64) -> SourceRecord {
    SourceRecord {
        path: path.to_string(),
        is_git,
        first_seen: first,
        last_visited: last,
        visit_count: visits,
    }
}

#[test]
fn classifies_git_directory() {
    let tmp = TempDir::new("git");
    std::fs::create_dir(tmp.0.join(".git")).unwrap();
    let found = classify_source(&tmp.0).unwrap();
    assert!(found.is_git);
    assert_eq!(found.path, tmp.0.to_string_lossy());
}

#[test]
fn classifies_build_xml_as_non_git() {
    let tmp = TempDir::new("ant");
    std::fs::write(tmp.0.join("build.xml"), "<project/>").unwrap();
    let found = classify_source(&tmp.0).unwrap();
    assert!(!found.is_git);
}

#[test]
fn git_wins_over_build_xml() {
    let tmp = TempDir::new("both");
    std::fs::create_dir(tmp.0.join(".git")).unwrap();
    std::fs::write(tmp.0.join("build.xml"), "<project/>").unwrap();
    assert!(classify_source(&tmp.0).unwrap().is_git);
}

#[test]
fn plain_directory_is_not_a_source() {
    let tmp = TempDir::new("plain");
    assert_eq!(classify_source(&tmp.0), None);
}

#[test]
fn linked_worktree_resolves_to_main_repo() {
    let tmp = TempDir::new("linked");
    let main = tmp.0.join("repo");
    let admin = main.join(".git/worktrees/feature");
    let linked = tmp.0.join("feature");
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::create_dir(&linked).unwrap();
    std::fs::write(admin.join("commondir"), "../..\n").unwrap();
    std::fs::write(linked.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();

    let found = classify_source(&linked).unwrap();
    assert!(found.is_git);
    assert_eq!(found.path, main.to_string_lossy());
}

#[test]
fn submodule_stays_its_own_source() {
    let tmp = TempDir::new("submodule");
    let module_git = tmp.0.join(".git/modules/lib");
    let module = tmp.0.join("lib");
    std::fs::create_dir_all(&module_git).unwrap();
    std::fs::create_dir(&module).unwrap();
    std::fs::write(module.join(".git"), "gitdir: ../.git/modules/lib\n").unwrap();

    let found = classify_source(&module).unwrap();
    assert!(found.is_git);
    assert_eq!(found.path, module.to_string_lossy());
}

#[test]
fn upsert_counts_visits_and_keeps_first_seen() {
    let conn = test_db();
    upsert_source(&conn, "/src/a", true, 100).unwrap();
    upsert_source(&conn, "/src/a", true, 200).unwrap();
    assert_eq!(list_sources(&conn).unwrap(), vec![record("/src/a", true, 100, 200, 2)]);
}

#[test]
fn list_is_most_recent_first() {
    let conn = test_db();
    upsert_source(&conn, "/src/old", false, 100).unwrap();
    upsert_source(&conn, "/src/new", true, 200).unwrap();
    let paths: Vec<_> = list_sources(&conn).unwrap().into_iter().map(|r| r.path).collect();
    assert_eq!(paths, ["/src/new", "/src/old"]);
}

#[test]
fn delete_reports_whether_row_existed() {
    let conn = test_db();
    upsert_source(&conn, "/src/a", true, 100).unwrap();
    assert!(delete_source(&conn, "/src/a").unwrap());
    assert!(!delete_source(&conn, "/src/a").unwrap());
}

#[test]
fn import_is_idempotent_and_merges() {
    let conn = test_db();
    upsert_source(&conn, "/src/a", false, 150).unwrap();
    let rows = [record("/src/a", true, 100, 120, 5), record("/src/b", false, 10, 20, 1)];
    import_sources(&conn, &rows).unwrap();
    import_sources(&conn, &rows).unwrap();
    assert_eq!(
        list_sources(&conn).unwrap(),
        vec![record("/src/a", true, 100, 150, 5), record("/src/b", false, 10, 20, 1)]
    );
}

fn session(id: &str, name: &str, roots: &[&str]) -> crate::multiplexer::MuxSession {
    crate::multiplexer::MuxSession {
        id: id.to_string(),
        name: name.to_string(),
        roots: roots.iter().map(|r| r.to_string()).collect(),
    }
}

#[test]
fn session_target_creates_unused_name() {
    assert_eq!(
        resolve_session_target("api", "/src/api", &[session("$1", "web", &["/src/web"])]),
        SessionTarget::Create("api".to_string())
    );
}

#[test]
fn session_target_reuses_same_directory() {
    assert_eq!(
        resolve_session_target("api", "/src/api", &[session("$1", "api", &["/src/api"])]),
        SessionTarget::Reuse("$1".to_string())
    );
}

#[test]
fn session_target_suffixes_on_clash() {
    assert_eq!(
        resolve_session_target("api", "/b/api", &[session("$1", "api", &["/a/api"])]),
        SessionTarget::Create("api-2".to_string())
    );
}

#[test]
fn session_target_reuses_suffixed_session() {
    let sessions = [session("$1", "api", &["/a/api"]), session("$2", "api-2", &["/b/api"])];
    assert_eq!(
        resolve_session_target("api", "/b/api", &sessions),
        SessionTarget::Reuse("$2".to_string())
    );
}

#[test]
fn session_target_checks_every_duplicate_label() {
    let sessions = [session("w1", "api", &["/a/api"]), session("w2", "api", &["/b/api"])];
    assert_eq!(
        resolve_session_target("api", "/b/api", &sessions),
        SessionTarget::Reuse("w2".to_string())
    );
}

#[test]
fn session_names_are_sanitized() {
    assert_eq!(sanitize_session_name("v1.2:rc"), "v1-2-rc");
    assert_eq!(sanitize_session_name("=main"), "-main");
    assert_eq!(sanitize_session_name(""), "source");
}

#[test]
fn rewrite_merges_colliding_paths() {
    let conn = test_db();
    import_sources(
        &conn,
        &[record("/old/a", false, 50, 300, 3), record("/new/a", true, 100, 200, 7)],
    )
    .unwrap();
    let rewritten = rewrite_source_paths(&conn, |p| p.replace("/old/", "/new/")).unwrap();
    assert_eq!(rewritten, 1);
    assert_eq!(list_sources(&conn).unwrap(), vec![record("/new/a", true, 50, 300, 7)]);
}
