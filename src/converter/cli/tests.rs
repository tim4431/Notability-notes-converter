use super::{mirror_folder, publish_output, OutputFormat, OwnedDirectory};
use std::{fs, path::PathBuf};

fn fixture() -> (OwnedDirectory, PathBuf, PathBuf) {
    let root =
        OwnedDirectory::beside(&std::env::temp_dir().join("notability-cli-test"), "test").unwrap();
    let input = root.path.join("input");
    let output = root.path.join("output");
    fs::create_dir(&input).unwrap();
    fs::create_dir(&output).unwrap();
    fs::write(output.join("previous.txt"), b"previous conversion").unwrap();
    (root, input, output)
}

#[test]
fn collision_does_not_delete_previous_output() {
    let (root, input, output) = fixture();
    fs::write(input.join("same.note"), b"placeholder").unwrap();
    fs::write(input.join("same.ntb"), b"placeholder").unwrap();
    let error = mirror_folder(&input, &output, OutputFormat::Pdf)
        .unwrap_err()
        .to_string();
    assert!(error.contains("collision"));
    assert_eq!(
        fs::read(output.join("previous.txt")).unwrap(),
        b"previous conversion"
    );
    assert_eq!(fs::read_dir(&root.path).unwrap().count(), 2);
}

#[test]
fn conversion_failure_never_publishes_partial_results() {
    for extension in ["note", "ntb"] {
        let (root, input, output) = fixture();
        fs::write(input.join("a.txt"), b"new content").unwrap();
        fs::write(input.join(format!("z-broken.{extension}")), b"invalid ZIP").unwrap();
        assert!(mirror_folder(&input, &output, OutputFormat::Pdf).is_err());
        assert_eq!(
            fs::read(output.join("previous.txt")).unwrap(),
            b"previous conversion"
        );
        assert!(!output.join("a.txt").exists());
        assert_eq!(
            fs::read_dir(&root.path).unwrap().count(),
            2,
            "staging was not cleaned up"
        );
    }
}

#[test]
fn successful_batch_keeps_a_recoverable_backup() {
    let (root, input, output) = fixture();
    fs::write(input.join("new.txt"), b"new content").unwrap();
    fs::create_dir_all(input.join("nested/empty")).unwrap();
    mirror_folder(&input, &output, OutputFormat::Pdf).unwrap();
    assert_eq!(fs::read(output.join("new.txt")).unwrap(), b"new content");
    assert!(output.join("nested/empty").is_dir());
    assert!(!output.join("previous.txt").exists());
    let backups: Vec<_> = fs::read_dir(&root.path)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".output.backup-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(
        fs::read(backups[0].join("previous/previous.txt")).unwrap(),
        b"previous conversion"
    );
}

#[test]
fn rejects_nested_and_equivalent_input_output_paths_before_writing() {
    let (root, input, output) = fixture();
    fs::write(input.join("keep.txt"), b"keep").unwrap();
    for target in [
        input.clone(),
        input.join("new/../output"),
        root.path.clone(),
    ] {
        assert!(mirror_folder(&input, &target, OutputFormat::Pdf).is_err());
        assert_eq!(fs::read(input.join("keep.txt")).unwrap(), b"keep");
    }
    assert!(!input.join("new").exists());
    assert!(!input.join("output").exists());
    assert_eq!(
        fs::read(output.join("previous.txt")).unwrap(),
        b"previous conversion"
    );
}

#[test]
fn publishing_failure_restores_previous_output() {
    let (root, _input, output) = fixture();
    let mut staging = OwnedDirectory::beside(&output, "staging").unwrap();
    fs::remove_dir(&staging.path).unwrap(); // Force the final rename to fail.
    assert!(publish_output(&mut staging, &output).is_err());
    assert_eq!(
        fs::read(output.join("previous.txt")).unwrap(),
        b"previous conversion"
    );
    assert_eq!(fs::read_dir(&root.path).unwrap().count(), 2);
}

#[cfg(unix)]
#[test]
fn rejects_nested_output_hidden_behind_a_symlink() {
    let (root, input, _output) = fixture();
    let alias = root.path.join("alias");
    std::os::unix::fs::symlink(&input, &alias).unwrap();
    assert!(mirror_folder(&input, &alias.join("output"), OutputFormat::Pdf).is_err());
    assert!(!input.join("output").exists());
}
