use std::path::PathBuf;

use festerm_test_support::{assert_fixture, discover_fixtures, load_fixture};

#[test]
fn repository_fixtures_are_discovered_and_pass() {
    let fixture_directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let fixtures = discover_fixtures(&fixture_directory).expect("fixture discovery should succeed");

    assert!(
        !fixtures.is_empty(),
        "the repository must contain golden fixtures"
    );
    for path in fixtures {
        let fixture = load_fixture(&path).expect("fixture should parse");
        assert_fixture(&fixture);
    }
}
