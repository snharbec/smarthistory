    use super::*;

    #[test]
    fn glob_to_regex_star_rs() {
        assert_eq!(glob_to_ag_regex("*.rs"), r".*\.rs$");
    }

    #[test]
    fn glob_to_regex_bla_star_txt() {
        assert_eq!(glob_to_ag_regex("bla*.txt"), r"bla.*\.txt$");
    }

    #[test]
    fn glob_to_regex_all_files() {
        assert_eq!(glob_to_ag_regex("*"), r".*$");
    }

    #[test]
    fn glob_to_regex_escapes_dot() {
        assert_eq!(glob_to_ag_regex("*.min.js"), r".*\.min\.js$");
    }

    #[test]
    fn glob_to_regex_escapes_plus() {
        assert_eq!(glob_to_ag_regex("file*.c++"), r"file.*\.c\+\+$");
    }

    #[test]
    fn glob_to_regex_no_star_is_literal() {
        assert_eq!(glob_to_ag_regex("Makefile"), r"Makefile$");
    }
