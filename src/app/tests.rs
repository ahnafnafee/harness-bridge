use super::*;

fn fixture(root: &Path) -> (Homes, SessionRef) {
    let claude = root.join("claude");
    let project = claude.join("projects/demo");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("source-one.jsonl"),json!({"type":"user","uuid":"u","parentUuid":null,"sessionId":"source-one","timestamp":"2026-10-21T10:00:00Z","cwd":"C:/work","message":{"role":"user","content":"captured prompt"}}).to_string()+"\n").unwrap();
    let homes = Homes {
        claude: Some(claude),
        codex: Some(root.join("codex")),
        qoder: Some(root.join("qoder")),
        ..Default::default()
    };
    let reference = discover_provider("claude", &homes).unwrap().remove(0);
    (homes, reference)
}

#[test]
fn confirmation_uses_the_previewed_source_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let (homes, reference) = fixture(root.path());
    let plan = TransferPlan::prepare(TransferRequest::new(&reference, "qoder"), &homes).unwrap();
    assert!(!homes.qoder.as_ref().unwrap().exists());
    assert_eq!(plan.preview.extra["dry_run"], true);
    std::fs::write(&reference.locator, "source changed after preview\n").unwrap();
    let outcome = plan.execute().unwrap();
    let written = std::fs::read_to_string(outcome.location).unwrap();
    assert!(written.contains("captured prompt"));
    assert!(!written.contains("source changed after preview"));
    assert_eq!(
        std::fs::read_to_string(reference.locator).unwrap(),
        "source changed after preview\n"
    );
}

#[test]
fn changed_destination_requires_a_new_preview() {
    let root = tempfile::tempdir().unwrap();
    let (homes, reference) = fixture(root.path());
    let plan = TransferPlan::prepare(TransferRequest::new(&reference, "qoder"), &homes).unwrap();
    let location = PathBuf::from(&plan.preview.location);
    std::fs::create_dir_all(location.parent().unwrap()).unwrap();
    std::fs::write(&location, "later destination turns").unwrap();
    assert!(plan
        .execute()
        .unwrap_err()
        .to_string()
        .contains("destination changed"));
    assert_eq!(
        std::fs::read_to_string(location).unwrap(),
        "later destination turns"
    );
}

#[test]
fn source_only_destinations_and_ambiguous_queries_fail_before_writes() {
    let root = tempfile::tempdir().unwrap();
    let (homes, reference) = fixture(root.path());
    assert!(TransferPlan::prepare(TransferRequest::new(&reference, "qoder-ide"), &homes).is_err());
    assert!(!homes.qoder.as_ref().unwrap().exists());
    let mut other = reference.clone();
    other.id = "source-two".into();
    other.title = reference.title.clone();
    let references = vec![reference, other];
    assert!(resolve(&references, "").is_err());
    assert!(resolve(&references, "source-").is_err());
    assert_eq!(resolve(&references, "source-one").unwrap().id, "source-one");
    assert!(matches(&references[0], "claude work"));
    assert!(!matches(&references[0], "missing"));
}

#[test]
fn portable_import_uses_the_same_preview_pipeline() {
    let root = tempfile::tempdir().unwrap();
    let (homes, reference) = fixture(root.path());
    let file = root.path().join("session.hbridge.json");
    export(&reference, &homes, &file, false, false).unwrap();
    let mut request = TransferRequest::new(&reference, "qoder");
    request.from.clear();
    request.id.clear();
    request.file = Some(file);
    let plan = TransferPlan::prepare(request, &homes).unwrap();
    assert!(!Path::new(&plan.preview.location).exists());
    assert!(plan.execute().is_ok());
}
