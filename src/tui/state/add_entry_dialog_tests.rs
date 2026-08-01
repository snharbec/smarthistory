    use super::{AddEntryDialog, AddEntryKind};

    /// Session dialog has 3
    /// fields (Name, Dir,
    /// Exec); Name is
    /// required, Dir is
    /// pre-filled from the
    /// source directory, Exec
    /// is optional.
    #[test]
    fn session_dialog_fields() {
        let d = AddEntryDialog::new(
            AddEntryKind::Session,
            "/home/user/project".to_string(),
            "make test".to_string(),
        );
        assert_eq!(d.kind, AddEntryKind::Session);
        assert_eq!(d.fields.len(), 3);
        assert_eq!(d.fields[0].name, "Name");
        assert!(d.fields[0].required);
        assert_eq!(d.fields[1].name, "Dir");
        assert!(!d.fields[1].required);
        // The Dir field is
        // pre-filled with
        // the source
        // directory.
        assert_eq!(d.fields[1].value, "/home/user/project");
        // The cursor lands
        // at the end of
        // the pre-filled
        // value.
        assert_eq!(d.fields[1].cursor, "/home/user/project".chars().count());
        assert_eq!(d.fields[2].name, "Exec");
        assert!(!d.fields[2].required);
    }

    /// Host dialog has 7
    /// fields (Name, Host,
    /// Hostname, User, Port,
    /// Identity, Exec); Name
    /// and Host are required;
    /// Host is pre-filled with
    /// the directory basename.
    #[test]
    fn host_dialog_fields() {
        let d = AddEntryDialog::new(
            AddEntryKind::Host,
            "/home/user/.config/herdr".to_string(),
            String::new(),
        );
        assert_eq!(d.kind, AddEntryKind::Host);
        assert_eq!(d.fields.len(), 7);
        assert_eq!(d.fields[0].name, "Name");
        assert!(d.fields[0].required);
        assert_eq!(d.fields[1].name, "Host");
        assert!(d.fields[1].required);
        // The Host field is
        // pre-filled with
        // the basename of
        // the source
        // directory.
        assert_eq!(d.fields[1].value, "herdr");
        assert_eq!(d.fields[2].name, "Hostname");
        assert!(!d.fields[2].required);
        assert_eq!(d.fields[3].name, "User");
        assert!(!d.fields[3].required);
        assert_eq!(d.fields[4].name, "Port");
        assert!(!d.fields[4].required);
        assert_eq!(d.fields[5].name, "Identity");
        assert!(!d.fields[5].required);
        assert_eq!(d.fields[6].name, "Exec");
        assert!(!d.fields[6].required);
    }

    /// Host dialog with a
    /// path that has no
    /// basename component
    /// (e.g. just "/") falls
    /// back to the full path
    /// for the Host pre-fill
    /// (rather than crashing
    /// on the missing
    /// basename).
    #[test]
    fn host_dialog_root_path_falls_back_to_full_path() {
        let d = AddEntryDialog::new(AddEntryKind::Host, "/".to_string(), String::new());
        // `/` has no
        // basename; the
        // fallback is the
        // full path.
        assert_eq!(d.fields[1].value, "/");
    }

    /// focus_next wraps from
    /// the last field back to
    /// the first.
    #[test]
    fn focus_next_wraps() {
        let mut d = AddEntryDialog::new(AddEntryKind::Session, "/tmp".to_string(), String::new());
        assert_eq!(d.focused, 0);
        d.focus_next();
        assert_eq!(d.focused, 1);
        d.focus_next();
        assert_eq!(d.focused, 2);
        d.focus_next();
        // Wrap to 0.
        assert_eq!(d.focused, 0);
    }

    /// focus_prev wraps from
    /// the first field back to
    /// the last.
    #[test]
    fn focus_prev_wraps() {
        let mut d = AddEntryDialog::new(AddEntryKind::Session, "/tmp".to_string(), String::new());
        assert_eq!(d.focused, 0);
        d.focus_prev();
        // Wrap to 2 (the
        // last field).
        assert_eq!(d.focused, 2);
    }

    /// The source directory
    /// and command are kept
    /// verbatim in the
    /// dialog's
    /// `source_directory` /
    /// `source_command`
    /// fields, which the
    /// renderer shows as a
    /// "from: <cmd> in <dir>"
    /// hint.
    #[test]
    fn source_fields_preserved() {
        let d = AddEntryDialog::new(
            AddEntryKind::Host,
            "/home/user/proj".to_string(),
            "cargo build --release".to_string(),
        );
        assert_eq!(d.source_directory, "/home/user/proj");
        assert_eq!(d.source_command, "cargo build --release");
    }
