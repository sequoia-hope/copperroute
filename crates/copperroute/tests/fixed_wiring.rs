#![forbid(unsafe_code)]

use std::path::PathBuf;

use copperroute::ops::fixed_wiring::{apply_sidecar, supersede, system_fixed_wiring, write_sidecar};
use copperroute::ops::load::{BoardSource, LoadRequest, Loaded, load, retire_fixed};

fn preamp() -> Loaded {
    let dsn = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/fixtures/Issue283-UnconnectedTracesUnderPads/Natural_Tone_Preamp.dsn");
    load(&LoadRequest::for_board(BoardSource::Path(dsn))).expect("the fixture loads")
}

#[test]
fn retiring_a_net_unfixes_only_its_wiring_and_the_session_supersedes_it() {
    let mut loaded = preamp();
    let original = system_fixed_wiring(&loaded.board);
    assert!(!original.is_empty());
    let total = original.len();

    let retired = retire_fixed(&mut loaded.board, &["GND".to_string()]).unwrap();
    assert!(retired > 0);
    let kept = system_fixed_wiring(&loaded.board);
    assert_eq!(kept.len(), total - retired);

    let superseded = supersede(&mut loaded.board, &original, &loaded.transform);
    assert_eq!(superseded.len(), retired);
    assert!(superseded.iter().all(|s| s.nets == ["GND"]), "{superseded:?}");
}

#[test]
fn a_sidecar_removes_exactly_what_it_supersedes_from_a_fresh_load() {
    let mut loaded = preamp();
    let original = system_fixed_wiring(&loaded.board);
    retire_fixed(&mut loaded.board, &["all".to_string()]).unwrap();
    let superseded = supersede(&mut loaded.board, &original, &loaded.transform);
    assert_eq!(superseded.len(), original.len());

    let dir = std::env::temp_dir().join("fr-fixed-wiring");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let session = dir.join("out.ses");
    write_sidecar(&session, &superseded).unwrap();

    let mut fresh = preamp();
    assert_eq!(apply_sidecar(&session, &mut fresh.board), Ok(original.len()));
    assert!(system_fixed_wiring(&fresh.board).is_empty());

    write_sidecar(&session, &[]).unwrap();
    assert!(!dir.join("out.superseded.json").exists());
}

#[test]
fn retiring_an_unknown_net_is_an_error() {
    let mut loaded = preamp();
    assert!(retire_fixed(&mut loaded.board, &["NO_SUCH_NET".to_string()]).is_err());
}
